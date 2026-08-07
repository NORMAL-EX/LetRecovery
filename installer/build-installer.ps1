[CmdletBinding()]
param(
    [string]$SourceDirectory,
    [string]$OutputDirectory,
    [string]$InnoCompiler,
    [switch]$RequireSignature
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($SourceDirectory)) {
    $SourceDirectory = Join-Path $PSScriptRoot "..\pkg"
}
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $PSScriptRoot "output"
}

function Resolve-ExistingFile {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Description)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description not found: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

$source = (Resolve-Path -LiteralPath $SourceDirectory).Path
if (-not (Test-Path -LiteralPath $source -PathType Container)) {
    throw "Package directory not found: $SourceDirectory"
}

$requiredFiles = @(
    "LetRecovery.exe",
    "config.json",
    "README.txt",
    "bin\pe\LetRecovery_PE.wim"
)
foreach ($relativePath in $requiredFiles) {
    Resolve-ExistingFile -Path (Join-Path $source $relativePath) -Description "Required package file" | Out-Null
}

$appExe = Join-Path $source "LetRecovery.exe"
$versionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($appExe)
$numericVersion = $versionInfo.FileVersion
if ([string]::IsNullOrWhiteSpace($numericVersion) -or $numericVersion -notmatch '^\d+\.\d+\.\d+\.\d+$') {
    throw "LetRecovery.exe has an invalid four-part file version: '$numericVersion'"
}
$displayVersion = $numericVersion -replace '\.0$', ''

if ([string]::IsNullOrWhiteSpace($InnoCompiler)) {
    $candidates = @(
        (Join-Path $PSScriptRoot "tools\inno\ISCC.exe"),
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "$env:ProgramFiles\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    )
    $InnoCompiler = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
}
if ([string]::IsNullOrWhiteSpace($InnoCompiler)) {
    throw "ISCC.exe was not found. Install Inno Setup 6.7 or pass -InnoCompiler explicitly."
}
$compiler = Resolve-ExistingFile -Path $InnoCompiler -Description "Inno Setup compiler"
$script = Resolve-ExistingFile -Path (Join-Path $PSScriptRoot "LetRecovery.iss") -Description "Installer script"
Resolve-ExistingFile -Path (Join-Path $PSScriptRoot "LICENSE.zh-CN.txt") -Description "Chinese license text" | Out-Null
Resolve-ExistingFile -Path (Join-Path $PSScriptRoot "NOTICE.zh-CN.txt") -Description "Installation notice" | Out-Null
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..")).Path
$icon = Get-ChildItem -LiteralPath $repoRoot -Directory | ForEach-Object {
    $manifest = Join-Path $_.FullName "Cargo.toml"
    $candidate = Join-Path $_.FullName "assets\icon.ico"
    if ((Test-Path -LiteralPath $manifest -PathType Leaf) -and
        (Test-Path -LiteralPath $candidate -PathType Leaf) -and
        ((Get-Content -LiteralPath $manifest -Raw) -match 'name\s*=\s*"LetRecovery"')) {
        $candidate
    }
} | Select-Object -First 1
$icon = Resolve-ExistingFile -Path $icon -Description "Application icon"

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$output = (Resolve-Path -LiteralPath $OutputDirectory).Path

Write-Host "Building LetRecovery installer"
Write-Host "  Source : $source"
Write-Host "  Version: $displayVersion"
Write-Host "  Output : $output"

try {
    $env:LETRECOVERY_INSTALLER_SOURCE = $source
    $env:LETRECOVERY_INSTALLER_OUTPUT = $output
    $env:LETRECOVERY_INSTALLER_VERSION = $numericVersion
    $env:LETRECOVERY_INSTALLER_DISPLAY_VERSION = $displayVersion
    $env:LETRECOVERY_INSTALLER_ICON = $icon

    & $compiler /Qp $script
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup compiler failed with exit code $LASTEXITCODE"
    }
}
finally {
    Remove-Item Env:LETRECOVERY_INSTALLER_SOURCE -ErrorAction SilentlyContinue
    Remove-Item Env:LETRECOVERY_INSTALLER_OUTPUT -ErrorAction SilentlyContinue
    Remove-Item Env:LETRECOVERY_INSTALLER_VERSION -ErrorAction SilentlyContinue
    Remove-Item Env:LETRECOVERY_INSTALLER_DISPLAY_VERSION -ErrorAction SilentlyContinue
    Remove-Item Env:LETRECOVERY_INSTALLER_ICON -ErrorAction SilentlyContinue
}

$installer = Resolve-ExistingFile -Path (Join-Path $output "LetRecovery-Setup-x64.exe") -Description "Compiled installer"
$signature = Get-AuthenticodeSignature -LiteralPath $installer
if ($RequireSignature -and $signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "Installer signature is required but status is $($signature.Status)."
}

$item = Get-Item -LiteralPath $installer
$hash = Get-FileHash -LiteralPath $installer -Algorithm SHA256
Write-Host "Installer ready: $installer"
Write-Host "Size: $($item.Length) bytes"
Write-Host "SHA-256: $($hash.Hash)"
Write-Host "Signature: $($signature.Status)"

[pscustomobject]@{
    Path = $installer
    Version = $displayVersion
    Size = $item.Length
    Sha256 = $hash.Hash
    Signature = $signature.Status.ToString()
}
