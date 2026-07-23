[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string] $ReleaseTag,

    [string] $VersionFile = (Join-Path $PSScriptRoot "..\..\官网\version.json")
)

$ErrorActionPreference = "Stop"

$tag = $ReleaseTag.Trim()
if ([string]::IsNullOrWhiteSpace($tag)) {
    throw "Release tag must not be empty"
}

$version = if ($tag.StartsWith("v", [StringComparison]::OrdinalIgnoreCase)) {
    "v" + $tag.Substring(1)
} else {
    "v" + $tag
}

if ($version -notmatch '^v\d{4}\.\d{1,2}\.\d{1,2}(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
    throw "Release tag cannot be used as a website version: $ReleaseTag"
}

if (-not (Test-Path -LiteralPath $VersionFile -PathType Leaf)) {
    throw "Website version file does not exist: $VersionFile"
}

$resolvedPath = (Resolve-Path -LiteralPath $VersionFile).Path
$currentDocument = Get-Content -Raw -Encoding UTF8 -LiteralPath $resolvedPath | ConvertFrom-Json
if ($null -eq $currentDocument.version -or $currentDocument.version -isnot [string]) {
    throw "Website version file must contain a string property named 'version'"
}
if ([string]$currentDocument.version -notmatch '^v\d{4}\.\d{1,2}\.\d{1,2}(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
    throw "Website version file contains an invalid version: $($currentDocument.version)"
}

$previousVersion = [string]$currentDocument.version
if ($previousVersion -eq $version) {
    Write-Host "Website version is already $version"
    return
}

$directory = Split-Path -Parent $resolvedPath
$temporaryPath = Join-Path $directory (".version." + [Guid]::NewGuid().ToString("N") + ".tmp")
$backupPath = Join-Path $directory (".version." + [Guid]::NewGuid().ToString("N") + ".backup")
$utf8WithoutBom = [Text.UTF8Encoding]::new($false)
$json = ([ordered]@{ version = $version } | ConvertTo-Json) + "`n"

try {
    [IO.File]::WriteAllText($temporaryPath, $json, $utf8WithoutBom)
    $stagedDocument = Get-Content -Raw -Encoding UTF8 -LiteralPath $temporaryPath | ConvertFrom-Json
    if ([string]$stagedDocument.version -ne $version) {
        throw "Temporary website version file failed read-back verification"
    }

    [IO.File]::Replace($temporaryPath, $resolvedPath, $backupPath)

    $persistedDocument = Get-Content -Raw -Encoding UTF8 -LiteralPath $resolvedPath | ConvertFrom-Json
    if ([string]$persistedDocument.version -ne $version) {
        throw "Website version file failed post-replacement verification"
    }
} finally {
    if (Test-Path -LiteralPath $temporaryPath) {
        Remove-Item -LiteralPath $temporaryPath -Force
    }
    if (Test-Path -LiteralPath $backupPath) {
        Remove-Item -LiteralPath $backupPath -Force
    }
}

Write-Host "Website version updated: $previousVersion -> $version"
