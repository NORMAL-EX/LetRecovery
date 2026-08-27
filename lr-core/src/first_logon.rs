//! Fixed first-logon finalizer shared by Direct and PE installs.
//!
//! Windows can suppress SetupComplete.cmd on OEM-key client editions, so Wi-Fi migration and
//! cleanup run from the built-in unattend FirstLogonCommands path instead. Security UI and curated
//! AppX cleanup are repeated here to catch packages registered during OOBE. Child console tools are
//! hidden and checked. App-removal diagnostics do not strand the installation, while the private
//! staging directory is removed by the parent `cmd.exe` only after this PowerShell process exits,
//! so the currently executing script cannot make a self-deletion look successful while leaving
//! the directory behind.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine as _;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

pub const SCRIPT_FILE_NAME: &str = "first-logon-finalize.ps1";
pub const LAUNCHER_FILE_NAME: &str = "LetRecovery-first-logon.cmd";
pub const ACCOUNT_HELPER_FILE_NAME: &str = "LetRecovery-account-helper.exe";
pub const ACCOUNT_HELPER_RUNTIME_FILE_NAME: &str = "vcruntime140.dll";
pub const PERSONAL_RESTORE_PENDING_FILE_NAME: &str = "personal-restore.pending";
pub const PERSONAL_RESTORE_RECEIPT_FILE_NAME: &str = "personal-restore.completed";
pub const PERSONAL_RESTORE_LOCK_FILE_NAME: &str = "personal-restore.lock";
pub const PERSONAL_RESTORE_SHELL_GATE_FILE_NAME: &str = "personal-restore-shell-gate.state";
pub const PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME: &str = "personal-restore-shell-gate.released";
pub const PERSONAL_RESTORE_SHELL_VERIFIED_FILE_NAME: &str =
    "personal-restore-shell-verified.receipt";
pub const PERSONAL_RESTORE_FAILURE_FILE_NAME: &str = "personal-restore.failed";
pub const PERSONAL_RESTORE_RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
pub const PERSONAL_RESTORE_RUN_VALUE: &str = "LetRecoveryPersonalRestore";
pub const BUILTIN_TRANSITION_MARKER_FILE_NAME: &str = "builtin-administrator-transition.pending";
pub const BUILTIN_TRANSITION_SECRET_STAGING_FILE_NAME: &str =
    "builtin-administrator-secret.pending";
pub const BUILTIN_TRANSITION_RUN_VALUE: &str = "LetRecoveryBuiltinAdministratorTransition";
pub const PERSONAL_RESTORE_RUN_ONCE_VALUE: &str = "LetRecoveryPersonalRestoreGate";
pub const PRIVATE_WIFI_PROFILE_MAX_BYTES: u64 = 1024 * 1024;

const WINLOGON_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon";
const MACHINE_RUN_ONCE_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\RunOnce";
const BUILTIN_TRANSITION_MAGIC: &str = "LRBAT1";
const BUILTIN_TRANSITION_MAX_BYTES: u64 = 512;
const PERSONAL_RESTORE_SHELL_GATE_MAGIC: &str = "LRPSG1";
const PERSONAL_RESTORE_SHELL_GATE_MAX_BYTES: u64 = 16 * 1024;

const LAUNCHER: &str = r#"@echo off
setlocal EnableExtensions EnableDelayedExpansion
set "lr_script=%SystemDrive%\LetRecovery_Scripts\first-logon-finalize.ps1"
if /i "%~1"=="cleanup" goto :cleanup_after_restore
set "lr_log_dir=%ProgramData%\LetRecovery\Logs"
if not exist "%lr_log_dir%" md "%lr_log_dir%"
if not exist "%lr_log_dir%" (
  echo LETRECOVERY_FIRST_LOGON_LOG_DIRECTORY_FAILURE 1>&2
  exit /b 3
)
set "lr_log=%lr_log_dir%\FirstLogon-finalize.log"
echo First-logon launcher: started 2>nul >>"%lr_log%"
if not exist "%lr_script%" (
  echo First-logon launcher: script missing 2>nul >>"%lr_log%"
  exit /b 3
)
if /i "%~1"=="restore" (
  "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoP -NonI -WindowStyle Hidden -ExecutionPolicy Bypass -File "%lr_script%" -PersonalRestoreAtShell
) else (
  "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoP -NonI -WindowStyle Hidden -ExecutionPolicy Bypass -File "%lr_script%"
)
set "lr_ec=!errorlevel!"
if not "!lr_ec!"=="0" (
  echo First-logon staging cleanup: preserved after failure exit=!lr_ec! 2>nul >>"%lr_log%"
  exit /b !lr_ec!
)
if /i "%~1"=="restore" goto :cleanup_after_restore
if exist "%SystemDrive%\LetRecovery_Scripts\builtin-administrator-transition.pending" (
  echo Built-in Administrator transition: staging preserved for the final account logon 2>nul >>"%lr_log%"
  exit /b 0
)
if exist "%SystemDrive%\LetRecovery_Scripts\personal-restore.pending" (
  echo Personal files restore: pre-Explorer gate retained; staging cleanup deferred 2>nul >>"%lr_log%"
  exit /b 0
)
if exist "%SystemDrive%\LetRecovery_Scripts\personal-restore-shell-gate.state" (
  echo Personal files restore: staging retained until the verified Explorer Shell takes over 2>nul >>"%lr_log%"
  exit /b 0
)
if exist "%SystemDrive%\LetRecovery_Scripts" rd /s /q "%SystemDrive%\LetRecovery_Scripts"
if exist "%SystemDrive%\LetRecovery_Scripts" (
  echo LETRECOVERY_FIRST_LOGON_CLEANUP_FAILURE 1>&2
  exit /b 3
)
echo First-logon staging cleanup: completed 2>nul >>"%lr_log%"
(goto) 2>nul & del /f /q "%~f0"
exit /b 0

:cleanup_after_restore
set /a lr_cleanup_attempt=0
:retry_cleanup_after_restore
set /a lr_cleanup_attempt+=1
if exist "%SystemDrive%\LetRecovery_Scripts" rd /s /q "%SystemDrive%\LetRecovery_Scripts" >nul 2>&1
if exist "%SystemDrive%\LetRecovery_Scripts" (
  if !lr_cleanup_attempt! GEQ 120 (
    echo LETRECOVERY_FIRST_LOGON_CLEANUP_FAILURE 1>&2
    exit /b 3
  )
  "%SystemRoot%\System32\WindowsPowerShell\v1.0\powershell.exe" -NoP -NonI -WindowStyle Hidden -ExecutionPolicy Bypass -Command "Start-Sleep -Milliseconds 500"
  if not "!errorlevel!"=="0" (
    exit /b 3
  )
  goto :retry_cleanup_after_restore
)
(goto) 2>nul & del /f /q "%~f0" >nul 2>&1
"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrivateWifiProfileBinding {
    pub length_bytes: u64,
    pub sha256: String,
}

impl PrivateWifiProfileBinding {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let length_bytes = u64::try_from(bytes.len()).context("Wi-Fi profile length overflow")?;
        if length_bytes == 0 || length_bytes > PRIVATE_WIFI_PROFILE_MAX_BYTES {
            anyhow::bail!(
                "Wi-Fi profile size {length_bytes} is outside 1..={PRIVATE_WIFI_PROFILE_MAX_BYTES} bytes"
            );
        }
        Ok(Self {
            length_bytes,
            sha256: crate::hash::sha256_bytes(bytes),
        })
    }

    pub fn verify(&self, bytes: &[u8]) -> Result<()> {
        let actual = Self::from_bytes(bytes)?;
        if &actual != self {
            anyhow::bail!("private Wi-Fi profile does not match its authenticated binding");
        }
        Ok(())
    }
}

/// Read the optional Wi-Fi binding from an already authenticated install INI. The three fields
/// are all-or-nothing so an older config remains compatible while a partially written new config
/// cannot silently disable a requested migration.
pub fn private_wifi_binding_from_install_ini(
    content: &str,
) -> Result<Option<PrivateWifiProfileBinding>> {
    let mut enabled = None;
    let mut length = None;
    let mut sha256 = None;
    let mut in_install = false;
    for raw in content.lines() {
        let line = raw.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_install = line.eq_ignore_ascii_case("[Install]");
            continue;
        }
        if !in_install || line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "MigrateWifi" => enabled = Some(value.trim().parse::<bool>()?),
            "WifiProfileLength" => length = Some(value.trim().parse::<u64>()?),
            "WifiProfileSha256" => sha256 = Some(value.trim().to_ascii_lowercase()),
            _ => {}
        }
    }
    match (enabled, length, sha256) {
        (None, None, None) | (Some(false), None, None) => Ok(None),
        (Some(false), Some(0), Some(value)) if value.is_empty() => Ok(None),
        (Some(true), Some(length_bytes), Some(sha256)) => {
            if length_bytes == 0 || length_bytes > PRIVATE_WIFI_PROFILE_MAX_BYTES {
                anyhow::bail!("authenticated Wi-Fi profile length is outside the supported range");
            }
            if sha256.len() != 64
                || !sha256
                    .as_bytes()
                    .iter()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                anyhow::bail!("authenticated Wi-Fi profile SHA-256 is malformed");
            }
            Ok(Some(PrivateWifiProfileBinding {
                length_bytes,
                sha256,
            }))
        }
        _ => anyhow::bail!("authenticated Wi-Fi migration fields are incomplete or contradictory"),
    }
}

const SCRIPT: &str = r#"[CmdletBinding()]
param(
  [switch]$PersonalRestoreAtShell
)
$ErrorActionPreference = 'Stop'
# SystemDrive is a drive-relative designator such as `C:`. Path.Combine('C:', 'name') produces
# `C:name`, whose meaning depends on the process's current directory on that drive. Derive the
# absolute volume root from the already-absolute SystemRoot instead.
$systemVolumeRoot = [System.IO.Path]::GetPathRoot($env:SystemRoot)
if ([string]::IsNullOrWhiteSpace($systemVolumeRoot) -or -not [System.IO.Path]::IsPathRooted($systemVolumeRoot)) {
  throw 'system volume root is unavailable'
}
$directory = [System.IO.Path]::Combine($systemVolumeRoot, 'LetRecovery_Scripts')
$secHealth = [System.IO.Path]::Combine($directory, 'remove-sec-health-ui.ps1')
$curated = [System.IO.Path]::Combine($directory, 'remove-curated-appx.ps1')
$softwareDirectory = [System.IO.Path]::Combine($directory, 'PreinstalledSoftware')
$softwarePlanBase64 = '__LETRECOVERY_SOFTWARE_PLAN_BASE64__'
$automationShutdownOnTerminal = __LETRECOVERY_AUTOMATION_SHUTDOWN_ON_TERMINAL__
$personalRestoreSessionId = '__LETRECOVERY_PERSONAL_RESTORE_SESSION_ID__'
$temporaryOobeAccountHex = '__LETRECOVERY_TEMPORARY_OOBE_ACCOUNT_HEX__'
$builtinAdministratorNameHex = '__LETRECOVERY_BUILTIN_ADMINISTRATOR_NAME_HEX__'
$personalRestoreHelper = [System.IO.Path]::Combine($directory, 'LetRecovery-account-helper.exe')
$personalRestoreLauncher = [System.IO.Path]::Combine($systemVolumeRoot, 'LetRecovery-first-logon.cmd')
$personalRestoreShellGate = [System.IO.Path]::Combine($directory, 'personal-restore-shell-gate.state')
$personalRestoreShellReleased = [System.IO.Path]::Combine($directory, 'personal-restore-shell-gate.released')
$personalRestoreShellVerified = [System.IO.Path]::Combine($directory, 'personal-restore-shell-verified.receipt')
$wifi = [System.IO.Path]::Combine($directory, 'LR_WiFi.xml')
$custom = [System.IO.Path]::Combine($directory, 'firstlogon.bat')
$logDirectory = [System.IO.Path]::Combine($env:ProgramData, 'LetRecovery', 'Logs')
$logPath = [System.IO.Path]::Combine($logDirectory, 'FirstLogon-finalize.log')
[void][System.IO.Directory]::CreateDirectory($logDirectory)
$finalExitCode = 0
$builtinTransitionScheduled = $false
$personalRestoreRestartScheduled = $false
function Convert-LrUtf16Hex([string]$Value) {
  if ([string]::IsNullOrWhiteSpace($Value) -or (($Value.Length % 4) -ne 0) -or $Value -notmatch '\A[0-9a-f]+\z') { throw 'invalid LetRecovery UTF-16 hex field' }
  $builder = [System.Text.StringBuilder]::new([int]($Value.Length / 4))
  for ($offset = 0; $offset -lt $Value.Length; $offset += 4) {
    [void]$builder.Append([char][System.Convert]::ToUInt16($Value.Substring($offset, 4), 16))
  }
  return $builder.ToString()
}
try {
  if (-not $PersonalRestoreAtShell) {
  if (-not [string]::IsNullOrWhiteSpace($builtinAdministratorNameHex)) {
    if (-not [System.IO.File]::Exists($personalRestoreHelper)) { throw 'built-in Administrator transition helper is missing' }
    $temporaryOobeAccountName = Convert-LrUtf16Hex $temporaryOobeAccountHex
    if ([string]::Equals([System.Environment]::UserName, $temporaryOobeAccountName, [System.StringComparison]::OrdinalIgnoreCase)) {
      if (-not [string]::IsNullOrWhiteSpace($personalRestoreSessionId)) {
        $gateProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-activate-personal-restore-shell-gate',$personalRestoreSessionId) -WindowStyle Hidden -Wait -PassThru
        if ($gateProcess.ExitCode -ne 0) { throw ('personal-file Shell gate activation failed with exit code {0}' -f $gateProcess.ExitCode) }
        [System.IO.File]::AppendAllText($logPath, "Personal files restore: Shell gate armed for final built-in Administrator logon`r`n")
      }
      if ([string]::IsNullOrWhiteSpace($personalRestoreSessionId)) {
        $transitionArguments = @('--internal-begin-builtin-administrator-transition',$builtinAdministratorNameHex,$temporaryOobeAccountHex)
      } else {
        $transitionArguments = @('--internal-begin-builtin-administrator-transition-with-personal-restore',$builtinAdministratorNameHex,$temporaryOobeAccountHex,$personalRestoreSessionId)
      }
      [System.IO.File]::AppendAllText($logPath, "First-logon transition: requesting pre-desktop restart force_apps_closed=true`r`n")
      $transitionProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList $transitionArguments -WindowStyle Hidden -Wait -PassThru
      if ($transitionProcess.ExitCode -ne 0) { throw ('built-in Administrator transition failed with exit code {0}' -f $transitionProcess.ExitCode) }
      $builtinTransitionScheduled = $true
      [System.IO.File]::AppendAllText($logPath, "Built-in Administrator transition: restart scheduled`r`n")
      exit 0
    }
    $transitionProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-finish-builtin-administrator-transition',$builtinAdministratorNameHex,$temporaryOobeAccountHex) -WindowStyle Hidden -Wait -PassThru
    if ($transitionProcess.ExitCode -ne 0) { throw ('built-in Administrator transition finalization failed with exit code {0}' -f $transitionProcess.ExitCode) }
    [System.IO.File]::AppendAllText($logPath, "Built-in Administrator transition: final account and profile verified`r`n")
  }
  if (-not [string]::IsNullOrWhiteSpace($personalRestoreSessionId) -and [string]::IsNullOrWhiteSpace($builtinAdministratorNameHex) -and -not [System.IO.File]::Exists($personalRestoreShellGate)) {
    if (-not [System.IO.File]::Exists($personalRestoreHelper)) { throw 'personal-file restore helper is missing' }
    [System.IO.File]::AppendAllText($logPath, "First-logon transition: requesting pre-desktop restart force_apps_closed=true`r`n")
    $secondLogonProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-begin-personal-restore-second-logon',$personalRestoreSessionId) -WindowStyle Hidden -Wait -PassThru
    if ($secondLogonProcess.ExitCode -ne 0) { throw ('personal-file second-logon preparation failed with exit code {0}' -f $secondLogonProcess.ExitCode) }
    $personalRestoreRestartScheduled = $true
    [System.IO.File]::AppendAllText($logPath, "Personal files restore: second logon and immediate restart scheduled before desktop`r`n")
    exit 0
  }
  if (-not [string]::IsNullOrWhiteSpace($temporaryOobeAccountHex)) {
    if (-not [System.IO.File]::Exists($personalRestoreHelper)) { throw 'temporary OOBE account cleanup helper is missing' }
    $cleanupProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-delete-temporary-oobe-account',$temporaryOobeAccountHex) -WindowStyle Hidden -Wait -PassThru
    if ($cleanupProcess.ExitCode -ne 0) { throw ('temporary OOBE account cleanup failed with exit code {0}' -f $cleanupProcess.ExitCode) }
    [System.IO.File]::AppendAllText($logPath, "Temporary OOBE account cleanup: completed`r`n")
  }
  if ([System.IO.File]::Exists($personalRestoreHelper)) {
    $defaultOobeCleanupProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-cleanup-disabled-defaultuser0') -WindowStyle Hidden -Wait -PassThru
    if ($defaultOobeCleanupProcess.ExitCode -ne 0) {
      [System.IO.File]::AppendAllText($logPath, ("Windows default OOBE account cleanup: warning exit={0}`r`n" -f $defaultOobeCleanupProcess.ExitCode))
    } else {
      [System.IO.File]::AppendAllText($logPath, "Windows default OOBE account cleanup: completed`r`n")
    }
  }
  if (-not [string]::IsNullOrWhiteSpace($personalRestoreSessionId)) {
    if (-not [System.IO.File]::Exists($personalRestoreShellGate)) { throw 'personal-file Shell gate state is missing on the restore logon' }
    [System.IO.File]::AppendAllText($logPath, "Personal files restore: starting before Explorer`r`n")
    $restoreProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-restore-personal-files-before-shell',$personalRestoreSessionId) -WindowStyle Hidden -Wait -PassThru
    if ($restoreProcess.ExitCode -ne 0) { throw ('pre-Explorer personal-file restore failed with exit code {0}' -f $restoreProcess.ExitCode) }
    if (-not [System.IO.File]::Exists($personalRestoreShellReleased)) { throw 'personal-file Shell release receipt is missing' }
    [System.IO.File]::AppendAllText($logPath, "Personal files restore: verified before Explorer; original Shell released`r`n")
  }
  if ([System.IO.File]::Exists($secHealth)) {
    [System.IO.File]::AppendAllText($logPath, "Windows Security UI cleanup retry: starting`r`n")
    $process = Start-Process -FilePath ([System.IO.Path]::Combine($env:SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')) -ArgumentList @('-NoP','-NonI','-ExecutionPolicy','Bypass','-File',('"{0}"' -f $secHealth),'-SuppressCurrentSecurityUpdate') -WindowStyle Hidden -PassThru
    if (-not $process.WaitForExit(180000)) {
      try { $process.Kill(); $process.WaitForExit() } catch {}
      [System.IO.File]::AppendAllText($logPath, "Windows Security UI cleanup retry: warning timeout=180000ms`r`n")
    } elseif ($process.ExitCode -ne 0) {
      [System.IO.File]::AppendAllText($logPath, ("Windows Security UI cleanup retry: warning exit={0}`r`n" -f $process.ExitCode))
    } else {
      [System.IO.File]::AppendAllText($logPath, "Windows Security UI cleanup retry: completed; see structured removal log for final inventory`r`n")
    }
  }
  if ([System.IO.File]::Exists($curated)) {
    [System.IO.File]::AppendAllText($logPath, "Preinstalled application verification: starting`r`n")
    $process = Start-Process -FilePath ([System.IO.Path]::Combine($env:SystemRoot, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe')) -ArgumentList @('-NoP','-NonI','-ExecutionPolicy','Bypass','-File',('"{0}"' -f $curated)) -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) {
      [System.IO.File]::AppendAllText($logPath, ("Preinstalled application verification: warning exit={0}`r`n" -f $process.ExitCode))
    } else {
      [System.IO.File]::AppendAllText($logPath, "Preinstalled application verification: completed`r`n")
    }
  }
  $softwarePlanJson = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($softwarePlanBase64))
  # Windows PowerShell 5.1 writes a top-level JSON array as one Object[] pipeline object.
  # Explicit pipeline enumeration is required; otherwise two packages become one entry whose
  # properties format as System.Object[] and no real installer path can be resolved.
  $entries = @((ConvertFrom-Json -InputObject $softwarePlanJson) | ForEach-Object { $_ })
  if ($entries.Count -gt 0) {
    [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: starting expected={0}`r`n" -f $entries.Count))
    $softwareFailures = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in @($entries)) {
      $filename = [string]$entry.filename
      if ([string]::IsNullOrWhiteSpace($filename) -or [System.IO.Path]::GetFileName($filename) -ne $filename) {
        $softwareFailures.Add(("{0}:invalid filename" -f $entry.id))
        [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: failed id={0} detail=invalid filename`r`n" -f $entry.id))
        continue
      }
      $installer = [System.IO.Path]::Combine($softwareDirectory, $filename)
      $installed = $false
      $lastSoftwareError = $null
      for ($attempt = 1; $attempt -le 3 -and -not $installed; $attempt++) {
        try {
          if (-not [System.IO.File]::Exists($installer)) { throw 'installer is missing' }
          if ([string]$entry.program -eq 'installer') {
            $program = $installer
          } elseif ([string]$entry.program -eq 'msiexec') {
            $program = [System.IO.Path]::Combine($env:SystemRoot, 'System32', 'msiexec.exe')
          } else {
            throw 'unsupported program kind'
          }
          $arguments = @($entry.arguments | ForEach-Object {
            $argument = [string]$_
            if ($argument -eq '__LETRECOVERY_FIRST_LOGON_INSTALLER__') { $installer } else { $argument }
          })
          $process = Start-Process -FilePath $program -ArgumentList $arguments -WindowStyle Hidden -PassThru
          if (-not $process.WaitForExit(1800000)) {
            try { $process.Kill(); $process.WaitForExit() } catch {}
            throw 'installer timed out after 1800000ms'
          }
          if ($process.ExitCode -notin @(0, 1641, 3010)) {
            throw ('installer exit code {0}' -f $process.ExitCode)
          }
          $installed = $true
          [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: completed id={0} attempt={1} exit={2}`r`n" -f $entry.id, $attempt, $process.ExitCode))
        } catch {
          $lastSoftwareError = $_.Exception.Message
          if ($attempt -lt 3) {
            [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: retry id={0} attempt={1}/3 detail={2}`r`n" -f $entry.id, $attempt, $lastSoftwareError))
            Start-Sleep -Seconds 5
          }
        }
      }
      if (-not $installed) {
        $softwareFailures.Add(("{0}:{1}" -f $entry.id, $lastSoftwareError))
        [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: warning id={0} attempts=3 detail={1}`r`n" -f $entry.id, $lastSoftwareError))
      }
      try { if ($installed -and [System.IO.File]::Exists($installer)) { [System.IO.File]::Delete($installer) } } catch {
        [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: warning id={0} cleanup={1}`r`n" -f $entry.id, $_.Exception.Message))
      }
    }
    if ($softwareFailures.Count -ne 0) {
      [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: warning failed={0} detail={1}; remaining first-logon work continues`r`n" -f $softwareFailures.Count, ([string]::Join('; ', $softwareFailures))))
    } else {
      [System.IO.File]::AppendAllText($logPath, ("Preinstalled software: finished completed={0}`r`n" -f $entries.Count))
    }
  }
  if ([System.IO.File]::Exists($wifi)) {
    [System.IO.File]::AppendAllText($logPath, "Wi-Fi profile import: starting`r`n")
    if (-not ('LetRecovery.NativeWifiProfile' -as [type])) {
      Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Xml;

namespace LetRecovery {
  public static class NativeWifiProfile {
    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WLAN_INTERFACE_INFO {
      public Guid InterfaceGuid;
      [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Description;
      public int State;
    }

    [DllImport("wlanapi.dll")]
    private static extern uint WlanOpenHandle(uint version, IntPtr reserved, out uint negotiated, out IntPtr handle);
    [DllImport("wlanapi.dll")]
    private static extern uint WlanEnumInterfaces(IntPtr handle, IntPtr reserved, out IntPtr list);
    [DllImport("wlanapi.dll", CharSet = CharSet.Unicode)]
    private static extern uint WlanSetProfile(IntPtr handle, ref Guid guid, uint flags, string xml, string security, bool overwrite, IntPtr reserved, out uint reason);
    [DllImport("wlanapi.dll", CharSet = CharSet.Unicode)]
    private static extern uint WlanGetProfile(IntPtr handle, ref Guid guid, string name, IntPtr reserved, out IntPtr xml, ref uint flags, out uint access);
    [DllImport("wlanapi.dll")]
    private static extern void WlanFreeMemory(IntPtr memory);
    [DllImport("wlanapi.dll")]
    private static extern uint WlanCloseHandle(IntPtr handle, IntPtr reserved);

    private static string ProfileName(string xml) {
      XmlDocument document = new XmlDocument();
      document.PreserveWhitespace = true;
      document.LoadXml(xml);
      XmlNode node = document.SelectSingleNode("/*[local-name()='WLANProfile']/*[local-name()='name']");
      if (node == null || String.IsNullOrEmpty(node.InnerText.Trim())) throw new InvalidDataException("Wi-Fi profile has no name");
      return node.InnerText.Trim();
    }

    public static string Import(string path) {
      string source = File.ReadAllText(path);
      string name = ProfileName(source);
      IntPtr handle = IntPtr.Zero;
      IntPtr list = IntPtr.Zero;
      uint negotiated;
      uint open = WlanOpenHandle(2, IntPtr.Zero, out negotiated, out handle);
      if (open != 0) throw new InvalidOperationException("WlanOpenHandle=" + open);
      try {
        uint enumerate = WlanEnumInterfaces(handle, IntPtr.Zero, out list);
        if (enumerate != 0) throw new InvalidOperationException("WlanEnumInterfaces=" + enumerate);
        int count = Marshal.ReadInt32(list, 0);
        if (count <= 0) throw new InvalidOperationException("WlanEnumInterfaces returned no wireless interface");
        int size = Marshal.SizeOf(typeof(WLAN_INTERFACE_INFO));
        List<string> errors = new List<string>();
        int verified = 0;
        for (int index = 0; index < count; index++) {
          WLAN_INTERFACE_INFO info = (WLAN_INTERFACE_INFO)Marshal.PtrToStructure(new IntPtr(list.ToInt64() + 8L + ((long)index * size)), typeof(WLAN_INTERFACE_INFO));
          uint reason;
          uint set = WlanSetProfile(handle, ref info.InterfaceGuid, 0, source, null, true, IntPtr.Zero, out reason);
          if (set != 0) { errors.Add("set=" + set + "/reason=" + reason); continue; }
          IntPtr returned = IntPtr.Zero;
          try {
            uint flags = 0;
            uint access;
            uint get = WlanGetProfile(handle, ref info.InterfaceGuid, name, IntPtr.Zero, out returned, ref flags, out access);
            if (get != 0 || returned == IntPtr.Zero) { errors.Add("readback=" + get); continue; }
            string actual = Marshal.PtrToStringUni(returned);
            if (!String.Equals(ProfileName(actual), name, StringComparison.Ordinal)) { errors.Add("readback-name-mismatch"); continue; }
            verified++;
          } finally {
            if (returned != IntPtr.Zero) WlanFreeMemory(returned);
          }
        }
        if (verified == 0) throw new InvalidOperationException("no interface accepted and read back the profile; " + String.Join(",", errors.ToArray()));
        return "verified interfaces=" + verified;
      } finally {
        if (list != IntPtr.Zero) WlanFreeMemory(list);
        if (handle != IntPtr.Zero) {
          uint close = WlanCloseHandle(handle, IntPtr.Zero);
          if (close != 0) throw new InvalidOperationException("WlanCloseHandle=" + close);
        }
      }
    }
  }
}
'@ -ReferencedAssemblies @('System.dll','System.Xml.dll')
    }
    $wifiResult = $null
    $wifiError = $null
    for ($attempt = 1; $attempt -le 12; $attempt++) {
      try {
        $wifiResult = [LetRecovery.NativeWifiProfile]::Import($wifi)
        break
      } catch {
        $wifiError = $_.Exception.Message
        if ($attempt -lt 12) { Start-Sleep -Seconds 5 }
      }
    }
    if ($null -eq $wifiResult) { throw ('Wi-Fi profile import failed after bounded retries: {0}' -f $wifiError) }
    [System.IO.File]::AppendAllText($logPath, ("Wi-Fi profile import: {0}`r`n" -f $wifiResult))
  }
  if ([System.IO.File]::Exists($custom)) {
    [System.IO.File]::AppendAllText($logPath, "Custom first-logon script: starting`r`n")
    $process = Start-Process -FilePath $env:ComSpec -ArgumentList @('/d','/c',('call "{0}"' -f $custom)) -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) { throw ('custom first-logon script failed with exit code {0}' -f $process.ExitCode) }
  }
  }
  if ($PersonalRestoreAtShell) {
    if ([string]::IsNullOrWhiteSpace($personalRestoreSessionId)) { throw 'personal-file Explorer-stage restore is not configured' }
    [System.IO.File]::AppendAllText($logPath, "Personal files restore: starting`r`n")
    if (-not [System.IO.File]::Exists($personalRestoreHelper)) { throw 'personal-file restore helper is missing' }
    $restoreStdout = [System.IO.Path]::Combine($directory, 'personal-restore.stdout.txt')
    $restoreStderr = [System.IO.Path]::Combine($directory, 'personal-restore.stderr.txt')
    try {
      [System.IO.File]::Delete($restoreStdout)
      [System.IO.File]::Delete($restoreStderr)
      # LetRecoveryPE.exe uses the GUI subsystem. PowerShell's call operator does not provide a
      # reliable synchronous exit code for GUI applications, so use an explicit process handle.
      $restoreProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-restore-personal-files-at-shell',$personalRestoreSessionId) -WindowStyle Hidden -RedirectStandardOutput $restoreStdout -RedirectStandardError $restoreStderr -Wait -PassThru
      foreach ($outputPath in @($restoreStdout,$restoreStderr)) {
        if ([System.IO.File]::Exists($outputPath)) {
          foreach ($line in [System.IO.File]::ReadAllLines($outputPath)) {
            [System.IO.File]::AppendAllText($logPath, ("Personal files restore: {0}`r`n" -f [string]$line))
          }
        }
      }
      if ($restoreProcess.ExitCode -ne 0) { throw ('personal-file restore helper failed with exit code {0}' -f $restoreProcess.ExitCode) }
    } finally {
      try { [System.IO.File]::Delete($restoreStdout) } catch {}
      try { [System.IO.File]::Delete($restoreStderr) } catch {}
    }
    [System.IO.File]::AppendAllText($logPath, "Personal files restore: completed`r`n")
  } elseif (-not [string]::IsNullOrWhiteSpace($personalRestoreSessionId) -and -not [System.IO.File]::Exists($personalRestoreShellReleased)) {
    throw 'personal-file restore did not produce the pre-Explorer Shell release receipt'
  }
  # The Explorer-stage worker runs after the main finalizer has already retired the
  # one-shot autologon, deleted the LSA secret, and removed the transition marker.
  # It must only restore personal files; repeating credential retirement would turn
  # that successful cleanup into a false failure because the marker is intentionally gone.
  if (-not $PersonalRestoreAtShell -and -not [string]::IsNullOrWhiteSpace($builtinAdministratorNameHex)) {
    $retireProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-retire-builtin-administrator-transition',$builtinAdministratorNameHex,$temporaryOobeAccountHex) -WindowStyle Hidden -Wait -PassThru
    if ($retireProcess.ExitCode -ne 0) { throw ('built-in Administrator transition retirement failed with exit code {0}' -f $retireProcess.ExitCode) }
    [System.IO.File]::AppendAllText($logPath, "Built-in Administrator transition: temporary autologon retired`r`n")
  }
  if (-not $PersonalRestoreAtShell -and -not [string]::IsNullOrWhiteSpace($personalRestoreSessionId)) {
    $shellVerificationDeadline = [DateTime]::UtcNow.AddSeconds(90)
    $verifiedShellPid = 0
    do {
      if ([System.IO.File]::Exists($personalRestoreShellVerified)) {
        $verifiedShellReceipt = [System.IO.File]::ReadAllText($personalRestoreShellVerified, [System.Text.UTF8Encoding]::new($false, $true))
        $verifiedShellMatch = [regex]::Match($verifiedShellReceipt, '\A([0-9a-f]{32}):([1-9][0-9]{0,9})\z')
        if ($verifiedShellMatch.Success -and [string]::Equals($verifiedShellMatch.Groups[1].Value, $personalRestoreSessionId, [System.StringComparison]::Ordinal)) {
          $verifiedShellPid = [uint32]$verifiedShellMatch.Groups[2].Value
          break
        }
      }
      if ([DateTime]::UtcNow -ge $shellVerificationDeadline) {
        throw 'verified current-session Explorer Shell receipt timed out'
      }
      Start-Sleep -Milliseconds 100
    } while ($true)
    [System.IO.File]::AppendAllText($logPath, ("Personal files restore: verified current-session Explorer shell pid={0}`r`n" -f $verifiedShellPid))
  }
  [System.IO.File]::AppendAllText($logPath, "First-logon finalization: completed`r`n")
} catch {
  [System.IO.File]::AppendAllText($logPath, ('First-logon finalization: failed: {0}`r`n' -f $_.Exception.Message))
  if (-not [string]::IsNullOrWhiteSpace($personalRestoreSessionId) -and [System.IO.File]::Exists($personalRestoreShellGate) -and -not [System.IO.File]::Exists($personalRestoreShellReleased) -and [System.IO.File]::Exists($personalRestoreHelper)) {
    try {
      $retryProcess = Start-Process -FilePath $personalRestoreHelper -ArgumentList @('--internal-rearm-personal-restore-before-shell',$personalRestoreSessionId) -WindowStyle Hidden -Wait -PassThru
      [System.IO.File]::AppendAllText($logPath, ("Personal files restore: retry RunOnce rearm exit={0}`r`n" -f $retryProcess.ExitCode))
    } catch {
      [System.IO.File]::AppendAllText($logPath, ('Personal files restore: retry RunOnce rearm failed: {0}`r`n' -f $_.Exception.Message))
    }
  }
  [Console]::Error.WriteLine(('LETRECOVERY_FIRST_LOGON_FAILURE {0}' -f $_.Exception.Message))
  $finalExitCode = 3
} finally {
  [System.IO.File]::AppendAllText($logPath, "First-logon staging cleanup: deferred until the PowerShell process exits`r`n")
  if (-not $builtinTransitionScheduled -and -not $personalRestoreRestartScheduled -and $automationShutdownOnTerminal -and ([string]::IsNullOrWhiteSpace($personalRestoreSessionId) -or $PersonalRestoreAtShell -or [System.IO.File]::Exists($personalRestoreShellReleased))) {
    try {
      if (-not ('LetRecovery.AutomationPower' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace LetRecovery {
  public static class AutomationPower {
    [StructLayout(LayoutKind.Sequential)] private struct LUID { public uint LowPart; public int HighPart; }
    [StructLayout(LayoutKind.Sequential)] private struct LUID_AND_ATTRIBUTES { public LUID Luid; public uint Attributes; }
    [StructLayout(LayoutKind.Sequential)] private struct TOKEN_PRIVILEGES { public uint PrivilegeCount; public LUID_AND_ATTRIBUTES Privileges; }

    [DllImport("kernel32.dll")] private static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll")] private static extern void SetLastError(uint error);
    [DllImport("advapi32.dll", SetLastError=true)] private static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] private static extern bool LookupPrivilegeValue(string system, string name, out LUID luid);
    [DllImport("advapi32.dll", SetLastError=true)] private static extern bool AdjustTokenPrivileges(IntPtr token, bool disableAll, ref TOKEN_PRIVILEGES privileges, uint length, IntPtr previous, IntPtr returnedLength);
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] private static extern bool InitiateSystemShutdownEx(string machine, string message, uint timeout, bool forceAppsClosed, bool rebootAfterShutdown, uint reason);

    public static void Schedule() {
      const uint TOKEN_QUERY = 0x0008;
      const uint TOKEN_ADJUST_PRIVILEGES = 0x0020;
      const uint SE_PRIVILEGE_ENABLED = 0x0002;
      const int ERROR_NOT_ALL_ASSIGNED = 1300;
      const int ERROR_SHUTDOWN_IN_PROGRESS = 1115;
      const uint REASON_APPLICATION_INSTALLATION_PLANNED = 0x80040002;
      IntPtr token;
      if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES, out token)) throw new Win32Exception(Marshal.GetLastWin32Error(), "OpenProcessToken");
      try {
        LUID luid;
        if (!LookupPrivilegeValue(null, "SeShutdownPrivilege", out luid)) throw new Win32Exception(Marshal.GetLastWin32Error(), "LookupPrivilegeValue");
        TOKEN_PRIVILEGES privileges = new TOKEN_PRIVILEGES();
        privileges.PrivilegeCount = 1;
        privileges.Privileges.Luid = luid;
        privileges.Privileges.Attributes = SE_PRIVILEGE_ENABLED;
        SetLastError(0);
        if (!AdjustTokenPrivileges(token, false, ref privileges, 0, IntPtr.Zero, IntPtr.Zero)) throw new Win32Exception(Marshal.GetLastWin32Error(), "AdjustTokenPrivileges");
        int privilegeError = Marshal.GetLastWin32Error();
        if (privilegeError == ERROR_NOT_ALL_ASSIGNED) throw new Win32Exception(privilegeError, "SeShutdownPrivilege is not assigned");
        if (privilegeError != 0) throw new Win32Exception(privilegeError, "AdjustTokenPrivileges");
        if (!InitiateSystemShutdownEx(null, "LetRecovery automation finished; this test machine will power off.", 300, false, false, REASON_APPLICATION_INSTALLATION_PLANNED)) {
          int shutdownError = Marshal.GetLastWin32Error();
          if (shutdownError != ERROR_SHUTDOWN_IN_PROGRESS) throw new Win32Exception(shutdownError, "InitiateSystemShutdownEx");
        }
      } finally {
        if (token != IntPtr.Zero) CloseHandle(token);
      }
    }
  }
}
'@
      }
      [LetRecovery.AutomationPower]::Schedule()
      [System.IO.File]::AppendAllText($logPath, "Automation shutdown: accepted timeout=300s force_apps_closed=false reboot=false`r`n")
    } catch {
      [System.IO.File]::AppendAllText($logPath, ("Automation shutdown: failed detail={0}`r`n" -f $_.Exception.Message))
      if ($finalExitCode -eq 0) { $finalExitCode = 4 }
    }
  }
}
exit $finalExitCode
"#;

pub fn stage(target_partition: &str) -> Result<PathBuf> {
    stage_with_software(target_partition, &[])
}

pub fn stage_with_software(
    target_partition: &str,
    packages: &[crate::software_install::SelectedSoftwarePackage],
) -> Result<PathBuf> {
    stage_with_software_and_shutdown(target_partition, packages, false)
}

pub fn stage_with_software_and_shutdown(
    target_partition: &str,
    packages: &[crate::software_install::SelectedSoftwarePackage],
    automation_shutdown_on_terminal: bool,
) -> Result<PathBuf> {
    stage_with_software_shutdown_and_personal_restore_and_builtin(
        target_partition,
        packages,
        automation_shutdown_on_terminal,
        None,
        None,
    )
}

pub fn stage_with_software_shutdown_and_personal_restore(
    target_partition: &str,
    packages: &[crate::software_install::SelectedSoftwarePackage],
    automation_shutdown_on_terminal: bool,
    personal_restore_session_id: Option<&str>,
) -> Result<PathBuf> {
    stage_with_software_shutdown_and_personal_restore_and_builtin(
        target_partition,
        packages,
        automation_shutdown_on_terminal,
        personal_restore_session_id,
        None,
    )
}

pub fn stage_with_software_shutdown_and_personal_restore_and_builtin(
    target_partition: &str,
    packages: &[crate::software_install::SelectedSoftwarePackage],
    automation_shutdown_on_terminal: bool,
    personal_restore_session_id: Option<&str>,
    builtin_transition: Option<BuiltinAdministratorTransitionAccounts<'_>>,
) -> Result<PathBuf> {
    let (temporary_oobe_account_name, builtin_administrator_name) = match builtin_transition {
        Some(transition) => (
            Some(transition.temporary_name),
            Some(transition.desired_name),
        ),
        None => (None, None),
    };
    let root = normalized_target_root(target_partition)?;
    let directory = root.join("LetRecovery_Scripts");
    std::fs::create_dir_all(&directory)?;
    reject_reparse_or_non_directory(&directory)?;
    verify_staged_software(&directory, packages)?;
    stage_personal_restore_marker(&directory, personal_restore_session_id)?;
    stage_builtin_transition_secret(
        &directory,
        builtin_transition.map(|transition| transition.password),
    )?;
    let rendered = rendered_script(
        packages,
        automation_shutdown_on_terminal,
        personal_restore_session_id,
        temporary_oobe_account_name,
        builtin_administrator_name,
    )?;
    let target = directory.join(SCRIPT_FILE_NAME);
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        &directory,
        "lr-first-logon",
        "ps1",
        rendered.as_bytes(),
    )?;
    temporary.persist_replace(&target)?;
    if std::fs::read(&target)? != rendered.as_bytes() {
        anyhow::bail!("first-logon finalizer readback mismatch");
    }
    let launcher = root.join(LAUNCHER_FILE_NAME);
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        &root,
        "lr-first-logon-launcher",
        "cmd",
        LAUNCHER.as_bytes(),
    )?;
    temporary.persist_replace(&launcher)?;
    if std::fs::read(&launcher)? != LAUNCHER.as_bytes() {
        anyhow::bail!("first-logon launcher readback mismatch");
    }
    Ok(target)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BuiltinAdministratorTransitionAccounts<'a> {
    pub desired_name: &'a str,
    pub temporary_name: &'a str,
    pub password: &'a crate::unattend_account::SensitiveString,
}

fn stage_builtin_transition_secret(
    directory: &Path,
    password: Option<&crate::unattend_account::SensitiveString>,
) -> Result<()> {
    let target = directory.join(BUILTIN_TRANSITION_SECRET_STAGING_FILE_NAME);
    match std::fs::symlink_metadata(&target) {
        Ok(metadata) if !metadata.is_file() || metadata_is_reparse_point(&metadata) => {
            anyhow::bail!("built-in Administrator secret staging path is not an ordinary file");
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let Some(password) = password else {
        if target.exists() {
            std::fs::remove_file(&target).with_context(|| {
                format!(
                    "remove stale built-in Administrator secret staging file {}",
                    target.display()
                )
            })?;
        }
        return Ok(());
    };
    let bytes = crate::unattend_account::serialize_protected_administrator_secret(password)
        .map_err(anyhow::Error::from)?;
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-builtin-administrator-secret",
        "pending",
        &bytes,
    )?;
    temporary.persist_replace(&target)?;
    let readback = Zeroizing::new(std::fs::read(&target)?);
    if !bool::from(readback.as_slice().ct_eq(bytes.as_slice())) {
        anyhow::bail!("built-in Administrator secret staging readback mismatch");
    }
    Ok(())
}

fn rendered_script(
    packages: &[crate::software_install::SelectedSoftwarePackage],
    automation_shutdown_on_terminal: bool,
    personal_restore_session_id: Option<&str>,
    temporary_oobe_account_name: Option<&str>,
    builtin_administrator_name: Option<&str>,
) -> Result<String> {
    let personal_restore_session_id = personal_restore_session_id.unwrap_or_default();
    if !personal_restore_session_id.is_empty() {
        validate_personal_restore_session_id(personal_restore_session_id)?;
    }
    let plan = crate::software_install::first_logon_plan_bytes(packages)?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(plan);
    let temporary_oobe_account_hex = match temporary_oobe_account_name {
        Some(account_name) => {
            crate::unattend_account::validate_temporary_oobe_account_name(account_name)
                .map_err(anyhow::Error::from)?;
            crate::windows_accounts::encode_account_name_utf16_hex(account_name)
                .map_err(anyhow::Error::from)?
        }
        None => String::new(),
    };
    let builtin_administrator_name_hex = match builtin_administrator_name {
        Some(account_name) => crate::windows_accounts::encode_account_name_utf16_hex(account_name)
            .map_err(anyhow::Error::from)?,
        None => String::new(),
    };
    if builtin_administrator_name_hex.is_empty() != temporary_oobe_account_hex.is_empty() {
        anyhow::bail!("built-in Administrator transition requires both account identities");
    }
    Ok(SCRIPT
        .replace("__LETRECOVERY_SOFTWARE_PLAN_BASE64__", &encoded)
        .replace(
            "__LETRECOVERY_AUTOMATION_SHUTDOWN_ON_TERMINAL__",
            if automation_shutdown_on_terminal {
                "$true"
            } else {
                "$false"
            },
        )
        .replace(
            "__LETRECOVERY_PERSONAL_RESTORE_SESSION_ID__",
            personal_restore_session_id,
        )
        .replace(
            "__LETRECOVERY_TEMPORARY_OOBE_ACCOUNT_HEX__",
            &temporary_oobe_account_hex,
        )
        .replace(
            "__LETRECOVERY_BUILTIN_ADMINISTRATOR_NAME_HEX__",
            &builtin_administrator_name_hex,
        ))
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct BuiltinAdministratorTransition {
    desired_name_hex: String,
    temporary_name_hex: String,
    temporary_sid: String,
}

impl BuiltinAdministratorTransition {
    fn new(desired_name: &str, temporary_name: &str, temporary_sid: &str) -> Result<Self> {
        crate::windows_accounts::validate_new_account_name(desired_name)
            .map_err(anyhow::Error::from)?;
        crate::unattend_account::validate_temporary_oobe_account_name(temporary_name)
            .map_err(anyhow::Error::from)?;
        if desired_name.eq_ignore_ascii_case(temporary_name) {
            anyhow::bail!("built-in Administrator transition account identities conflict");
        }
        if temporary_sid.is_empty()
            || temporary_sid.len() > 184
            || !temporary_sid.starts_with("S-1-")
            || !temporary_sid
                .bytes()
                .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
        {
            anyhow::bail!("built-in Administrator transition SID is invalid");
        }
        Ok(Self {
            desired_name_hex: crate::windows_accounts::encode_account_name_utf16_hex(desired_name)
                .map_err(anyhow::Error::from)?,
            temporary_name_hex: crate::windows_accounts::encode_account_name_utf16_hex(
                temporary_name,
            )
            .map_err(anyhow::Error::from)?,
            temporary_sid: temporary_sid.to_owned(),
        })
    }

    fn render(&self) -> Vec<u8> {
        format!(
            "{BUILTIN_TRANSITION_MAGIC}\r\nDesiredNameHex={}\r\nTemporaryNameHex={}\r\nTemporarySid={}\r\n",
            self.desired_name_hex, self.temporary_name_hex, self.temporary_sid
        )
        .into_bytes()
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() as u64 > BUILTIN_TRANSITION_MAX_BYTES {
            anyhow::bail!("built-in Administrator transition marker is outside its size limit");
        }
        let text = std::str::from_utf8(bytes)
            .context("built-in Administrator transition marker is not UTF-8")?;
        let mut lines = text.split("\r\n");
        if lines.next() != Some(BUILTIN_TRANSITION_MAGIC) {
            anyhow::bail!("unsupported built-in Administrator transition marker");
        }
        let desired_name_hex = lines
            .next()
            .and_then(|line| line.strip_prefix("DesiredNameHex="))
            .context("built-in Administrator transition marker has no desired account")?
            .to_owned();
        let temporary_name_hex = lines
            .next()
            .and_then(|line| line.strip_prefix("TemporaryNameHex="))
            .context("built-in Administrator transition marker has no temporary account")?
            .to_owned();
        let temporary_sid = lines
            .next()
            .and_then(|line| line.strip_prefix("TemporarySid="))
            .context("built-in Administrator transition marker has no temporary SID")?
            .to_owned();
        if lines.next() != Some("") || lines.next().is_some() {
            anyhow::bail!("built-in Administrator transition marker has trailing fields");
        }
        let desired_name =
            crate::windows_accounts::decode_account_name_utf16_hex(&desired_name_hex)
                .map_err(anyhow::Error::from)?;
        let temporary_name =
            crate::windows_accounts::decode_account_name_utf16_hex(&temporary_name_hex)
                .map_err(anyhow::Error::from)?;
        let parsed = Self::new(&desired_name, &temporary_name, &temporary_sid)?;
        if parsed.render() != bytes {
            anyhow::bail!("built-in Administrator transition marker is not canonical");
        }
        Ok(parsed)
    }

    fn verify_names(&self, desired_name: &str, temporary_name: &str) -> Result<()> {
        if crate::windows_accounts::decode_account_name_utf16_hex(&self.desired_name_hex)
            .map_err(anyhow::Error::from)?
            != desired_name
            || crate::windows_accounts::decode_account_name_utf16_hex(&self.temporary_name_hex)
                .map_err(anyhow::Error::from)?
                != temporary_name
        {
            anyhow::bail!("built-in Administrator transition marker identity mismatch");
        }
        Ok(())
    }
}

fn builtin_transition_directory() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("resolve current account helper")?;
    let directory = executable
        .parent()
        .context("account helper has no parent directory")?
        .to_path_buf();
    reject_reparse_or_non_directory(&directory)?;
    if directory.file_name().and_then(|name| name.to_str()) != Some("LetRecovery_Scripts") {
        anyhow::bail!("account helper is outside the fixed LetRecovery staging directory");
    }
    Ok(directory)
}

fn builtin_transition_marker_path() -> Result<PathBuf> {
    Ok(builtin_transition_directory()?.join(BUILTIN_TRANSITION_MARKER_FILE_NAME))
}

fn read_builtin_transition_marker(path: &Path) -> Result<BuiltinAdministratorTransition> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect transition marker {}", path.display()))?;
    if !metadata.is_file()
        || metadata_is_reparse_point(&metadata)
        || metadata.len() == 0
        || metadata.len() > BUILTIN_TRANSITION_MAX_BYTES
    {
        anyhow::bail!("built-in Administrator transition marker is not an ordinary bounded file");
    }
    BuiltinAdministratorTransition::parse(
        &std::fs::read(path)
            .with_context(|| format!("read transition marker {}", path.display()))?,
    )
}

fn require_current_account(expected: &str) -> Result<()> {
    let actual =
        crate::windows_accounts::current_local_account_name().map_err(anyhow::Error::from)?;
    if actual != expected {
        anyhow::bail!(
            "current account does not match the expected transition account: expected={expected:?} actual={actual:?}"
        );
    }
    Ok(())
}

/// Move the authenticated plaintext staging payload into a local-only LSA secret while Setup is
/// still running as SYSTEM, then remove the plaintext file. This is the documented LSA secret
/// use case (a password that must survive a reboot); `L$` also prevents remote retrieval.
pub fn protect_staged_builtin_administrator_secret() -> Result<()> {
    let directory = builtin_transition_directory()?;
    let path = directory.join(BUILTIN_TRANSITION_SECRET_STAGING_FILE_NAME);
    let bytes = match std::fs::symlink_metadata(&path) {
        Ok(metadata)
            if metadata.is_file()
                && !metadata_is_reparse_point(&metadata)
                && metadata.len() > 0
                && metadata.len()
                    <= crate::unattend_account::PROTECTED_ADMINISTRATOR_SECRET_MAX_BYTES =>
        {
            Some(Zeroizing::new(std::fs::read(&path).with_context(|| {
                format!("read staged Administrator secret {}", path.display())
            })?))
        }
        Ok(_) => {
            anyhow::bail!("staged built-in Administrator secret is not an ordinary bounded file")
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if let Some(bytes) = bytes {
        let password = crate::unattend_account::parse_protected_administrator_secret(&bytes)
            .map_err(anyhow::Error::msg)?;
        crate::windows_accounts::store_builtin_transition_password(password.as_str())
            .map_err(anyhow::Error::from)?;
        let readback = crate::windows_accounts::retrieve_builtin_transition_password()
            .map_err(anyhow::Error::from)?;
        if !bool::from(readback.as_bytes().ct_eq(password.as_bytes())) {
            anyhow::bail!("built-in Administrator LSA secret readback mismatch");
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("remove staged Administrator secret {}", path.display()))?;
        if std::fs::symlink_metadata(&path).is_ok() {
            anyhow::bail!("plaintext built-in Administrator secret survived protection");
        }
    } else {
        // A repeated specialize invocation after a successful store is safe and idempotent.
        crate::windows_accounts::retrieve_builtin_transition_password()
            .map_err(anyhow::Error::from)?;
    }
    Ok(())
}

/// Complete OOBE under the session-bound temporary administrator, then prepare RID 500 and
/// schedule exactly one second autologon. The password is never passed on a command line or
/// logged: specialize encrypted it as a local-only LSA secret before deleting plaintext staging.
pub fn begin_builtin_administrator_transition(
    desired_name: &str,
    temporary_name: &str,
) -> Result<()> {
    begin_builtin_administrator_transition_inner(desired_name, temporary_name, None)
}

pub fn begin_builtin_administrator_transition_with_personal_restore(
    desired_name: &str,
    temporary_name: &str,
    session_id: &str,
) -> Result<()> {
    validate_personal_restore_session_id(session_id)?;
    begin_builtin_administrator_transition_inner(desired_name, temporary_name, Some(session_id))
}

fn begin_builtin_administrator_transition_inner(
    desired_name: &str,
    temporary_name: &str,
    personal_restore_session_id: Option<&str>,
) -> Result<()> {
    crate::windows_accounts::validate_new_account_name(desired_name)
        .map_err(anyhow::Error::from)?;
    crate::unattend_account::validate_temporary_oobe_account_name(temporary_name)
        .map_err(anyhow::Error::from)?;
    require_current_account(temporary_name)?;

    let directory = builtin_transition_directory()?;
    let marker_path = directory.join(BUILTIN_TRANSITION_MARKER_FILE_NAME);
    let transition = match std::fs::symlink_metadata(&marker_path) {
        Ok(_) => {
            let existing = read_builtin_transition_marker(&marker_path)?;
            existing.verify_names(desired_name, temporary_name)?;
            existing
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let sid = crate::windows_accounts::local_account_sid_string(temporary_name)
                .map_err(anyhow::Error::from)?;
            let transition =
                BuiltinAdministratorTransition::new(desired_name, temporary_name, &sid)?;
            let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
                &directory,
                "lr-builtin-administrator-transition",
                "state",
                &transition.render(),
            )?;
            crate::scoped_temp_file::atomic_publish_new_path(temporary.path(), &marker_path)?;
            if read_builtin_transition_marker(&marker_path)? != transition {
                anyhow::bail!("built-in Administrator transition marker readback mismatch");
            }
            transition
        }
        Err(error) => return Err(error.into()),
    };
    transition.verify_names(desired_name, temporary_name)?;

    crate::windows_accounts::prepare_local_account_by_rid(500, desired_name)
        .map_err(anyhow::Error::from)?;
    let password = crate::windows_accounts::retrieve_builtin_transition_password()
        .map_err(anyhow::Error::from)?;

    let launcher = directory
        .parent()
        .context("LetRecovery staging directory has no volume root")?
        .join(LAUNCHER_FILE_NAME);
    let launcher_metadata = std::fs::symlink_metadata(&launcher)
        .with_context(|| format!("inspect first-logon launcher {}", launcher.display()))?;
    if !launcher_metadata.is_file() || metadata_is_reparse_point(&launcher_metadata) {
        anyhow::bail!("first-logon launcher is not an ordinary file");
    }
    let command_interpreter = crate::windows_compat::system_directory()?.join("cmd.exe");
    let command = format!(
        r#""{}" /d /c "{}""#,
        command_interpreter.display(),
        launcher.display()
    );
    if command.encode_utf16().count() > 260 {
        anyhow::bail!("built-in Administrator RunOnce command exceeds 260 UTF-16 code units");
    }
    if let Some(session_id) = personal_restore_session_id {
        register_personal_restore_logon_task(session_id, desired_name)?;
    } else {
        crate::registry::OfflineRegistry::set_string(
            MACHINE_RUN_ONCE_KEY,
            BUILTIN_TRANSITION_RUN_VALUE,
            &command,
        )?;
    }
    crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "DefaultUserName", desired_name)?;
    crate::registry::OfflineRegistry::set_string(
        WINLOGON_KEY,
        "DefaultPassword",
        password.as_str(),
    )?;
    crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "AutoAdminLogon", "1")?;
    crate::registry::OfflineRegistry::set_dword(WINLOGON_KEY, "AutoLogonCount", 1)?;
    let run_once_matches = match personal_restore_session_id {
        Some(_) => true,
        None => {
            crate::registry::OfflineRegistry::query_string(
                MACHINE_RUN_ONCE_KEY,
                BUILTIN_TRANSITION_RUN_VALUE,
            )? == command
        }
    };
    if !run_once_matches
        || crate::registry::OfflineRegistry::query_string(WINLOGON_KEY, "DefaultUserName")?
            != desired_name
        || !bool::from(
            Zeroizing::new(crate::registry::OfflineRegistry::query_string(
                WINLOGON_KEY,
                "DefaultPassword",
            )?)
            .as_bytes()
            .ct_eq(password.as_bytes()),
        )
        || crate::registry::OfflineRegistry::query_string(WINLOGON_KEY, "AutoAdminLogon")? != "1"
        || crate::registry::OfflineRegistry::query_dword(WINLOGON_KEY, "AutoLogonCount")? != 1
    {
        anyhow::bail!("built-in Administrator autologon registry readback mismatch");
    }
    // Microsoft documents that bForceAppsClosed=FALSE can leave the shutdown pending on any
    // console application.  This is an internal OOBE transition before the requested desktop is
    // released, after the account/autologon state has been durably read back; no user document is
    // available to preserve, so use the explicit unattended restart boundary.
    crate::windows_shutdown::schedule_restart_for_automation(
        0,
        "LetRecovery is switching from its temporary OOBE account to the requested built-in Administrator account.",
    )?;
    Ok(())
}

/// Verify the second logon is the requested RID-500 account, then delete the exact temporary SAM
/// account and the profile identified by the SID captured before the first restart.
pub fn finish_builtin_administrator_transition(
    desired_name: &str,
    temporary_name: &str,
) -> Result<()> {
    crate::windows_accounts::validate_new_account_name(desired_name)
        .map_err(anyhow::Error::from)?;
    crate::unattend_account::validate_temporary_oobe_account_name(temporary_name)
        .map_err(anyhow::Error::from)?;
    require_current_account(desired_name)?;
    crate::windows_accounts::prepare_local_account_by_rid(500, desired_name)
        .map_err(anyhow::Error::from)?;
    let marker = read_builtin_transition_marker(&builtin_transition_marker_path()?)?;
    marker.verify_names(desired_name, temporary_name)?;
    crate::windows_accounts::delete_local_account_and_profile(temporary_name, &marker.temporary_sid)
        .map_err(anyhow::Error::from)
}

/// Remove the one-shot autologon material only after the normal first-logon finalizer has
/// completed. The transition marker is removed last so launcher cleanup cannot erase diagnostics
/// while any credential cleanup remains unresolved.
pub fn retire_builtin_administrator_transition(
    desired_name: &str,
    temporary_name: &str,
) -> Result<()> {
    require_current_account(desired_name)?;
    let marker_path = builtin_transition_marker_path()?;
    let marker = read_builtin_transition_marker(&marker_path)?;
    marker.verify_names(desired_name, temporary_name)?;
    crate::windows_accounts::delete_local_account_and_profile(
        temporary_name,
        &marker.temporary_sid,
    )
    .map_err(anyhow::Error::from)?;
    for value in ["DefaultPassword", "AutoAdminLogon"] {
        if crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, value)?.is_some() {
            crate::registry::OfflineRegistry::delete_value(WINLOGON_KEY, value)?;
        }
    }
    if crate::registry::OfflineRegistry::query_dword_optional(WINLOGON_KEY, "AutoLogonCount")?
        .is_some()
    {
        crate::registry::OfflineRegistry::delete_value(WINLOGON_KEY, "AutoLogonCount")?;
    }
    if crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "DefaultPassword")?
        .is_some()
        || crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "AutoAdminLogon")?
            .is_some()
        || crate::registry::OfflineRegistry::query_dword_optional(WINLOGON_KEY, "AutoLogonCount")?
            .is_some()
    {
        anyhow::bail!("built-in Administrator autologon values survived retirement");
    }
    crate::windows_accounts::delete_builtin_transition_password().map_err(anyhow::Error::from)?;
    std::fs::remove_file(&marker_path)
        .with_context(|| format!("remove transition marker {}", marker_path.display()))?;
    if std::fs::symlink_metadata(&marker_path).is_ok() {
        anyhow::bail!("built-in Administrator transition marker survived retirement");
    }
    Ok(())
}

fn validate_personal_restore_session_id(session_id: &str) -> Result<()> {
    if session_id.len() != 32
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        anyhow::bail!("invalid personal-file restore session id");
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PersonalRestoreShellGate {
    session_id: String,
    original_shell: Option<String>,
}

impl PersonalRestoreShellGate {
    fn new(session_id: &str, original_shell: Option<String>) -> Result<Self> {
        validate_personal_restore_session_id(session_id)?;
        if original_shell
            .as_ref()
            .is_some_and(|value| value.contains('\0') || value.encode_utf16().count() > 4096)
        {
            anyhow::bail!("original Winlogon Shell value is outside the supported limit");
        }
        Ok(Self {
            session_id: session_id.to_owned(),
            original_shell,
        })
    }

    fn render(&self) -> Vec<u8> {
        let (present, encoded) = match &self.original_shell {
            Some(value) => (
                "1",
                base64::engine::general_purpose::STANDARD.encode(value.as_bytes()),
            ),
            None => ("0", String::new()),
        };
        format!(
            "{PERSONAL_RESTORE_SHELL_GATE_MAGIC}\r\nSessionId={}\r\nOriginalShellPresent={present}\r\nOriginalShellUtf8Base64={encoded}\r\n",
            self.session_id
        )
        .into_bytes()
    }

    fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() as u64 > PERSONAL_RESTORE_SHELL_GATE_MAX_BYTES {
            anyhow::bail!("personal-file Shell gate state is outside its size limit");
        }
        let text =
            std::str::from_utf8(bytes).context("personal-file Shell gate state is not UTF-8")?;
        let mut lines = text.split("\r\n");
        if lines.next() != Some(PERSONAL_RESTORE_SHELL_GATE_MAGIC) {
            anyhow::bail!("unsupported personal-file Shell gate state");
        }
        let session_id = lines
            .next()
            .and_then(|line| line.strip_prefix("SessionId="))
            .context("personal-file Shell gate state has no session id")?;
        let present = lines
            .next()
            .and_then(|line| line.strip_prefix("OriginalShellPresent="))
            .context("personal-file Shell gate state has no original-value flag")?;
        let encoded = lines
            .next()
            .and_then(|line| line.strip_prefix("OriginalShellUtf8Base64="))
            .context("personal-file Shell gate state has no original value")?;
        if lines.next() != Some("") || lines.next().is_some() {
            anyhow::bail!("personal-file Shell gate state has trailing fields");
        }
        let original_shell = match present {
            "0" if encoded.is_empty() => None,
            "1" => Some(
                String::from_utf8(
                    base64::engine::general_purpose::STANDARD
                        .decode(encoded)
                        .context("personal-file Shell gate state has invalid base64")?,
                )
                .context("personal-file Shell gate original value is not UTF-8")?,
            ),
            _ => anyhow::bail!("personal-file Shell gate original-value fields conflict"),
        };
        let parsed = Self::new(session_id, original_shell)?;
        if parsed.render() != bytes {
            anyhow::bail!("personal-file Shell gate state is not canonical");
        }
        Ok(parsed)
    }
}

fn personal_restore_shell_gate_path(directory: &Path) -> PathBuf {
    directory.join(PERSONAL_RESTORE_SHELL_GATE_FILE_NAME)
}

fn read_personal_restore_shell_gate(
    directory: &Path,
    session_id: &str,
) -> Result<PersonalRestoreShellGate> {
    let path = personal_restore_shell_gate_path(directory);
    let metadata = std::fs::symlink_metadata(&path)
        .with_context(|| format!("inspect personal-file Shell gate state {}", path.display()))?;
    if !metadata.is_file()
        || metadata_is_reparse_point(&metadata)
        || metadata.len() == 0
        || metadata.len() > PERSONAL_RESTORE_SHELL_GATE_MAX_BYTES
    {
        anyhow::bail!("personal-file Shell gate state is not an ordinary bounded file");
    }
    let state = PersonalRestoreShellGate::parse(&std::fs::read(&path)?)?;
    if state.session_id != session_id {
        anyhow::bail!("personal-file Shell gate state does not match this session");
    }
    Ok(state)
}

fn personal_restore_launcher_path(directory: &Path) -> Result<PathBuf> {
    directory
        .parent()
        .map(|root| root.join(LAUNCHER_FILE_NAME))
        .ok_or_else(|| anyhow::anyhow!("LetRecovery staging directory has no volume root"))
}

fn personal_restore_shell_command(directory: &Path, session_id: &str) -> Result<String> {
    validate_personal_restore_session_id(session_id)?;
    let helper = directory.join(ACCOUNT_HELPER_FILE_NAME);
    let metadata = std::fs::symlink_metadata(&helper).with_context(|| {
        format!(
            "inspect personal-file progress Shell helper {}",
            helper.display()
        )
    })?;
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("personal-file progress Shell helper is not an ordinary file");
    }
    render_personal_restore_shell_command(&helper, session_id)
}

fn render_personal_restore_shell_command(helper: &Path, session_id: &str) -> Result<String> {
    validate_personal_restore_session_id(session_id)?;
    if !helper.is_absolute()
        || helper.file_name().and_then(|name| name.to_str()) != Some(ACCOUNT_HELPER_FILE_NAME)
    {
        anyhow::bail!("personal-file progress Shell helper path is invalid");
    }
    // A console process launched as Winlogon's temporary Shell is not a visibility guarantee on
    // modern Windows: console delegation may expose only a pseudoconsole message-queue HWND. Use
    // the staged native helper itself as the Shell so it owns a real top-level Win32 window on the
    // interactive desktop. The session id is fixed lowercase hexadecimal and the helper path is
    // fixed staging content, so no shell quoting boundary is involved.
    let command = format!(
        r#""{}" --internal-personal-restore-progress-shell {}"#,
        helper.display(),
        session_id
    );
    if command.contains(['\r', '\n']) || command.encode_utf16().count() > 260 {
        anyhow::bail!("personal-file Shell gate command exceeds its supported limit");
    }
    Ok(command)
}

fn first_logon_run_once_command(directory: &Path) -> Result<String> {
    let launcher = personal_restore_launcher_path(directory)?;
    let metadata = std::fs::symlink_metadata(&launcher)
        .with_context(|| format!("inspect first-logon launcher {}", launcher.display()))?;
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("first-logon launcher is not an ordinary file");
    }
    let command_interpreter = crate::windows_compat::system_directory()?.join("cmd.exe");
    let command = format!(
        r#""{}" /d /c "{}""#,
        command_interpreter.display(),
        launcher.display()
    );
    if command.encode_utf16().count() > 260 {
        anyhow::bail!("first-logon RunOnce command exceeds 260 UTF-16 code units");
    }
    Ok(command)
}

fn personal_restore_task_name(session_id: &str) -> Result<String> {
    validate_personal_restore_session_id(session_id)?;
    Ok(format!("LetRecovery Personal Restore {session_id}"))
}

fn personal_restore_task_arguments(launcher: &Path, session_id: &str) -> Result<String> {
    validate_personal_restore_session_id(session_id)?;
    if !launcher.is_absolute()
        || launcher.file_name().and_then(|name| name.to_str()) != Some(LAUNCHER_FILE_NAME)
    {
        anyhow::bail!("personal-file task launcher path is invalid");
    }
    let arguments = format!(r#"/d /c call "{}""#, launcher.display());
    if arguments.contains(['\r', '\n']) || arguments.encode_utf16().count() > 260 {
        anyhow::bail!("personal-file task arguments exceed their supported limit");
    }
    Ok(arguments)
}

fn xml_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn personal_restore_task_xml(
    directory: &Path,
    session_id: &str,
    account_sid: &str,
) -> Result<String> {
    validate_personal_restore_session_id(session_id)?;
    if account_sid.is_empty()
        || account_sid.len() > 184
        || !account_sid.starts_with("S-1-")
        || !account_sid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-' || byte == b'S')
    {
        anyhow::bail!("personal-file restore task account SID is invalid");
    }
    let launcher = personal_restore_launcher_path(directory)?;
    let command_interpreter = crate::windows_compat::system_directory()?.join("cmd.exe");
    let description = "Restore LetRecovery personal files before Windows Explorer starts.";
    let arguments = personal_restore_task_arguments(&launcher, session_id)?;
    let triggers = format!(
        "<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger></Triggers>",
        xml_text(account_sid)
    );
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{description}</Description></RegistrationInfo>
  {triggers}
  <Principals><Principal id="Author"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries><StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><AllowHardTerminate>true</AllowHardTerminate><StartWhenAvailable>false</StartWhenAvailable><RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable><AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled><Hidden>false</Hidden><RunOnlyIfIdle>false</RunOnlyIfIdle><WakeToRun>false</WakeToRun><ExecutionTimeLimit>PT2H</ExecutionTimeLimit><Priority>4</Priority></Settings>
  <Actions Context="Author"><Exec><Command>{command}</Command><Arguments>{arguments}</Arguments></Exec></Actions>
</Task>"#,
        description = xml_text(description),
        triggers = triggers,
        sid = xml_text(account_sid),
        command = xml_text(&command_interpreter.to_string_lossy()),
        arguments = xml_text(&arguments),
    ))
}

#[cfg(windows)]
struct TaskSchedulerComApartment {
    uninitialize: bool,
}

#[cfg(windows)]
impl TaskSchedulerComApartment {
    fn enter() -> Result<Self> {
        use windows::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result.is_ok() {
            return Ok(Self { uninitialize: true });
        }
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self {
                uninitialize: false,
            });
        }
        anyhow::bail!(
            "CoInitializeEx(Task Scheduler) failed: 0x{:08X}",
            result.0 as u32
        )
    }
}

#[cfg(windows)]
impl Drop for TaskSchedulerComApartment {
    fn drop(&mut self) {
        if self.uninitialize {
            unsafe { windows::Win32::System::Com::CoUninitialize() };
        }
    }
}

#[cfg(windows)]
fn connect_task_scheduler() -> Result<(
    TaskSchedulerComApartment,
    windows::Win32::System::TaskScheduler::ITaskFolder,
)> {
    use windows::core::{BSTR, VARIANT};
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
    use windows::Win32::System::TaskScheduler::{ITaskService, TaskScheduler};

    let apartment = TaskSchedulerComApartment::enter()?;
    let service: ITaskService =
        unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }
            .context("CoCreateInstance(CLSID_TaskScheduler)")?;
    let empty = VARIANT::default();
    unsafe { service.Connect(&empty, &empty, &empty, &empty) }
        .context("ITaskService::Connect(local)")?;
    let root_path = BSTR::from(r"\");
    let root = unsafe { service.GetFolder(&root_path) }.context("ITaskService::GetFolder(root)")?;
    Ok((apartment, root))
}

#[cfg(windows)]
fn register_personal_restore_logon_task(session_id: &str, account_name: &str) -> Result<()> {
    use windows::core::{BSTR, VARIANT};
    use windows::Win32::System::TaskScheduler::{
        TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN,
    };

    let directory = personal_restore_state_directory()?;
    let account_sid = crate::windows_accounts::local_account_sid_string(account_name)
        .map_err(anyhow::Error::from)?;
    let (_apartment, root) = connect_task_scheduler()?;
    let user = VARIANT::from(BSTR::from(account_sid.as_str()));
    let empty = VARIANT::default();
    // TaskFolder::RegisterTask is available from Vista. Microsoft defines logon type 3 as an
    // already-existing interactive token and explicitly warns that a group passed with type 3
    // registers but never runs. Bind both tasks to the exact local-user SID with no stored
    // password. Winlogon owns the ordinary-token visible Shell helper; this
    // HighestAvailable task only performs the privileged restore work without a UAC prompt.
    let task_name = personal_restore_task_name(session_id)?;
    let xml = personal_restore_task_xml(&directory, session_id, &account_sid)?;
    let task_name_bstr = BSTR::from(task_name.as_str());
    let xml_bstr = BSTR::from(xml.as_str());
    let registered = unsafe {
        root.RegisterTask(
            &task_name_bstr,
            &xml_bstr,
            TASK_CREATE_OR_UPDATE.0,
            &user,
            &empty,
            TASK_LOGON_INTERACTIVE_TOKEN,
            &empty,
        )
    }
    .context("ITaskFolder::RegisterTask(personal restore)")?;
    let expected_path = BSTR::from(format!(r"\{task_name}"));
    if unsafe { registered.Path() }? != expected_path || unsafe { registered.Enabled() }?.0 == 0 {
        anyhow::bail!("personal-file restore task registration readback mismatch");
    }
    let definition = unsafe { registered.Definition() }?;
    let mut actual_xml = BSTR::new();
    unsafe { definition.XmlText(&mut actual_xml) }?;
    let actual_xml = actual_xml.to_string();
    for required in [
        account_sid.as_str(),
        "InteractiveToken",
        "HighestAvailable",
        LAUNCHER_FILE_NAME,
    ] {
        if !actual_xml.contains(required) {
            anyhow::bail!("personal-file restore task XML readback omitted {required}");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn register_personal_restore_logon_task(_session_id: &str, _account_name: &str) -> Result<()> {
    anyhow::bail!("Task Scheduler is unavailable on this platform")
}

#[cfg(windows)]
fn delete_personal_restore_logon_task(session_id: &str) -> Result<()> {
    use windows::core::BSTR;

    let (_apartment, root) = connect_task_scheduler()?;
    let task_name = personal_restore_task_name(session_id)?;
    let task_name_bstr = BSTR::from(task_name.as_str());
    unsafe { root.DeleteTask(&task_name_bstr, 0) }
        .context("ITaskFolder::DeleteTask(personal restore)")?;
    if unsafe { root.GetTask(&task_name_bstr) }.is_ok() {
        anyhow::bail!("personal-file restore task survived deletion");
    }
    Ok(())
}

#[cfg(not(windows))]
fn delete_personal_restore_logon_task(_session_id: &str) -> Result<()> {
    anyhow::bail!("Task Scheduler is unavailable on this platform")
}

/// Activate the one-logon Shell gate only after OOBE has reached FirstLogonCommands. Microsoft
/// explicitly does not support setting a custom shell before OOBE on Windows 10. The original
/// value is durably captured before the registry write and is restored byte-for-byte after the
/// personal-file receipt has been committed and read back.
pub fn activate_personal_restore_shell_gate(session_id: &str) -> Result<()> {
    validate_personal_restore_session_id(session_id)?;
    let directory = personal_restore_state_directory()?;
    let state_path = personal_restore_shell_gate_path(&directory);
    let command = personal_restore_shell_command(&directory, session_id)?;
    let state = match std::fs::symlink_metadata(&state_path) {
        Ok(_) => read_personal_restore_shell_gate(&directory, session_id)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let original_shell =
                crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "Shell")?;
            let state = PersonalRestoreShellGate::new(session_id, original_shell)?;
            let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
                &directory,
                "lr-personal-restore-shell-gate",
                "state",
                &state.render(),
            )?;
            crate::scoped_temp_file::atomic_publish_new_path(temporary.path(), &state_path)?;
            if read_personal_restore_shell_gate(&directory, session_id)? != state {
                anyhow::bail!("personal-file Shell gate state readback mismatch");
            }
            state
        }
        Err(error) => return Err(error.into()),
    };
    let current = crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "Shell")?;
    if current.as_deref() != Some(command.as_str()) {
        if current != state.original_shell {
            anyhow::bail!("refusing to replace a concurrently changed Winlogon Shell value");
        }
        crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "Shell", &command)?;
    }
    if crate::registry::OfflineRegistry::query_string(WINLOGON_KEY, "Shell")? != command {
        anyhow::bail!("personal-file Shell gate registry readback mismatch");
    }
    for stale in [
        directory.join(PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME),
        directory.join(PERSONAL_RESTORE_SHELL_VERIFIED_FILE_NAME),
        directory.join(PERSONAL_RESTORE_FAILURE_FILE_NAME),
    ] {
        match std::fs::symlink_metadata(&stale) {
            Ok(metadata) if metadata.is_file() && !metadata_is_reparse_point(&metadata) => {
                std::fs::remove_file(&stale)?;
            }
            Ok(_) => anyhow::bail!("personal-file Shell gate stale state is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Ordinary unattended accounts have already consumed their only AutoLogon entry by the time
/// FirstLogonCommands runs. Arm the gate and a SID-bound highest-interactive logon task for the
/// next logon, then immediately restart while Windows is still withholding the first desktop.
pub fn begin_personal_restore_second_logon(session_id: &str) -> Result<()> {
    activate_personal_restore_shell_gate(session_id)?;
    let account =
        crate::windows_accounts::current_local_account_name().map_err(anyhow::Error::from)?;
    register_personal_restore_logon_task(session_id, &account)?;
    crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "DefaultUserName", &account)?;
    crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "DefaultPassword", "")?;
    crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "AutoAdminLogon", "1")?;
    crate::registry::OfflineRegistry::set_dword(WINLOGON_KEY, "AutoLogonCount", 1)?;
    if crate::registry::OfflineRegistry::query_string(WINLOGON_KEY, "DefaultUserName")? != account
        || !crate::registry::OfflineRegistry::query_string(WINLOGON_KEY, "DefaultPassword")?
            .is_empty()
        || crate::registry::OfflineRegistry::query_string(WINLOGON_KEY, "AutoAdminLogon")? != "1"
        || crate::registry::OfflineRegistry::query_dword(WINLOGON_KEY, "AutoLogonCount")? != 1
    {
        anyhow::bail!("personal-file second-logon registry readback mismatch");
    }
    append_first_logon_line(
        "Personal files restore: Shell gate armed for ordinary account second logon",
    )?;
    // This is the same bounded pre-desktop transition as the RID-500 path above.  A graceful
    // interactive restart would let an unrelated first-logon helper (for example rgnupdt.exe)
    // hold Winlogon on the "apps are preventing restart" screen indefinitely.
    crate::windows_shutdown::schedule_restart_for_automation(
        0,
        "LetRecovery is restarting once so personal files can be restored before Windows Explorer starts.",
    )
}

fn stage_personal_restore_marker(
    directory: &Path,
    personal_restore_session_id: Option<&str>,
) -> Result<()> {
    let pending = directory.join(PERSONAL_RESTORE_PENDING_FILE_NAME);
    let receipt = directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME);
    let lease = directory.join(PERSONAL_RESTORE_LOCK_FILE_NAME);
    let shell_gate = directory.join(PERSONAL_RESTORE_SHELL_GATE_FILE_NAME);
    let shell_released = directory.join(PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME);
    let shell_verified = directory.join(PERSONAL_RESTORE_SHELL_VERIFIED_FILE_NAME);
    let failure = directory.join(PERSONAL_RESTORE_FAILURE_FILE_NAME);
    for stale in [
        &pending,
        &receipt,
        &lease,
        &shell_gate,
        &shell_released,
        &shell_verified,
        &failure,
    ] {
        match std::fs::symlink_metadata(stale) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
                    anyhow::bail!(
                        "personal-file restore state is not a regular file: {}",
                        stale.display()
                    );
                }
                std::fs::remove_file(stale)
                    .with_context(|| format!("remove stale restore state {}", stale.display()))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let Some(session_id) = personal_restore_session_id else {
        return Ok(());
    };
    validate_personal_restore_session_id(session_id)?;
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-personal-restore-pending",
        "state",
        session_id.as_bytes(),
    )?;
    temporary.persist_replace(&pending)?;
    if std::fs::read_to_string(&pending)? != session_id {
        anyhow::bail!("personal-file restore pending marker readback mismatch");
    }
    Ok(())
}

fn personal_restore_state_directory() -> Result<PathBuf> {
    let executable = std::env::current_exe().context("resolve current restore helper")?;
    let directory = executable
        .parent()
        .ok_or_else(|| anyhow::anyhow!("restore helper has no parent directory"))?
        .to_path_buf();
    reject_reparse_or_non_directory(&directory)?;
    Ok(directory)
}

#[cfg(windows)]
fn current_session_shell_process_id() -> Result<Option<u32>> {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

    let current_process_id = unsafe { GetCurrentProcessId() };
    let mut current_session_id = 0_u32;
    unsafe { ProcessIdToSessionId(current_process_id, &mut current_session_id) }
        .context("ProcessIdToSessionId(personal restore Shell gate)")?;
    let shell_window = unsafe { GetShellWindow() };
    if shell_window.0.is_null() {
        return Ok(None);
    }
    let mut shell_process_id = 0_u32;
    if unsafe { GetWindowThreadProcessId(shell_window, Some(&mut shell_process_id)) } == 0
        || shell_process_id == 0
    {
        return Ok(None);
    }
    let mut shell_session_id = 0_u32;
    if unsafe { ProcessIdToSessionId(shell_process_id, &mut shell_session_id) }.is_err()
        || shell_session_id != current_session_id
    {
        return Ok(None);
    }
    Ok(Some(shell_process_id))
}

#[cfg(windows)]
#[derive(Clone, Debug, Eq, PartialEq)]
enum PersonalRestoreProgressPhase {
    Restoring,
    Verifying,
    StartingDesktop,
    Failed(String),
}

#[cfg(windows)]
struct PersonalRestoreProgressWindow {
    directory: PathBuf,
    session_id: String,
    phase: PersonalRestoreProgressPhase,
    animation_tick: u32,
    dpi: u32,
    preview: bool,
    background: windows::Win32::Graphics::Gdi::HBRUSH,
    title_font: windows::Win32::Graphics::Gdi::HFONT,
}

#[cfg(windows)]
fn personal_restore_progress_window_style() -> windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE
{
    use windows::Win32::UI::WindowsAndMessaging::{WS_POPUP, WS_VISIBLE};
    // `ShowWindow` may ignore its first command when the launching program supplied STARTUPINFO.
    // Creating the native Shell window with WS_VISIBLE makes visibility part of the window's own
    // creation contract instead of inheriting Winlogon's show state.
    WS_POPUP | WS_VISIBLE
}

#[cfg(windows)]
impl Drop for PersonalRestoreProgressWindow {
    fn drop(&mut self) {
        use windows::Win32::Graphics::Gdi::DeleteObject;
        unsafe {
            if !self.background.is_invalid() {
                let _ = DeleteObject(self.background);
            }
            if !self.title_font.is_invalid() {
                let _ = DeleteObject(self.title_font);
            }
        }
    }
}

#[cfg(windows)]
fn progress_shell_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn read_progress_shell_file(path: &Path, maximum_bytes: u64) -> Result<Option<String>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file()
                || metadata_is_reparse_point(&metadata)
                || metadata.len() == 0
                || metadata.len() > maximum_bytes
            {
                anyhow::bail!(
                    "personal-file progress Shell state is not an ordinary bounded file: {}",
                    path.display()
                );
            }
            let bytes = std::fs::read(path)
                .with_context(|| format!("read progress Shell state {}", path.display()))?;
            Ok(Some(
                std::str::from_utf8(&bytes)
                    .with_context(|| format!("decode progress Shell state {}", path.display()))?
                    .to_owned(),
            ))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn poll_personal_restore_progress_shell(state: &mut PersonalRestoreProgressWindow) -> Result<bool> {
    if let Some(failure) = read_progress_shell_file(
        &state.directory.join(PERSONAL_RESTORE_FAILURE_FILE_NAME),
        128,
    )? {
        if failure != state.session_id {
            anyhow::bail!("personal-file progress Shell failure marker belongs to another session");
        }
        state.phase = PersonalRestoreProgressPhase::Failed(
            "个人文件恢复未完成，请重新启动 Windows 后重试。".to_owned(),
        );
        return Ok(false);
    }

    let completed = read_progress_shell_file(
        &state.directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME),
        128,
    )?;
    let released = read_progress_shell_file(
        &state
            .directory
            .join(PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME),
        128,
    )?;
    let verified = read_progress_shell_file(
        &state
            .directory
            .join(PERSONAL_RESTORE_SHELL_VERIFIED_FILE_NAME),
        256,
    )?;
    for (name, value) in [("completion", &completed), ("release", &released)] {
        if let Some(value) = value {
            if value != &state.session_id {
                anyhow::bail!("personal-file progress Shell {name} receipt mismatch");
            }
        }
    }

    state.phase = if completed.is_none() {
        PersonalRestoreProgressPhase::Restoring
    } else if released.is_none() {
        PersonalRestoreProgressPhase::Verifying
    } else {
        PersonalRestoreProgressPhase::StartingDesktop
    };
    let Some(verified) = verified else {
        return Ok(false);
    };
    let Some(pid) = verified.strip_prefix(&format!("{}:", state.session_id)) else {
        anyhow::bail!("personal-file progress Shell verified receipt mismatch");
    };
    if pid.is_empty()
        || pid.starts_with('0')
        || !pid.bytes().all(|byte| byte.is_ascii_digit())
        || pid.parse::<u32>().ok().filter(|pid| *pid != 0).is_none()
    {
        anyhow::bail!("personal-file progress Shell verified receipt has an invalid process id");
    }
    Ok(completed.is_some() && released.is_some())
}

#[cfg(windows)]
unsafe fn draw_progress_shell_text(
    dc: windows::Win32::Graphics::Gdi::HDC,
    font: windows::Win32::Graphics::Gdi::HFONT,
    color: windows::Win32::Foundation::COLORREF,
    text: &str,
    mut rect: windows::Win32::Foundation::RECT,
) {
    use windows::Win32::Graphics::Gdi::{
        DrawTextW, SelectObject, SetBkMode, SetTextColor, DT_CENTER, DT_NOPREFIX, DT_SINGLELINE,
        DT_VCENTER, TRANSPARENT,
    };
    let old_font = SelectObject(dc, font);
    let _ = SetBkMode(dc, TRANSPARENT);
    let _ = SetTextColor(dc, color);
    let mut text = progress_shell_wide(text);
    let _ = DrawTextW(
        dc,
        &mut text,
        &mut rect,
        DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
    );
    let _ = SelectObject(dc, old_font);
}

#[cfg(windows)]
fn personal_restore_progress_percent(
    phase: &PersonalRestoreProgressPhase,
    animation_tick: u32,
) -> u8 {
    if matches!(phase, PersonalRestoreProgressPhase::Failed(_)) {
        return 100;
    }
    let (minimum, maximum) = match phase {
        PersonalRestoreProgressPhase::Restoring => (14_u32, 70_u32),
        PersonalRestoreProgressPhase::Verifying => (74, 88),
        PersonalRestoreProgressPhase::StartingDesktop => (92, 98),
        PersonalRestoreProgressPhase::Failed(_) => unreachable!(),
    };
    let distance = maximum - minimum;
    let cycle = distance.saturating_mul(2).max(1);
    let position = (animation_tick / 2) % cycle;
    let offset = if position <= distance {
        position
    } else {
        cycle - position
    };
    (minimum + offset) as u8
}

#[cfg(windows)]
fn personal_restore_progress_layout(
    client: windows::Win32::Foundation::RECT,
    dpi: u32,
) -> (
    windows::Win32::Foundation::RECT,
    windows::Win32::Foundation::RECT,
) {
    use windows::Win32::Foundation::RECT;

    let scaled = |value: i32| {
        let dpi = i64::from(dpi.max(96));
        ((i64::from(value) * dpi + 48) / 96).clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    let width = (client.right - client.left).max(1);
    let height = (client.bottom - client.top).max(1);
    let center = client.top + height / 2;
    let bar_width = (width * 42 / 100)
        .clamp(scaled(280), scaled(560))
        .min(width);
    let bar_left = client.left + (width - bar_width) / 2;
    (
        RECT {
            left: client.left + scaled(40),
            top: center - scaled(50),
            right: client.right - scaled(40),
            bottom: center - scaled(12),
        },
        RECT {
            left: bar_left,
            top: center + scaled(14),
            right: bar_left + bar_width,
            bottom: center + scaled(24),
        },
    )
}

#[cfg(windows)]
fn create_personal_restore_progress_font(dpi: u32) -> windows::Win32::Graphics::Gdi::HFONT {
    use windows::core::PCWSTR;
    use windows::Win32::Graphics::Gdi::CreateFontW;

    let face = progress_shell_wide("Microsoft YaHei UI");
    let height = -((24_i64 * i64::from(dpi.max(96)) + 48) / 96) as i32;
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            400,
            0,
            0,
            0,
            1,
            0,
            0,
            5,
            0,
            PCWSTR(face.as_ptr()),
        )
    }
}

#[cfg(windows)]
fn progress_shell_rects_intersect(
    left: windows::Win32::Foundation::RECT,
    right: windows::Win32::Foundation::RECT,
) -> bool {
    left.left < right.right
        && left.right > right.left
        && left.top < right.bottom
        && left.bottom > right.top
}

#[cfg(windows)]
unsafe fn draw_personal_restore_progress_bar(
    dc: windows::Win32::Graphics::Gdi::HDC,
    rect: windows::Win32::Foundation::RECT,
    percent: u8,
) {
    use windows::Win32::Graphics::Gdi::{
        StretchDIBits, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, SRCCOPY,
    };

    let width = (rect.right - rect.left).max(0);
    let height = (rect.bottom - rect.top).max(0);
    if width == 0 || height == 0 {
        return;
    }
    let pixels = crate::progress_raster::render_rounded_progress_bgra(
        width,
        height,
        percent,
        crate::progress_raster::RoundedProgressPalette::PE_DARK,
    );
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            biSizeImage: (width * height * 4) as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let _ = StretchDIBits(
        dc,
        rect.left,
        rect.top,
        width,
        height,
        0,
        0,
        width,
        height,
        Some(pixels.as_ptr().cast()),
        &info,
        DIB_RGB_COLORS,
        SRCCOPY,
    );
}

#[cfg(windows)]
unsafe fn paint_personal_restore_progress_surface(
    state: &PersonalRestoreProgressWindow,
    dc: windows::Win32::Graphics::Gdi::HDC,
    client: windows::Win32::Foundation::RECT,
    paint_rect: windows::Win32::Foundation::RECT,
    offset_x: i32,
    offset_y: i32,
) {
    use windows::Win32::Foundation::{COLORREF, RECT};
    use windows::Win32::Graphics::Gdi::FillRect;

    let _ = FillRect(dc, &paint_rect, state.background);
    let dirty_client = RECT {
        left: paint_rect.left - offset_x,
        top: paint_rect.top - offset_y,
        right: paint_rect.right - offset_x,
        bottom: paint_rect.bottom - offset_y,
    };
    let (title_rect, bar_rect) = personal_restore_progress_layout(client, state.dpi);
    let title = match &state.phase {
        PersonalRestoreProgressPhase::Failed(_) => "恢复失败",
        _ => "正在恢复个人文件",
    };
    if progress_shell_rects_intersect(dirty_client, title_rect) {
        draw_progress_shell_text(
            dc,
            state.title_font,
            COLORREF(0x00ff_ffff),
            title,
            RECT {
                left: title_rect.left + offset_x,
                top: title_rect.top + offset_y,
                right: title_rect.right + offset_x,
                bottom: title_rect.bottom + offset_y,
            },
        );
    }
    if progress_shell_rects_intersect(dirty_client, bar_rect) {
        draw_personal_restore_progress_bar(
            dc,
            RECT {
                left: bar_rect.left + offset_x,
                top: bar_rect.top + offset_y,
                right: bar_rect.right + offset_x,
                bottom: bar_rect.bottom + offset_y,
            },
            personal_restore_progress_percent(&state.phase, state.animation_tick),
        );
    }
}

#[cfg(windows)]
unsafe extern "system" fn personal_restore_progress_window_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::{HWND, LRESULT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        EndPaint, InvalidateRect, SelectObject, PAINTSTRUCT, SRCCOPY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, DestroyWindow, GetClientRect, GetWindowLongPtrW, KillTimer,
        PostQuitMessage, SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW, GWLP_USERDATA,
        SWP_NOACTIVATE, SWP_NOZORDER, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
        WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_TIMER,
    };

    if message == WM_NCCREATE {
        let create = &*(lparam.0 as *const CREATESTRUCTW);
        let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
    }
    let state =
        (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut PersonalRestoreProgressWindow).as_mut();
    match message {
        WM_TIMER => {
            if let Some(state) = state {
                state.animation_tick = state.animation_tick.wrapping_add(1);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let (_, bar) = personal_restore_progress_layout(client, state.dpi);
                if state.preview {
                    let _ = InvalidateRect(hwnd, Some(&bar), false);
                    return LRESULT(0);
                }
                let failed_before = matches!(state.phase, PersonalRestoreProgressPhase::Failed(_));
                match poll_personal_restore_progress_shell(state) {
                    Ok(true) => {
                        let _ = KillTimer(hwnd, 1);
                        let _ = DestroyWindow(hwnd);
                    }
                    Ok(false) => {
                        let failed_after =
                            matches!(state.phase, PersonalRestoreProgressPhase::Failed(_));
                        let _ = InvalidateRect(
                            hwnd,
                            if failed_before == failed_after {
                                Some(&bar)
                            } else {
                                None
                            },
                            false,
                        );
                    }
                    Err(error) => {
                        state.phase = PersonalRestoreProgressPhase::Failed(format!(
                            "恢复状态验证失败：{error:#}"
                        ));
                        let _ = InvalidateRect(hwnd, None, false);
                    }
                }
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            if let Some(state) = state {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut client = RECT::default();
                let _ = GetClientRect(hwnd, &mut client);
                let width = (paint.rcPaint.right - paint.rcPaint.left).max(0);
                let height = (paint.rcPaint.bottom - paint.rcPaint.top).max(0);
                if width > 0 && height > 0 {
                    // Match the PE progress window: compose the complete dirty rectangle off-screen
                    // and expose it with one SRCCOPY. Direct FillRect + StretchDIBits can reveal the
                    // intermediate background frame, which is especially visible on a 250 ms timer.
                    let memory_dc = CreateCompatibleDC(dc);
                    let bitmap = CreateCompatibleBitmap(dc, width, height);
                    if !memory_dc.is_invalid() && !bitmap.is_invalid() {
                        let old_bitmap = SelectObject(memory_dc, bitmap);
                        paint_personal_restore_progress_surface(
                            state,
                            memory_dc,
                            client,
                            RECT {
                                left: 0,
                                top: 0,
                                right: width,
                                bottom: height,
                            },
                            -paint.rcPaint.left,
                            -paint.rcPaint.top,
                        );
                        let _ = BitBlt(
                            dc,
                            paint.rcPaint.left,
                            paint.rcPaint.top,
                            width,
                            height,
                            memory_dc,
                            0,
                            0,
                            SRCCOPY,
                        );
                        let _ = SelectObject(memory_dc, old_bitmap);
                    } else {
                        // Resource exhaustion must not leave the progress surface unpainted.
                        paint_personal_restore_progress_surface(
                            state,
                            dc,
                            client,
                            paint.rcPaint,
                            0,
                            0,
                        );
                    }
                    if !bitmap.is_invalid() {
                        let _ = DeleteObject(bitmap);
                    }
                    if !memory_dc.is_invalid() {
                        let _ = DeleteDC(memory_dc);
                    }
                }
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, message, wparam, lparam)
            }
        }
        WM_CLOSE => {
            if state.is_some_and(|state| state.preview) {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(state) = state {
                let suggested = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    HWND::default(),
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOACTIVATE | SWP_NOZORDER,
                );
                let dpi = crate::windows_compat::dpi_for_window(hwnd).max(96);
                let font = create_personal_restore_progress_font(dpi);
                if !font.is_invalid() {
                    use windows::Win32::Graphics::Gdi::DeleteObject;
                    let old_font = std::mem::replace(&mut state.title_font, font);
                    if !old_font.is_invalid() {
                        let _ = DeleteObject(old_font);
                    }
                    state.dpi = dpi;
                }
                let _ = InvalidateRect(hwnd, None, false);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, 1);
            PostQuitMessage(0);
            LRESULT(0)
        }
        WM_NCDESTROY => {
            let _ = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

#[cfg(windows)]
fn show_personal_restore_progress_fallback(error: &anyhow::Error) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let message = progress_shell_wide(&format!(
        "LetRecovery 无法创建个人文件恢复进度窗口。\r\n\r\n{error:#}\r\n\r\n请重新启动 Windows 后重试。"
    ));
    let title = progress_shell_wide("LetRecovery 个人文件恢复");
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR | MB_TOPMOST | MB_SETFOREGROUND,
        );
    }
}

#[cfg(windows)]
fn run_personal_restore_progress_window(
    directory: PathBuf,
    session_id: String,
    preview: bool,
) -> Result<()> {
    use std::mem::size_of;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND};
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DispatchMessageW, GetMessageW, GetSystemMetrics, LoadCursorW,
        RegisterClassExW, SetForegroundWindow, SetTimer, ShowWindow, TranslateMessage, CS_HREDRAW,
        CS_VREDRAW, HMENU, IDC_ARROW, MSG, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, WNDCLASSEXW,
        WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    // This helper is a fresh process in production and preview routes. Establish per-monitor-v2
    // awareness before creating any HWND; Windows 7 uses the shared system-DPI-aware fallback.
    let _ = crate::windows_compat::enable_best_process_dpi_awareness();
    let initial_dpi = crate::windows_compat::dpi_for_system().max(96);
    let instance = unsafe { GetModuleHandleW(None) }.context("GetModuleHandleW(progress Shell)")?;
    let class_name = w!("LetRecovery.PersonalRestoreProgressShell");
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(personal_restore_progress_window_proc),
        hInstance: HINSTANCE(instance.0),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.context("LoadCursorW(progress Shell)")?,
        lpszClassName: class_name,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err(windows::core::Error::from_win32()).context("RegisterClassExW(progress Shell)");
    }
    let mut state = Box::new(PersonalRestoreProgressWindow {
        directory,
        session_id,
        phase: PersonalRestoreProgressPhase::Restoring,
        animation_tick: 0,
        dpi: initial_dpi,
        preview,
        background: unsafe { CreateSolidBrush(COLORREF(0x002b_2b2b)) },
        title_font: create_personal_restore_progress_font(initial_dpi),
    });
    if state.background.is_invalid() || state.title_font.is_invalid() {
        anyhow::bail!("create progress Shell GDI resources");
    }
    let title = progress_shell_wide("LetRecovery 恢复预览");
    let screen_width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(1);
    let screen_height = unsafe { GetSystemMetrics(SM_CYSCREEN) }.max(1);
    let (ex_style, style, x, y, width, height) = if preview {
        let scale = |value: i32| {
            ((i64::from(value) * i64::from(initial_dpi) + 48) / 96).clamp(1, i64::from(i32::MAX))
                as i32
        };
        let width = scale(900).min(screen_width);
        let height = scale(560).min(screen_height);
        (
            WS_EX_APPWINDOW,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            (screen_width - width) / 2,
            (screen_height - height) / 2,
            width,
            height,
        )
    } else {
        (
            WS_EX_TOPMOST | WS_EX_APPWINDOW,
            personal_restore_progress_window_style(),
            0,
            0,
            screen_width,
            screen_height,
        )
    };
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            class_name,
            PCWSTR(title.as_ptr()),
            style,
            x,
            y,
            width,
            height,
            HWND::default(),
            HMENU::default(),
            HINSTANCE(instance.0),
            Some((&mut *state as *mut PersonalRestoreProgressWindow).cast()),
        )
    }
    .context("CreateWindowExW(progress Shell)")?;
    if unsafe { SetTimer(hwnd, 1, 250, None) } == 0 {
        // The production window itself is already visible. Keep a permanent failure page rather
        // than destroying the only Shell UI and falling back to a black desktop.
        state.phase = PersonalRestoreProgressPhase::Failed(format!(
            "progress timer failed: {}",
            std::io::Error::last_os_error()
        ));
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
        }
    }
    unsafe {
        // The window is already WS_VISIBLE. Call twice as an additional defence because the first
        // command may be replaced by Winlogon's STARTUPINFO show state.
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    let mut message = MSG::default();
    loop {
        let status = unsafe { GetMessageW(&mut message, None, 0, 0) };
        if status.0 == -1 {
            return Err(windows::core::Error::from_win32()).context("GetMessageW(progress Shell)");
        }
        if status.0 == 0 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    Ok(())
}

/// Run the ordinary-user native progress Shell while the exact SID-bound highest-interactive task
/// restores personal files. This window only reads authenticated state; all writes, Shell release,
/// Explorer startup and verification remain owned by the privileged worker.
#[cfg(windows)]
pub fn run_personal_restore_progress_shell(session_id: &str) -> Result<()> {
    let result = (|| {
        validate_personal_restore_session_id(session_id)?;
        let directory = personal_restore_state_directory()?;
        let _ = read_personal_restore_shell_gate(&directory, session_id)?;
        run_personal_restore_progress_window(directory, session_id.to_owned(), false)
    })();
    if let Err(error) = &result {
        show_personal_restore_progress_fallback(error);
    }
    result
}

/// Side-effect-free, closable desktop preview of the exact first-logon progress renderer.
#[cfg(windows)]
pub fn run_personal_restore_progress_preview() -> Result<()> {
    let result = run_personal_restore_progress_window(PathBuf::new(), String::new(), true);
    if let Err(error) = &result {
        show_personal_restore_progress_fallback(error);
    }
    result
}

#[cfg(not(windows))]
pub fn run_personal_restore_progress_shell(_session_id: &str) -> Result<()> {
    anyhow::bail!("the personal-file native progress Shell is available only on Windows")
}

#[cfg(not(windows))]
pub fn run_personal_restore_progress_preview() -> Result<()> {
    anyhow::bail!("the personal-file native progress preview is available only on Windows")
}

/// Start Explorer from the already-running highest-available interactive restore task and prove
/// that Windows has published a stable Shell window in that same user session before the native
/// progress gate exits. The gate remains read-only; every receipt and diagnostic write stays here.
#[cfg(windows)]
pub fn start_personal_restore_explorer(session_id: &str) -> Result<()> {
    validate_personal_restore_session_id(session_id)?;
    let directory = personal_restore_state_directory()?;
    if !read_personal_restore_state(
        &directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME),
        session_id,
    )? || !read_personal_restore_state(
        &directory.join(PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME),
        session_id,
    )? {
        anyhow::bail!("personal-file restore Shell release receipts are incomplete");
    }
    let system_directory = crate::windows_compat::system_directory()
        .context("resolve the Windows system directory for Explorer release")?;
    let windows_directory = system_directory
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Windows system directory has no parent"))?;
    let explorer = windows_directory.join("explorer.exe");
    let metadata = std::fs::symlink_metadata(&explorer)
        .with_context(|| format!("inspect Windows Explorer {}", explorer.display()))?;
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("Windows Explorer is not an ordinary file");
    }
    if let Some(shell_process_id) = current_session_shell_process_id()? {
        write_personal_restore_shell_verified_receipt(&directory, session_id, shell_process_id)?;
        return Ok(());
    }
    let child = std::process::Command::new(&explorer)
        .spawn()
        .with_context(|| format!("start Windows Explorer {}", explorer.display()))?;
    let started_process_id = child.id();
    drop(child);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut stable: Option<(u32, std::time::Instant)> = None;
    while std::time::Instant::now() < deadline {
        match current_session_shell_process_id()? {
            Some(process_id) => match stable {
                Some((stable_id, since))
                    if stable_id == process_id
                        && since.elapsed() >= std::time::Duration::from_secs(2) =>
                {
                    write_personal_restore_shell_verified_receipt(
                        &directory, session_id, process_id,
                    )?;
                    return Ok(());
                }
                Some((stable_id, _)) if stable_id == process_id => {}
                _ => stable = Some((process_id, std::time::Instant::now())),
            },
            None => stable = None,
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    anyhow::bail!(
        "Windows Explorer process {started_process_id} did not publish a stable current-session Shell window within 60 seconds"
    )
}

#[cfg(not(windows))]
pub fn start_personal_restore_explorer(_session_id: &str) -> Result<()> {
    anyhow::bail!("personal-file Explorer Shell release is available only on Windows")
}

fn write_personal_restore_shell_verified_receipt(
    directory: &Path,
    session_id: &str,
    shell_process_id: u32,
) -> Result<()> {
    validate_personal_restore_session_id(session_id)?;
    if shell_process_id == 0 {
        anyhow::bail!("verified personal-file Explorer Shell process id is zero");
    }
    let content = format!("{session_id}:{shell_process_id}");
    let path = directory.join(PERSONAL_RESTORE_SHELL_VERIFIED_FILE_NAME);
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-personal-restore-shell-verified",
        "receipt",
        content.as_bytes(),
    )?;
    temporary.persist_replace(&path)?;
    if std::fs::read_to_string(&path)? != content {
        anyhow::bail!("verified personal-file Explorer Shell receipt readback mismatch");
    }
    Ok(())
}

fn read_personal_restore_state(path: &Path, session_id: &str) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
                anyhow::bail!(
                    "personal-file restore state is not a regular file: {}",
                    path.display()
                );
            }
            let actual = std::fs::read_to_string(path)
                .with_context(|| format!("read restore state {}", path.display()))?;
            if actual != session_id {
                anyhow::bail!("personal-file restore state does not match this session");
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn read_personal_restore_authorization(directory: &Path, session_id: &str) -> Result<(bool, bool)> {
    let pending_matches = read_personal_restore_state(
        &directory.join(PERSONAL_RESTORE_PENDING_FILE_NAME),
        session_id,
    )?;
    let receipt_matches = read_personal_restore_state(
        &directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME),
        session_id,
    )?;
    if !pending_matches && !receipt_matches {
        anyhow::bail!("personal-file restore pending marker is missing");
    }
    Ok((pending_matches, receipt_matches))
}

fn explorer_restore_command(launcher: &Path) -> Result<String> {
    if !launcher.is_absolute()
        || launcher.file_name().and_then(|name| name.to_str()) != Some(LAUNCHER_FILE_NAME)
    {
        anyhow::bail!("personal-file restore launcher path is invalid");
    }
    let metadata = std::fs::symlink_metadata(launcher)
        .with_context(|| format!("inspect restore launcher {}", launcher.display()))?;
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("personal-file restore launcher is not a regular file");
    }
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("SystemRoot is unavailable"))?;
    let command_interpreter = system_root.join("System32").join("cmd.exe");
    let command = format!(
        "\"{}\" /d /s /c \"\"{}\" restore\"",
        command_interpreter.display(),
        launcher.display()
    );
    // Microsoft documents a 260-character command-line limit for Run/RunOnce values.
    if command.encode_utf16().count() > 260 {
        anyhow::bail!("personal-file restore Run command exceeds 260 UTF-16 code units");
    }
    Ok(command)
}

/// Register the fixed root launcher for the current user's Explorer initialization. This runs
/// after unattend FirstLogonCommands, which Microsoft documents as completing before the desktop
/// appears. A transient `Run` value is retained on failure and removed only after restore success.
pub fn register_personal_restore_at_shell(session_id: &str, launcher: &Path) -> Result<()> {
    validate_personal_restore_session_id(session_id)?;
    let directory = personal_restore_state_directory()?;
    if !read_personal_restore_state(
        &directory.join(PERSONAL_RESTORE_PENDING_FILE_NAME),
        session_id,
    )? {
        anyhow::bail!("personal-file restore pending marker is missing");
    }
    let command = explorer_restore_command(launcher)?;
    crate::registry::OfflineRegistry::set_string(
        PERSONAL_RESTORE_RUN_KEY,
        PERSONAL_RESTORE_RUN_VALUE,
        &command,
    )?;
    let actual = crate::registry::OfflineRegistry::query_string(
        PERSONAL_RESTORE_RUN_KEY,
        PERSONAL_RESTORE_RUN_VALUE,
    )?;
    if actual != command {
        anyhow::bail!("personal-file restore Run registration readback mismatch");
    }
    Ok(())
}

/// Restore at Explorer initialization and retire only the exact Run value registered above.
/// A durable receipt makes cleanup retryable if registry deletion fails after files were moved.
pub fn restore_personal_files_at_shell(
    session_id: &str,
) -> Result<Option<crate::personal_files::PersonalFileRestoreReport>> {
    let profile_flags = current_user_profile_type_flags()?;
    ensure_persistent_current_user_profile(profile_flags)?;
    restore_personal_files_at_shell_with_persistent_profile(session_id)
}

fn restore_personal_files_at_shell_with_persistent_profile(
    session_id: &str,
) -> Result<Option<crate::personal_files::PersonalFileRestoreReport>> {
    validate_personal_restore_session_id(session_id)?;
    let directory = personal_restore_state_directory()?;
    let _lease = acquire_personal_restore_lease(&directory)?;
    let pending = directory.join(PERSONAL_RESTORE_PENDING_FILE_NAME);
    let receipt = directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME);
    // HKCU Run and the already-started Shell waiter intentionally provide two ways to reach the
    // same restore. The lease serializes them, but the winner removes `pending` after writing the
    // durable receipt. Treat that exact same-session receipt as a cleanup-only retry so the loser
    // can release its image and hand the staging directory to the bounded cleanup launcher.
    let (pending_matches, receipt_matches) =
        read_personal_restore_authorization(&directory, session_id)?;
    let report = if receipt_matches {
        None
    } else {
        let report =
            crate::personal_files::restore_preserved_personal_files_for_current_user(session_id)?;
        let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
            &directory,
            "lr-personal-restore-completed",
            "state",
            session_id.as_bytes(),
        )?;
        temporary.persist_replace(&receipt)?;
        if !read_personal_restore_state(&receipt, session_id)? {
            anyhow::bail!("personal-file restore completion receipt is missing");
        }
        Some(report)
    };

    let launcher = directory
        .parent()
        .ok_or_else(|| anyhow::anyhow!("restore staging directory has no volume root"))?
        .join(LAUNCHER_FILE_NAME);
    let expected = explorer_restore_command(&launcher)?;
    let actual = crate::registry::OfflineRegistry::query_string_optional(
        PERSONAL_RESTORE_RUN_KEY,
        PERSONAL_RESTORE_RUN_VALUE,
    )?;
    match actual {
        Some(value) if value == expected => {
            crate::registry::OfflineRegistry::delete_value(
                PERSONAL_RESTORE_RUN_KEY,
                PERSONAL_RESTORE_RUN_VALUE,
            )?;
        }
        None => {}
        Some(_) => anyhow::bail!("refusing to delete a changed personal-file restore Run value"),
    }
    if crate::registry::OfflineRegistry::query_string_optional(
        PERSONAL_RESTORE_RUN_KEY,
        PERSONAL_RESTORE_RUN_VALUE,
    )?
    .is_some()
    {
        anyhow::bail!("personal-file restore Run value still exists after deletion");
    }
    if pending_matches {
        std::fs::remove_file(&pending)
            .with_context(|| format!("remove restore pending marker {}", pending.display()))?;
    }
    if pending.exists() {
        anyhow::bail!("personal-file restore pending marker still exists after deletion");
    }
    Ok(report)
}

fn write_exact_personal_restore_state(
    directory: &Path,
    filename: &str,
    session_id: &str,
) -> Result<()> {
    validate_personal_restore_session_id(session_id)?;
    let path = directory.join(filename);
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        directory,
        "lr-personal-restore-state",
        "state",
        session_id.as_bytes(),
    )?;
    temporary.persist_replace(&path)?;
    if !read_personal_restore_state(&path, session_id)? {
        anyhow::bail!("personal-file restore state readback failed");
    }
    Ok(())
}

fn release_personal_restore_shell_gate(session_id: &str) -> Result<()> {
    let directory = personal_restore_state_directory()?;
    let state = read_personal_restore_shell_gate(&directory, session_id)?;
    // Console presentation is a non-destructive UX concern. The ordinary-user custom Shell is
    // intentionally read-only and may never be required to write into SYSTEM-owned staging before
    // the privileged worker restores the original Shell.
    append_first_logon_line("Personal files restore: native progress Shell gate selected")?;
    let gate_command = personal_restore_shell_command(&directory, session_id)?;
    let current = crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "Shell")?;
    if current.as_deref() == Some(gate_command.as_str()) {
        match &state.original_shell {
            Some(original) => {
                crate::registry::OfflineRegistry::set_string(WINLOGON_KEY, "Shell", original)?
            }
            None => crate::registry::OfflineRegistry::delete_value(WINLOGON_KEY, "Shell")?,
        }
    } else if current != state.original_shell {
        anyhow::bail!("refusing to overwrite a concurrently changed Winlogon Shell value");
    }
    if crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "Shell")?
        != state.original_shell
    {
        anyhow::bail!("original Winlogon Shell value did not survive readback");
    }
    let run_once = first_logon_run_once_command(&directory)?;
    match crate::registry::OfflineRegistry::query_string_optional(
        MACHINE_RUN_ONCE_KEY,
        PERSONAL_RESTORE_RUN_ONCE_VALUE,
    )? {
        Some(value) if value == run_once => crate::registry::OfflineRegistry::delete_value(
            MACHINE_RUN_ONCE_KEY,
            PERSONAL_RESTORE_RUN_ONCE_VALUE,
        )?,
        None => {}
        Some(_) => anyhow::bail!("refusing to delete a changed personal-file RunOnce value"),
    }
    for value in ["DefaultPassword", "AutoAdminLogon", "AutoLogonCount"] {
        let exists = if value == "AutoLogonCount" {
            crate::registry::OfflineRegistry::query_dword_optional(WINLOGON_KEY, value)?.is_some()
        } else {
            crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, value)?.is_some()
        };
        if exists {
            crate::registry::OfflineRegistry::delete_value(WINLOGON_KEY, value)?;
        }
    }
    if crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "DefaultPassword")?
        .is_some()
        || crate::registry::OfflineRegistry::query_string_optional(WINLOGON_KEY, "AutoAdminLogon")?
            .is_some()
        || crate::registry::OfflineRegistry::query_dword_optional(WINLOGON_KEY, "AutoLogonCount")?
            .is_some()
    {
        anyhow::bail!("personal-file second-logon autologon values survived retirement");
    }
    append_first_logon_line(
        "Personal files restore: pre-Explorer receipt verified and original Shell restored",
    )?;
    write_exact_personal_restore_state(
        &directory,
        PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME,
        session_id,
    )?;
    if let Err(error) = delete_personal_restore_logon_task(session_id) {
        // The task action points only at the fixed staging launcher, which the successful parent
        // finalizer removes. An orphaned registration is therefore bounded and non-load-bearing;
        // do not turn an already verified file restore and Shell readback into a locked desktop.
        let _ = append_first_logon_line(&format!(
            "Personal files restore: warning task cleanup failed detail={error:#}"
        ));
    }
    Ok(())
}

/// Restore while the current user's profile is loaded but the temporary Winlogon Shell is still
/// withholding Explorer. The privileged worker owns every state/log write, restores the original
/// Shell, then starts and verifies Explorer in the same interactive session before returning.
pub fn restore_personal_files_before_shell(
    session_id: &str,
) -> Result<Option<crate::personal_files::PersonalFileRestoreReport>> {
    let result = (|| {
        append_first_logon_line("Personal files restore: native progress Shell gate active")?;
        let profile_flags = current_user_profile_type_flags()?;
        ensure_persistent_current_user_profile(profile_flags)?;
        let report = restore_personal_files_at_shell_with_persistent_profile(session_id)?;
        if let Some(report) = &report {
            append_personal_restore_report(report);
        }
        release_personal_restore_shell_gate(session_id)?;
        start_personal_restore_explorer(session_id)?;
        Ok(report)
    })();
    if let Err(error) = &result {
        let directory = personal_restore_state_directory()?;
        let _ = write_exact_personal_restore_state(
            &directory,
            PERSONAL_RESTORE_FAILURE_FILE_NAME,
            session_id,
        );
        let _ = append_first_logon_line(&format!(
            "Personal files restore: pre-Explorer worker failed detail={error:#}"
        ));
    }
    result
}

/// Preserve a retry path after a failed pre-Explorer run. The exact SID-bound logon task must
/// still exist and be enabled; the failure marker never substitutes for that executable path.
pub fn rearm_personal_restore_before_shell(session_id: &str) -> Result<()> {
    let directory = personal_restore_state_directory()?;
    let _ = read_personal_restore_shell_gate(&directory, session_id)?;
    #[cfg(windows)]
    {
        use windows::core::BSTR;
        let (_apartment, root) = connect_task_scheduler()?;
        let task_name = personal_restore_task_name(session_id)?;
        let task_name_bstr = BSTR::from(task_name.as_str());
        let task = unsafe { root.GetTask(&task_name_bstr) }
            .context("ITaskFolder::GetTask(personal restore retry)")?;
        if unsafe { task.Enabled() }?.0 == 0 {
            anyhow::bail!("personal-file retry task is disabled");
        }
    }
    #[cfg(not(windows))]
    anyhow::bail!("Task Scheduler is unavailable on this platform");
    write_exact_personal_restore_state(&directory, PERSONAL_RESTORE_FAILURE_FILE_NAME, session_id)
}

#[cfg(windows)]
fn current_user_profile_type_flags() -> Result<u32> {
    use windows::Win32::UI::Shell::GetProfileType;

    let mut flags = 0_u32;
    unsafe { GetProfileType(&mut flags) }.context("GetProfileType(current user)")?;
    Ok(flags)
}

#[cfg(not(windows))]
fn current_user_profile_type_flags() -> Result<u32> {
    anyhow::bail!("the Windows user-profile type API is unavailable on this platform")
}

fn ensure_persistent_current_user_profile(flags: u32) -> Result<()> {
    // userenv.h: PT_TEMPORARY is deleted at logoff; PT_MANDATORY discards user changes. Roaming
    // profiles remain writable/persistent and therefore are not rejected here.
    const PT_TEMPORARY: u32 = 0x0000_0001;
    const PT_MANDATORY: u32 = 0x0000_0004;
    if flags & PT_TEMPORARY != 0 {
        anyhow::bail!("the current user has a temporary profile that will be deleted at logoff")
    }
    if flags & PT_MANDATORY != 0 {
        anyhow::bail!("the current user has a mandatory profile that discards user changes")
    }
    Ok(())
}

fn acquire_personal_restore_lease(directory: &Path) -> Result<std::fs::File> {
    let path = directory.join(PERSONAL_RESTORE_LOCK_FILE_NAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if metadata.is_some_and(|metadata| !metadata.is_file() || metadata_is_reparse_point(&metadata))
    {
        anyhow::bail!("personal-file restore lock is not a regular file");
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // A zero share mode is a kernel-enforced current-session lease. The empty file may remain
        // after a power loss, but an abandoned file has no live handle and is therefore reusable.
        options.share_mode(0);
    }
    options
        .open(&path)
        .with_context(|| format!("acquire personal-file restore lease {}", path.display()))
}

fn append_first_logon_line(line: &str) -> Result<()> {
    use std::io::Write as _;

    let program_data = std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("ProgramData is unavailable"))?;
    let directory = program_data.join("LetRecovery").join("Logs");
    std::fs::create_dir_all(&directory)?;
    reject_reparse_or_non_directory(&directory)?;
    let path = directory.join("FirstLogon-finalize.log");
    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
        if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
            anyhow::bail!("first-logon log is not a regular file");
        }
    }
    let sanitized = line.replace(['\r', '\n'], " ");
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(output, "{sanitized}")?;
    output.flush()?;
    Ok(())
}

fn image_state_allows_personal_restore(image_state: Option<&str>) -> bool {
    image_state == Some("IMAGE_STATE_COMPLETE")
}

#[cfg(windows)]
fn wait_for_current_session_shell() -> Result<u32> {
    use windows::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_TIMEOUT};
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::{
        GetCurrentProcessId, OpenProcess, WaitForInputIdle, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetShellWindow, GetWindowThreadProcessId};

    struct OwnedProcess(windows::Win32::Foundation::HANDLE);
    impl Drop for OwnedProcess {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    let current_process_id = unsafe { GetCurrentProcessId() };
    let mut current_session_id = 0_u32;
    unsafe { ProcessIdToSessionId(current_process_id, &mut current_session_id) }
        .context("ProcessIdToSessionId(current restore helper)")?;
    const SETUP_STATE_KEY: &str = r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Setup\State";
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
    'wait_for_shell: loop {
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("timed out waiting 1800 seconds for the current-session Shell window");
        }
        let shell_window = unsafe { GetShellWindow() };
        if shell_window.0.is_null() {
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }
        let mut shell_process_id = 0_u32;
        let shell_thread_id =
            unsafe { GetWindowThreadProcessId(shell_window, Some(&mut shell_process_id)) };
        if shell_thread_id == 0 || shell_process_id == 0 {
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }
        let mut shell_session_id = 0_u32;
        if unsafe { ProcessIdToSessionId(shell_process_id, &mut shell_session_id) }.is_err()
            || shell_session_id != current_session_id
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }
        let process = match unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                shell_process_id,
            )
        } {
            Ok(handle) => OwnedProcess(handle),
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue;
            }
        };
        // Microsoft defines zero as satisfied, WAIT_TIMEOUT as an elapsed bound, and WAIT_FAILED
        // as an error. It also documents that the wait is meaningful only once per process, so we
        // make one bounded call and then re-read the Shell HWND/PID rather than polling it again.
        let wait = unsafe { WaitForInputIdle(process.0, 60_000) };
        if wait == WAIT_TIMEOUT.0 {
            anyhow::bail!("the current-session Shell did not become input-idle within 60 seconds");
        }
        if wait == WAIT_FAILED.0 {
            return Err(std::io::Error::last_os_error())
                .context("WaitForInputIdle(current-session Shell)");
        }
        if wait != 0 {
            anyhow::bail!("WaitForInputIdle returned unexpected status {wait}");
        }
        let confirmed_window = unsafe { GetShellWindow() };
        let mut confirmed_process_id = 0_u32;
        if confirmed_window.0.is_null()
            || confirmed_window != shell_window
            || unsafe {
                GetWindowThreadProcessId(confirmed_window, Some(&mut confirmed_process_id))
            } == 0
            || confirmed_process_id != shell_process_id
        {
            std::thread::sleep(std::time::Duration::from_millis(250));
            continue;
        }

        // FirstLogonCommands are asynchronous on modern Windows. A Shell HWND can therefore
        // exist while Setup is still finishing the user profile. Microsoft documents
        // IMAGE_STATE_COMPLETE as the authoritative indication that specialize + oobeSystem have
        // completed. OOBEInProgress and SystemSetupInProgress are transient implementation
        // details: real installations may remove either value after setup, so requiring
        // `Some(0)` leaves this worker waiting forever on an already-complete system. Keep the
        // documented ImageState plus the same Shell PID continuously for a short quiescence
        // window; the window is reset by any state change rather than acting as a blind delay.
        let stable_until = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while std::time::Instant::now() < stable_until {
            let current_window = unsafe { GetShellWindow() };
            let mut current_shell_process_id = 0_u32;
            if current_window.0.is_null()
                || current_window != shell_window
                || unsafe {
                    GetWindowThreadProcessId(current_window, Some(&mut current_shell_process_id))
                } == 0
                || current_shell_process_id != shell_process_id
            {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue 'wait_for_shell;
            }
            let image_state = crate::registry::OfflineRegistry::query_string_optional(
                SETUP_STATE_KEY,
                "ImageState",
            )
            .ok()
            .flatten();
            if !image_state_allows_personal_restore(image_state.as_deref()) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                continue 'wait_for_shell;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        return Ok(shell_process_id);
    }
}

#[cfg(not(windows))]
fn wait_for_current_session_shell() -> Result<u32> {
    anyhow::bail!("the Windows Shell readiness API is unavailable on this platform")
}

fn append_personal_restore_report(report: &crate::personal_files::PersonalFileRestoreReport) {
    let _ = append_first_logon_line(
        "Personal files restore: regular-file strategy=create-new-stream-sync-readback-delete-source-v3",
    );
    let _ = append_first_logon_line(&format!(
        "Personal files restore: completed profile={} sources={} directories={} files={} conflicts={}",
        report.current_profile_root.display(),
        report.source_profiles,
        report.restored_directories,
        report.restored_files,
        report.renamed_conflicts
    ));
    for (scope, names, paths) in [
        (
            "personal",
            [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Music",
                "Videos",
            ],
            &report.personal_directories,
        ),
        (
            "public",
            [
                "Desktop",
                "Documents",
                "Downloads",
                "Pictures",
                "Music",
                "Videos",
            ],
            &report.public_directories,
        ),
    ] {
        for (name, path) in names.into_iter().zip(paths.iter()) {
            let _ = append_first_logon_line(&format!(
                "Personal files restore: destination scope={scope} name={name} path={}",
                path.display()
            ));
        }
    }
}

fn personal_restore_cleanup_raw_arguments(launcher: &Path) -> Result<String> {
    let text = launcher.as_os_str().to_string_lossy();
    if text.contains(['"', '\r', '\n']) {
        anyhow::bail!(
            "personal-file restore cleanup launcher path contains command metacharacters"
        );
    }
    Ok(format!(r#"/d /s /c ""{text}" cleanup""#))
}

fn spawn_personal_restore_cleanup() -> Result<()> {
    let directory = personal_restore_state_directory()?;
    let launcher = directory
        .parent()
        .ok_or_else(|| anyhow::anyhow!("restore staging directory has no volume root"))?
        .join(LAUNCHER_FILE_NAME);
    explorer_restore_command(&launcher)?;
    if std::fs::read(&launcher)? != LAUNCHER.as_bytes() {
        anyhow::bail!("personal-file restore cleanup launcher content mismatch");
    }
    let system_root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("SystemRoot is unavailable"))?;
    let command_interpreter = system_root.join("System32").join("cmd.exe");
    // cmd.exe /S /C requires an outer quote pair around a quoted command path plus arguments.
    // The launcher bytes and fixed root path were verified above; no user-controlled argument is
    // passed. The launcher retries deletion until this currently loaded helper image is released.
    let raw_arguments = personal_restore_cleanup_raw_arguments(&launcher)?;
    use std::os::windows::process::CommandExt as _;
    std::process::Command::new(command_interpreter)
        // cmd.exe deliberately does not use the C runtime argument grammar. `raw_arg` is the
        // documented Rust boundary for `/c`; `/s` then removes only this command string's first
        // and last quote, leaving the quoted fixed launcher path and numeric PID intact.
        .raw_arg(raw_arguments)
        .spawn()
        .context("start fixed first-logon cleanup launcher")?;
    Ok(())
}

/// Wait for the documented current-session Shell window, restore into the actual current token's
/// Known Folders, then hand cleanup to the fixed root launcher. The transient HKCU Run value is
/// registered first by the caller and remains a next-logon fallback if this worker is interrupted.
pub fn restore_personal_files_after_shell(
    session_id: &str,
    automation_shutdown_on_terminal: bool,
) -> Result<Option<crate::personal_files::PersonalFileRestoreReport>> {
    let _ = append_first_logon_line("Personal files restore: waiting for current-session Shell");
    let result = (|| {
        let shell_process_id = wait_for_current_session_shell()?;
        let _ = append_first_logon_line(&format!(
            "Personal files restore: current-session Shell ready pid={shell_process_id}"
        ));
        let profile_flags = current_user_profile_type_flags()?;
        let _ = append_first_logon_line(&format!(
            "Personal files restore: current user profile flags=0x{profile_flags:08x}"
        ));
        if let Err(profile_error) = ensure_persistent_current_user_profile(profile_flags) {
            if automation_shutdown_on_terminal {
                crate::windows_shutdown::schedule_graceful_shutdown(
                    30,
                    "LetRecovery detected a non-persistent first-logon profile; this test machine will power off without consuming preserved personal files.",
                )?;
                let _ = append_first_logon_line(
                    "Automation shutdown: accepted timeout=30s force_apps_closed=false reboot=false reason=non_persistent_profile",
                );
            }
            return Err(profile_error);
        }
        let report = restore_personal_files_at_shell_with_persistent_profile(session_id)?;
        if let Some(report) = &report {
            append_personal_restore_report(report);
        } else {
            let _ = append_first_logon_line(&format!(
                "Personal files restore: completed cleanup-only receipt={session_id}"
            ));
        }
        spawn_personal_restore_cleanup()?;
        if automation_shutdown_on_terminal {
            crate::windows_shutdown::schedule_graceful_shutdown(
                300,
                "LetRecovery automation finished; this test machine will power off.",
            )?;
            let _ = append_first_logon_line(
                "Automation shutdown: accepted timeout=300s force_apps_closed=false reboot=false",
            );
        }
        Ok(report)
    })();
    if let Err(error) = &result {
        let _ = append_first_logon_line(&format!(
            "Personal files restore: Explorer-stage worker failed detail={error:#}"
        ));
    }
    result
}

fn verify_staged_software(
    scripts_directory: &Path,
    packages: &[crate::software_install::SelectedSoftwarePackage],
) -> Result<()> {
    crate::software_install::validate_selected_packages(packages)?;
    if packages.is_empty() {
        return Ok(());
    }
    let directory = scripts_directory.join(crate::software_install::STAGING_DIRECTORY_NAME);
    reject_reparse_or_non_directory(&directory)?;
    for package in packages {
        let installer = directory.join(&package.filename);
        let metadata = std::fs::symlink_metadata(&installer)
            .with_context(|| format!("inspect staged software {}", package.id))?;
        if !metadata.is_file() || metadata_is_reparse_point(&metadata) || metadata.len() == 0 {
            anyhow::bail!(
                "staged software {} is not a non-empty regular file",
                package.id
            );
        }
    }
    Ok(())
}

pub fn stage_wifi_profile(target_partition: &str, bytes: &[u8]) -> Result<PathBuf> {
    PrivateWifiProfileBinding::from_bytes(bytes)?;
    let root = normalized_target_root(target_partition)?;
    let directory = root.join("LetRecovery_Scripts");
    std::fs::create_dir_all(&directory)?;
    reject_reparse_or_non_directory(&directory)?;
    let target = directory.join("LR_WiFi.xml");
    let temporary = crate::scoped_temp_file::ScopedTempFile::create_in(
        &directory,
        "lr-wifi-profile",
        "xml",
        bytes,
    )?;
    temporary.persist_replace(&target)?;
    let metadata = std::fs::symlink_metadata(&target)?;
    if !metadata.is_file()
        || metadata_is_reparse_point(&metadata)
        || std::fs::read(&target)? != bytes
    {
        anyhow::bail!("staged Wi-Fi profile readback mismatch");
    }
    Ok(target)
}

/// Stage the currently running, already-built endpoint as a short-lived native account helper.
/// Both endpoint binaries expose the same private post-OOBE account routes from
/// `lr-core::windows_accounts`, including bounded disabled-`defaultuser0` cleanup and the optional
/// RID-500 transition. The helper is deleted with the rest of `LetRecovery_Scripts` only after the
/// final account has logged on and first-logon finalization has completed.
pub fn stage_account_helper(target_partition: &str) -> Result<PathBuf> {
    use std::io::{Read as _, Write as _};

    let source = std::env::current_exe().context("locate the running account helper executable")?;
    let source_metadata = std::fs::symlink_metadata(&source)?;
    if !source_metadata.is_file() || metadata_is_reparse_point(&source_metadata) {
        anyhow::bail!("running account helper is not a regular file");
    }
    let root = normalized_target_root(target_partition)?;
    let directory = root.join("LetRecovery_Scripts");
    std::fs::create_dir_all(&directory)?;
    reject_reparse_or_non_directory(&directory)?;
    let target = directory.join(ACCOUNT_HELPER_FILE_NAME);
    let (temporary, mut output) = crate::scoped_temp_file::ScopedTempFile::create_writer_in(
        &directory,
        "lr-account-helper",
        "exe",
    )?;
    let mut input = std::fs::File::open(&source)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    drop(output);
    temporary.persist_replace(&target)?;
    let target_metadata = std::fs::symlink_metadata(&target)?;
    if !target_metadata.is_file()
        || metadata_is_reparse_point(&target_metadata)
        || target_metadata.len() != source_metadata.len()
        || crate::hash::sha256_file(&target, |_| {})? != crate::hash::sha256_file(&source, |_| {})?
    {
        anyhow::bail!("staged account helper readback mismatch");
    }
    // Keep the import explicit: this function intentionally consumes a file stream rather than
    // loading a multi-megabyte endpoint binary into memory.
    let mut probe = [0_u8; 2];
    std::fs::File::open(&target)?.read_exact(&mut probe)?;
    if probe != *b"MZ" {
        anyhow::bail!("staged account helper is not a PE executable");
    }
    let source_runtime = source
        .parent()
        .ok_or_else(|| anyhow::anyhow!("running account helper has no parent directory"))?
        .join(ACCOUNT_HELPER_RUNTIME_FILE_NAME);
    let runtime_metadata = match std::fs::symlink_metadata(&source_runtime) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if source
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("LetRecoveryPE.exe"))
        && runtime_metadata.is_none()
    {
        anyhow::bail!("PE account helper runtime is missing");
    }
    if let Some(runtime_metadata) = runtime_metadata {
        if !runtime_metadata.is_file() || metadata_is_reparse_point(&runtime_metadata) {
            anyhow::bail!("account helper runtime is not a regular file");
        }
        let runtime_target = directory.join(ACCOUNT_HELPER_RUNTIME_FILE_NAME);
        let (temporary, mut output) = crate::scoped_temp_file::ScopedTempFile::create_writer_in(
            &directory,
            "lr-account-helper-runtime",
            "dll",
        )?;
        let mut input = std::fs::File::open(&source_runtime)?;
        std::io::copy(&mut input, &mut output)?;
        output.flush()?;
        output.sync_all()?;
        drop(output);
        temporary.persist_replace(&runtime_target)?;
        let target_metadata = std::fs::symlink_metadata(&runtime_target)?;
        if !target_metadata.is_file()
            || metadata_is_reparse_point(&target_metadata)
            || target_metadata.len() != runtime_metadata.len()
            || crate::hash::sha256_file(&runtime_target, |_| {})?
                != crate::hash::sha256_file(&source_runtime, |_| {})?
        {
            anyhow::bail!("staged account helper runtime readback mismatch");
        }
    }
    Ok(target)
}

pub fn is_staged(target_partition: &str) -> Result<bool> {
    let root = normalized_target_root(target_partition)?;
    let path = root.join("LetRecovery_Scripts").join(SCRIPT_FILE_NAME);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("first-logon finalizer is not a regular file");
    }
    let bytes = std::fs::read(path)?;
    if !bytes.starts_with(b"[CmdletBinding()]")
        || bytes
            .windows("__LETRECOVERY_SOFTWARE_PLAN_BASE64__".len())
            .any(|window| window == b"__LETRECOVERY_SOFTWARE_PLAN_BASE64__")
    {
        return Ok(false);
    }
    let launcher = root.join(LAUNCHER_FILE_NAME);
    let metadata = match std::fs::symlink_metadata(&launcher) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("first-logon launcher is not a regular file");
    }
    Ok(std::fs::read(launcher)? == LAUNCHER.as_bytes())
}

pub fn render_command(order: u32) -> Result<String> {
    // Keep the answer-file command deliberately small. Windows 10 accepted the previous nested
    // `cmd /s /c` string into Panther but a real fresh-install run reached the desktop without
    // starting it or producing the first log line. The staged launcher owns quoting, diagnostics,
    // failure preservation, and post-process cleanup; the unattend field now names one fixed,
    // space-free path whose existence was read back before the image was booted.
    let command = format!(r#"cmd.exe /d /c %SystemDrive%\{LAUNCHER_FILE_NAME}"#);
    crate::unattend_command::render_first_logon_synchronous_command(
        order,
        &command,
        "Finalize LetRecovery setup",
    )
}

fn normalized_target_root(target_partition: &str) -> Result<PathBuf> {
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

fn reject_reparse_or_non_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspect first-logon directory {}", path.display()))?;
    if !metadata.is_dir() || metadata_is_reparse_point(&metadata) {
        anyhow::bail!("first-logon directory is not a regular directory");
    }
    Ok(())
}

fn metadata_is_reparse_point(metadata: &std::fs::Metadata) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "letrecovery-first-logon-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn finalizer_and_launcher_preserve_failures_and_cleanup_only_after_success() {
        assert!(SCRIPT.contains("Start-Process"));
        assert!(SCRIPT.contains("-WindowStyle Hidden -PassThru"));
        assert!(SCRIPT.contains("-SuppressCurrentSecurityUpdate"));
        assert!(SCRIPT.contains("WaitForExit(180000)"));
        assert!(SCRIPT.contains("timeout=180000ms"));
        assert!(SCRIPT.contains("if ($process.ExitCode -ne 0)"));
        assert!(SCRIPT.contains("remove-curated-appx.ps1"));
        assert!(SCRIPT.contains("remove-sec-health-ui.ps1"));
        assert!(SCRIPT.contains("WlanSetProfile"));
        assert!(SCRIPT.contains("WlanGetProfile"));
        assert!(SCRIPT.contains("-ReferencedAssemblies @('System.dll','System.Xml.dll')"));
        assert!(SCRIPT.contains("reason="));
        assert!(!SCRIPT.contains("netsh.exe"));
        assert!(SCRIPT.contains("} finally {"));
        assert!(!SCRIPT.contains("Remove-Item -LiteralPath $directory"));
        assert!(SCRIPT.contains("deferred until the PowerShell process exits"));
        assert!(!SCRIPT.contains("preinstalled application verification failed"));
        assert!(LAUNCHER.contains("First-logon launcher: started"));
        assert!(LAUNCHER.contains("First-logon launcher: script missing"));
        assert!(
            LAUNCHER.contains("%SystemRoot%\\System32\\WindowsPowerShell\\v1.0\\powershell.exe")
        );
        assert!(LAUNCHER.contains("rd /s /q \"%SystemDrive%\\LetRecovery_Scripts\""));
        assert!(LAUNCHER.contains("LETRECOVERY_FIRST_LOGON_CLEANUP_FAILURE"));
        assert!(LAUNCHER.contains("preserved after failure"));
        assert!(LAUNCHER.contains("if not \"!lr_ec!\"==\"0\""));
        assert!(LAUNCHER.contains("exit /b !lr_ec!"));
        assert!(LAUNCHER.contains("-PersonalRestoreAtShell"));
        assert!(LAUNCHER.contains("if /i \"%~1\"==\"restore\" goto :cleanup_after_restore"));
        assert!(LAUNCHER.contains(PERSONAL_RESTORE_PENDING_FILE_NAME));
        assert!(!LAUNCHER.contains("if /i \"%~1\"==\"gate\""));
        assert!(!LAUNCHER.contains("--internal-show-personal-restore-console"));
        assert!(!LAUNCHER.contains("--internal-run-personal-restore-console"));
        assert!(!LAUNCHER.contains("personal-restore-shell-gate.visible"));
        assert!(!LAUNCHER.contains("--internal-start-personal-restore-explorer"));
        assert!(!LAUNCHER.contains("-ShowPersonalRestoreConsole"));
        assert!(!LAUNCHER.contains("GetConsoleWindow"));
        assert!(!LAUNCHER.contains("ShowWindow"));
        assert!(LAUNCHER.contains("rd /s /q \"%SystemDrive%\\LetRecovery_Scripts\" >nul 2>&1"));
        for line in LAUNCHER
            .lines()
            .filter(|line| line.contains(">>\"%lr_log%\""))
        {
            assert!(
                line.contains("2>nul"),
                "ordinary-user diagnostic append can leak Access Denied: {line}"
            );
        }
        assert!(!LAUNCHER.contains("start \"LetRecovery - Personal File Recovery\""));
        assert!(!LAUNCHER.contains("[!lr_progress!] Step !lr_step!/4"));
        assert!(!LAUNCHER.contains("[####] Step 4/4"));
        assert!(!LAUNCHER.contains(r"C:\Windows"));
        assert!(!LAUNCHER.contains(r"C:\LetRecovery"));
        assert!(!SCRIPT.contains("ShowPersonalRestoreConsole"));
        assert!(!SCRIPT.contains("PersonalRestoreGateSessionId"));
        assert!(!SCRIPT.contains("GetConsoleWindow"));
        assert!(!SCRIPT.contains("IsWindowVisible"));
        assert!(SCRIPT.contains(PERSONAL_RESTORE_SHELL_VERIFIED_FILE_NAME));
        assert!(SCRIPT.contains("verified current-session Explorer shell pid={0}"));
        assert!(!SCRIPT.contains("Start-Process -FilePath $explorerPath"));
        assert!(!SCRIPT.contains(r"C:\Windows"));
        assert!(LAUNCHER.contains(BUILTIN_TRANSITION_MARKER_FILE_NAME));
        assert!(LAUNCHER.contains("staging preserved for the final account logon"));
        assert!(LAUNCHER.contains("pre-Explorer gate retained"));
        assert!(LAUNCHER.contains(
            "if exist \"%SystemDrive%\\LetRecovery_Scripts\\personal-restore-shell-gate.state\""
        ));
        assert!(LAUNCHER.contains("staging retained until the verified Explorer Shell takes over"));
        assert!(!LAUNCHER.contains("--internal-start-personal-restore-explorer"));
        assert!(LAUNCHER.contains("goto :cleanup_after_restore"));
        assert!(!LAUNCHER.contains("cleanup worker started"));
        assert!(!LAUNCHER.contains("retry limit reached attempts="));
        let command = render_command(1).unwrap();
        assert!(command.contains(&format!(
            "cmd.exe /d /c %SystemDrive%\\{LAUNCHER_FILE_NAME}"
        )));
        assert!(!command.contains("powershell.exe"));
        assert!(!command.contains("&amp;"));
    }

    #[test]
    fn builtin_administrator_transition_marker_is_canonical_and_identity_bound() {
        let marker = BuiltinAdministratorTransition::new(
            "LRAdmin11",
            "LrOOBE-0123456789ab",
            "S-1-5-21-100-200-300-1001",
        )
        .unwrap();
        let bytes = marker.render();
        assert_eq!(
            BuiltinAdministratorTransition::parse(&bytes).unwrap(),
            marker
        );
        marker
            .verify_names("LRAdmin11", "LrOOBE-0123456789ab")
            .unwrap();
        assert!(marker
            .verify_names("Administrator", "LrOOBE-0123456789ab")
            .is_err());

        let mut noncanonical = bytes;
        noncanonical.extend_from_slice(b"Extra=true\r\n");
        assert!(BuiltinAdministratorTransition::parse(&noncanonical).is_err());
    }

    #[test]
    fn personal_restore_marker_is_exact_and_none_removes_stale_state() {
        let directory = temporary_directory("personal-restore-state");
        let session_id = "0123456789abcdef0123456789abcdef";
        stage_personal_restore_marker(&directory, Some(session_id)).unwrap();
        assert_eq!(
            std::fs::read_to_string(directory.join(PERSONAL_RESTORE_PENDING_FILE_NAME)).unwrap(),
            session_id
        );
        std::fs::write(
            directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME),
            session_id,
        )
        .unwrap();
        std::fs::write(directory.join(PERSONAL_RESTORE_LOCK_FILE_NAME), []).unwrap();
        std::fs::write(
            directory.join(PERSONAL_RESTORE_SHELL_GATE_FILE_NAME),
            session_id,
        )
        .unwrap();
        std::fs::write(
            directory.join(PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME),
            session_id,
        )
        .unwrap();
        std::fs::write(
            directory.join(PERSONAL_RESTORE_FAILURE_FILE_NAME),
            session_id,
        )
        .unwrap();
        stage_personal_restore_marker(&directory, None).unwrap();
        assert!(!directory.join(PERSONAL_RESTORE_PENDING_FILE_NAME).exists());
        assert!(!directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME).exists());
        assert!(!directory.join(PERSONAL_RESTORE_LOCK_FILE_NAME).exists());
        assert!(!directory
            .join(PERSONAL_RESTORE_SHELL_GATE_FILE_NAME)
            .exists());
        assert!(!directory
            .join(PERSONAL_RESTORE_SHELL_RELEASED_FILE_NAME)
            .exists());
        assert!(!directory.join(PERSONAL_RESTORE_FAILURE_FILE_NAME).exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn personal_restore_shell_gate_state_is_canonical_and_session_bound() {
        let session_id = "0123456789abcdef0123456789abcdef";
        let state = PersonalRestoreShellGate::new(
            session_id,
            Some(r#"explorer.exe /factory,{A-B-C}"#.to_owned()),
        )
        .unwrap();
        let bytes = state.render();
        assert_eq!(PersonalRestoreShellGate::parse(&bytes).unwrap(), state);
        let absent = PersonalRestoreShellGate::new(session_id, None).unwrap();
        assert_eq!(
            PersonalRestoreShellGate::parse(&absent.render()).unwrap(),
            absent
        );
        let mut trailing = bytes;
        trailing.extend_from_slice(b"Unexpected=true\r\n");
        assert!(PersonalRestoreShellGate::parse(&trailing).is_err());
        assert!(PersonalRestoreShellGate::new("bad", Some("explorer.exe".to_owned())).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn personal_restore_task_is_sid_bound_highest_interactive_worker() {
        let root = temporary_directory("personal-restore-task");
        let directory = root.join("LetRecovery_Scripts");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(root.join(LAUNCHER_FILE_NAME), LAUNCHER).unwrap();
        let session_id = "0123456789abcdef0123456789abcdef";
        let sid = "S-1-5-21-100-200-300-1001";
        let worker = personal_restore_task_xml(&directory, session_id, sid).unwrap();
        assert!(worker.contains(&format!("<UserId>{sid}</UserId>")));
        assert!(worker.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(worker.contains(LAUNCHER_FILE_NAME));
        assert!(worker.contains(r#"/d /c call &quot;"#));
        assert!(!worker.contains("/s /c"));
        assert!(!worker.contains(r#"\&quot;"#));
        assert!(worker.contains("<RunLevel>HighestAvailable</RunLevel>"));
        assert!(worker.contains("<LogonTrigger>"));
        assert!(!worker.contains(" gate "));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn native_progress_shell_gate_and_run_once_commands_use_real_windows_quotes() {
        use windows::Win32::UI::WindowsAndMessaging::{WS_POPUP, WS_VISIBLE};

        let style = personal_restore_progress_window_style();
        assert_ne!(style.0 & WS_VISIBLE.0, 0);
        assert_ne!(style.0 & WS_POPUP.0, 0);
        let root = temporary_directory("personal-restore-shell-command");
        let directory = root.join("LetRecovery_Scripts");
        std::fs::create_dir_all(&directory).unwrap();
        let launcher = root.join(LAUNCHER_FILE_NAME);
        std::fs::write(&launcher, LAUNCHER).unwrap();
        let helper = directory.join(ACCOUNT_HELPER_FILE_NAME);
        std::fs::write(&helper, b"helper").unwrap();
        let session_id = "0123456789abcdef0123456789abcdef";
        let command_interpreter = crate::windows_compat::system_directory()
            .unwrap()
            .join("cmd.exe");

        assert_eq!(
            render_personal_restore_shell_command(&helper, session_id).unwrap(),
            format!(
                r#""{}" --internal-personal-restore-progress-shell {}"#,
                helper.display(),
                session_id
            )
        );
        assert_eq!(
            first_logon_run_once_command(&directory).unwrap(),
            format!(
                r#""{}" /d /c "{}""#,
                command_interpreter.display(),
                launcher.display()
            )
        );
        let shell = render_personal_restore_shell_command(&helper, session_id).unwrap();
        assert!(!shell.starts_with("cmd.exe"));
        assert!(!shell.contains("powershell.exe"));
        assert!(!shell.contains("ShowPersonalRestoreConsole"));
        assert!(render_personal_restore_shell_command(&launcher, session_id).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_session_receipt_authorizes_cleanup_only_retry_after_pending_was_consumed() {
        let directory = temporary_directory("personal-restore-receipt-retry");
        let session_id = "0123456789abcdef0123456789abcdef";
        std::fs::write(
            directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME),
            session_id,
        )
        .unwrap();
        assert_eq!(
            read_personal_restore_authorization(&directory, session_id).unwrap(),
            (false, true)
        );
        assert!(read_personal_restore_authorization(
            &directory,
            "fedcba9876543210fedcba9876543210"
        )
        .is_err());
        std::fs::remove_file(directory.join(PERSONAL_RESTORE_RECEIPT_FILE_NAME)).unwrap();
        assert!(read_personal_restore_authorization(&directory, session_id).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn builtin_transition_secret_is_canonical_and_none_removes_stale_state() {
        let directory = temporary_directory("builtin-transition-secret");
        let password = crate::unattend_account::SensitiveString::new("S3cret<&!");
        stage_builtin_transition_secret(&directory, Some(&password)).unwrap();
        let path = directory.join(BUILTIN_TRANSITION_SECRET_STAGING_FILE_NAME);
        let bytes = Zeroizing::new(std::fs::read(&path).unwrap());
        let parsed = crate::unattend_account::parse_protected_administrator_secret(&bytes).unwrap();
        assert_eq!(parsed.as_str(), password.expose_secret());
        stage_builtin_transition_secret(&directory, None).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn explorer_restore_command_is_fixed_and_within_the_documented_run_limit() {
        let directory = temporary_directory("explorer-command");
        let launcher = directory.join(LAUNCHER_FILE_NAME);
        std::fs::write(&launcher, LAUNCHER).unwrap();
        let command = explorer_restore_command(&launcher).unwrap();
        assert!(command.contains("cmd.exe\" /d /s /c"));
        assert!(command.ends_with("\" restore\""));
        assert!(command.encode_utf16().count() <= 260);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cleanup_command_has_raw_cmd_outer_quotes_and_only_a_numeric_pid() {
        let command =
            personal_restore_cleanup_raw_arguments(Path::new(r"C:\LetRecovery-first-logon.cmd"))
                .unwrap();
        assert_eq!(
            command,
            r#"/d /s /c ""C:\LetRecovery-first-logon.cmd" cleanup""#
        );
        assert!(
            personal_restore_cleanup_raw_arguments(Path::new("C:\\bad\nlauncher.cmd")).is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn cleanup_raw_cmd_arguments_execute_a_launcher_path_with_spaces() {
        use std::os::windows::process::CommandExt as _;

        let directory = temporary_directory("cleanup raw command");
        let launcher = directory.join("launcher with spaces.cmd");
        let marker = directory.join("cleanup-marker.txt");
        std::fs::write(
            &launcher,
            b"@echo off\r\n> \"%~dp0cleanup-marker.txt\" echo %~1\r\n",
        )
        .unwrap();
        let command_interpreter = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32")
            .join("cmd.exe");
        let raw_arguments = personal_restore_cleanup_raw_arguments(&launcher).unwrap();
        let status = std::process::Command::new(command_interpreter)
            .raw_arg(raw_arguments)
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "cleanup");
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn non_persistent_profile_types_are_rejected_before_personal_file_restore() {
        assert!(ensure_persistent_current_user_profile(0).is_ok());
        assert!(ensure_persistent_current_user_profile(0x0000_0002).is_ok());
        assert!(ensure_persistent_current_user_profile(0x0000_0008).is_ok());
        assert!(ensure_persistent_current_user_profile(0x0000_0001).is_err());
        assert!(ensure_persistent_current_user_profile(0x0000_0004).is_err());
        assert!(ensure_persistent_current_user_profile(0x0000_0003).is_err());
    }

    #[test]
    fn personal_restore_uses_only_the_documented_complete_image_state() {
        assert!(image_state_allows_personal_restore(Some(
            "IMAGE_STATE_COMPLETE"
        )));
        assert!(!image_state_allows_personal_restore(None));
        assert!(!image_state_allows_personal_restore(Some(
            "IMAGE_STATE_SPECIALIZE_RESEAL_TO_OOBE"
        )));
        assert!(!image_state_allows_personal_restore(Some(
            "IMAGE_STATE_UNDEPLOYABLE"
        )));
    }

    #[test]
    fn selected_software_plan_is_embedded_in_the_only_required_finalizer_file() {
        let package = crate::software_install::SelectedSoftwarePackage {
            id: "sevenzip".into(),
            name: "7-Zip".into(),
            download_url: "https://example.test/7z.exe".into(),
            filename: "7z.exe".into(),
            silent_command: "\"{installer}\" /S".into(),
            requires_admin: true,
        };
        let second_package = crate::software_install::SelectedSoftwarePackage {
            id: "bandizip".into(),
            name: "Bandizip".into(),
            download_url: "https://example.test/bandizip.exe".into(),
            filename: "bandizip.exe".into(),
            silent_command: "\"{installer}\" /S".into(),
            requires_admin: true,
        };
        let session_id = "0123456789abcdef0123456789abcdef";
        let temporary_oobe_account =
            crate::unattend_account::temporary_oobe_account_name(session_id).unwrap();
        let rendered = rendered_script(
            &[package, second_package],
            true,
            Some(session_id),
            Some(&temporary_oobe_account),
            Some("RecoveryAdmin"),
        )
        .unwrap();
        assert!(!rendered.contains("__LETRECOVERY_SOFTWARE_PLAN_BASE64__"));
        assert!(!rendered.contains("__LETRECOVERY_AUTOMATION_SHUTDOWN_ON_TERMINAL__"));
        assert!(!rendered.contains("__LETRECOVERY_PERSONAL_RESTORE_SESSION_ID__"));
        assert!(!rendered.contains("__LETRECOVERY_TEMPORARY_OOBE_ACCOUNT_HEX__"));
        assert!(!rendered.contains("__LETRECOVERY_BUILTIN_ADMINISTRATOR_NAME_HEX__"));
        assert!(rendered.contains("$automationShutdownOnTerminal = $true"));
        assert!(rendered.contains(&format!("$personalRestoreSessionId = '{session_id}'")));
        assert!(rendered.contains("--internal-activate-personal-restore-shell-gate"));
        assert!(rendered.contains("--internal-begin-personal-restore-second-logon"));
        assert!(rendered.contains("--internal-restore-personal-files-before-shell"));
        assert!(rendered.contains("--internal-rearm-personal-restore-before-shell"));
        assert!(rendered.contains("--internal-restore-personal-files-at-shell"));
        assert!(!rendered.contains("--internal-register-personal-files-at-shell"));
        assert!(!rendered.contains("--internal-restore-personal-files-after-shell"));
        assert!(rendered.contains("--internal-delete-temporary-oobe-account"));
        assert!(rendered.contains("--internal-cleanup-disabled-defaultuser0"));
        assert!(rendered.contains("Windows default OOBE account cleanup: warning"));
        assert!(rendered.contains("--internal-begin-builtin-administrator-transition"));
        assert!(rendered
            .contains("--internal-begin-builtin-administrator-transition-with-personal-restore"));
        assert!(rendered.contains("--internal-finish-builtin-administrator-transition"));
        assert!(rendered.contains("--internal-retire-builtin-administrator-transition"));
        assert!(rendered.contains(
            "if (-not $PersonalRestoreAtShell -and -not [string]::IsNullOrWhiteSpace($builtinAdministratorNameHex))"
        ));
        assert!(rendered.contains("Temporary OOBE account cleanup: completed"));
        assert!(rendered.contains(
            "First-logon transition: requesting pre-desktop restart force_apps_closed=true"
        ));
        assert!(rendered.contains("starting before Explorer"));
        assert!(rendered.contains("original Shell released"));
        assert!(rendered.contains("Start-Process -FilePath $personalRestoreHelper"));
        assert!(rendered.contains("-Wait -PassThru"));
        assert!(rendered.contains("-RedirectStandardOutput $restoreStdout"));
        assert!(rendered.contains("personal-file restore helper failed with exit code"));
        assert!(
            rendered
                .find("Personal files restore: starting before Explorer")
                .unwrap()
                < rendered
                    .find("Windows Security UI cleanup retry: starting")
                    .unwrap(),
            "personal files must be restored before optional online maintenance"
        );
        assert!(
            rendered
                .find("Personal files restore: starting before Explorer")
                .unwrap()
                < rendered.find("Preinstalled software: starting").unwrap(),
            "personal files must be restored before optional software installation"
        );
        assert!(!rendered.contains("preinstalled-software-plan.json"));
        assert!(rendered.contains("Preinstalled software: starting expected="));
        assert!(
            rendered.contains("$systemVolumeRoot = [System.IO.Path]::GetPathRoot($env:SystemRoot)")
        );
        assert!(rendered.contains(
            "$directory = [System.IO.Path]::Combine($systemVolumeRoot, 'LetRecovery_Scripts')"
        ));
        assert!(!rendered.contains("Combine($env:SystemDrive, 'LetRecovery_Scripts')"));
        assert!(rendered.contains(
            "$entries = @((ConvertFrom-Json -InputObject $softwarePlanJson) | ForEach-Object { $_ })"
        ));
        assert!(rendered.contains("for ($attempt = 1; $attempt -le 3"));
        assert!(rendered
            .contains("if ($argument -eq '__LETRECOVERY_FIRST_LOGON_INSTALLER__') { $installer }"));
        assert!(rendered.contains("Preinstalled software: retry id="));
        assert!(rendered.contains("remaining first-logon work continues"));
        assert!(!rendered.contains("selected preinstalled software failed"));
        assert!(rendered.contains("WaitForExit(1800000)"));
        assert!(rendered.contains("InitiateSystemShutdownEx"));
        assert!(rendered.contains("REASON_APPLICATION_INSTALLATION_PLANNED"));
        assert!(rendered.contains("timeout=300s force_apps_closed=false reboot=false"));
        assert!(rendered.contains(
            "$automationShutdownOnTerminal -and ([string]::IsNullOrWhiteSpace($personalRestoreSessionId) -or $PersonalRestoreAtShell -or [System.IO.File]::Exists($personalRestoreShellReleased))"
        ));
    }

    #[test]
    fn builtin_administrator_transition_requires_both_account_identities() {
        assert!(rendered_script(&[], false, None, Some("LrOOBE-0123456789ab"), None).is_err());
        assert!(rendered_script(&[], false, None, None, Some("RecoveryAdmin")).is_err());
    }

    #[test]
    fn private_wifi_binding_is_all_or_nothing_and_detects_tampering() {
        let bytes = b"<WLANProfile><name>test</name></WLANProfile>";
        let binding = PrivateWifiProfileBinding::from_bytes(bytes).unwrap();
        binding.verify(bytes).unwrap();
        assert!(binding.verify(b"<WLANProfile />").is_err());

        let parsed = private_wifi_binding_from_install_ini(&format!(
            "[Install]\r\nMigrateWifi=true\r\nWifiProfileLength={}\r\nWifiProfileSha256={}\r\n",
            binding.length_bytes, binding.sha256
        ))
        .unwrap()
        .unwrap();
        assert_eq!(parsed, binding);
        assert!(private_wifi_binding_from_install_ini(
            "[Install]\r\nMigrateWifi=true\r\nWifiProfileLength=12\r\n"
        )
        .is_err());
        assert_eq!(
            private_wifi_binding_from_install_ini("[Install]\r\nMigrateWifi=false\r\n").unwrap(),
            None
        );
    }
}
