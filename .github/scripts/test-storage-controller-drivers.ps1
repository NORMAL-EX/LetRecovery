param(
    [Parameter(Mandatory = $true)][string]$Root,
    [string]$LockFile = "docs\STORAGE_CONTROLLER_DRIVERS.lock.json",
    [switch]$VerifySignatures
)

$ErrorActionPreference = "Stop"
$resolvedRoot = (Resolve-Path -LiteralPath $Root -ErrorAction Stop).Path
$resolvedLock = (Resolve-Path -LiteralPath $LockFile -ErrorAction Stop).Path
$rootItem = Get-Item -LiteralPath $resolvedRoot
if (-not $rootItem.PSIsContainer -or ($rootItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
    throw "Storage-controller root must be a regular directory: $resolvedRoot"
}
$manifest = Get-Content -LiteralPath $resolvedLock -Raw -Encoding UTF8 | ConvertFrom-Json
if ([int]$manifest.version -ne 1 -or @($manifest.packages).Count -ne 2) {
    throw "Invalid storage-controller driver lock manifest: $resolvedLock"
}

$expectedFiles = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($package in $manifest.packages) {
    $directory = [string]$package.directory
    if ([IO.Path]::GetFileName($directory) -ne $directory -or $directory -notmatch '^intel-vmd-[a-z0-9-]+$') {
        throw "Invalid storage-controller package directory: $directory"
    }
    $packageRoot = Join-Path $resolvedRoot $directory
    if (-not (Test-Path -LiteralPath $packageRoot -PathType Container)) {
        throw "Missing storage-controller package: $packageRoot"
    }
    $infPath = Join-Path $packageRoot "iaStorVD.inf"
    $infText = Get-Content -LiteralPath $infPath -Raw
    foreach ($controllerId in $package.controller_ids) {
        if ($infText.IndexOf([string]$controllerId, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
            throw "INF does not cover locked controller $controllerId`: $infPath"
        }
    }

    foreach ($file in $package.files) {
        $name = [string]$file.name
        if ([IO.Path]::GetFileName($name) -ne $name) {
            throw "Invalid locked driver filename: $directory\$name"
        }
        $relative = "$directory\$name"
        if (-not $expectedFiles.Add($relative)) {
            throw "Duplicate locked driver file: $relative"
        }
        $path = Join-Path $packageRoot $name
        if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
            throw "Missing locked driver file: $path"
        }
        $item = Get-Item -LiteralPath $path
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
            throw "Driver package member must not be a reparse point: $path"
        }
        if ($item.Length -ne [int64]$file.size) {
            throw "Driver package member size mismatch: $relative"
        }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if (-not $hash.Equals([string]$file.sha256, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Driver package member SHA-256 mismatch: $relative"
        }
        $signerProperty = $file.PSObject.Properties["signer_contains"]
        $signerContains = if ($null -ne $signerProperty) { [string]$signerProperty.Value } else { "" }
        if ($VerifySignatures -and -not [string]::IsNullOrWhiteSpace($signerContains)) {
            $signature = Get-AuthenticodeSignature -LiteralPath $path
            if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
                throw "Driver package signature is not valid: $relative ($($signature.Status))"
            }
            $subject = [string]$signature.SignerCertificate.Subject
            if ($subject.IndexOf($signerContains, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
                throw "Unexpected driver signer for $relative`: $subject"
            }
        }
    }
}

$actualFiles = @(
    Get-ChildItem -LiteralPath $resolvedRoot -Recurse -File | ForEach-Object {
        $_.FullName.Substring($resolvedRoot.Length).TrimStart('\')
    }
)
$expectedDirectories = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($package in $manifest.packages) {
    [void]$expectedDirectories.Add([string]$package.directory)
}
$actualDirectories = @(Get-ChildItem -LiteralPath $resolvedRoot -Recurse -Directory)
if ($actualDirectories.Count -ne $expectedDirectories.Count) {
    throw "Storage-controller driver tree contains missing or extra package directories"
}
foreach ($directory in $actualDirectories) {
    $relativeDirectory = $directory.FullName.Substring($resolvedRoot.Length).TrimStart('\')
    if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        -not $expectedDirectories.Contains($relativeDirectory)) {
        throw "Unexpected storage-controller driver package directory: $($directory.FullName)"
    }
}
if ($actualFiles.Count -ne $expectedFiles.Count) {
    throw "Storage-controller driver tree contains missing or extra files"
}
foreach ($relative in $actualFiles) {
    if (-not $expectedFiles.Contains($relative)) {
        throw "Unexpected storage-controller driver file: $relative"
    }
}

Write-Host "Validated $($manifest.packages.Count) locked storage-controller packages and $($actualFiles.Count) files at $resolvedRoot"
