param(
    [string]$Root = "assets\release\bin\drivers",
    [string]$LockFile = "docs\WINDOWS7_DRIVERS.lock.json"
)

$ErrorActionPreference = "Stop"
$resolvedRoot = (Resolve-Path -LiteralPath $Root -ErrorAction Stop).Path
$architectureMap = [ordered]@{
    "amd30h"      = @("x86", "amd64")
    "amd30ryzen"  = @("amd64")
    "amd3110013"  = @("amd64")
    "amd311053"   = @("x86", "amd64")
    "ASMedia"     = @("x86", "amd64")
    "etron"       = @("x86", "amd64")
    "intel7th"    = @("amd64")
    "intel8th+"   = @("amd64")
    "inteltitan"  = @("amd64")
    "RenesasGen1" = @("amd64")
    "RenesasGen2" = @("amd64")
    "Texas"       = @("x86", "amd64")
    "via"         = @("amd64")
}

function Get-LockedFile([string]$Path, [string]$Base) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if (-not $item.PSIsContainer -and -not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        $relative = $item.FullName.Substring($Base.Length).TrimStart('\').Replace('\', '/')
        return [ordered]@{
            path = $relative
            size = [int64]$item.Length
            sha256 = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash.ToUpperInvariant()
        }
    }
    throw "Driver package member is not a regular file: $Path"
}

$usbRoot = Join-Path $resolvedRoot "usb3"
$nvmeRoot = Join-Path $resolvedRoot "nvme"
$actualPackages = @(Get-ChildItem -LiteralPath $usbRoot -Directory | Sort-Object Name)
if (@(Compare-Object @($architectureMap.Keys) @($actualPackages.Name)).Count -ne 0) {
    throw "USB3 package set does not match the reviewed architecture map"
}

$usbPackages = foreach ($package in $actualPackages) {
    $files = @(
        Get-ChildItem -LiteralPath $package.FullName -Recurse -File |
            Sort-Object FullName |
            ForEach-Object { Get-LockedFile $_.FullName $package.FullName }
    )
    [ordered]@{
        directory = $package.Name
        architectures = @($architectureMap[$package.Name])
        files = $files
    }
}

$nvmeFiles = @(
    Get-ChildItem -LiteralPath $nvmeRoot -File |
        Sort-Object Name |
        ForEach-Object {
            $locked = Get-LockedFile $_.FullName $nvmeRoot
            $locked.signer_contains = "Microsoft Corporation"
            $locked
        }
)
$installOrder = @(
    "Windows6.1-KB2990941-v3-x64.cab",
    "Windows6.1-KB3087873-v2-x64.cab"
)
if (@(Compare-Object $installOrder @($nvmeFiles.path)).Count -ne 0) {
    throw "NVMe package set does not match the reviewed Microsoft update pair"
}

$manifest = [ordered]@{
    version = 1
    source = "User-provided legacy LetRecovery.7z; unsafe or non-kernel-policy USB3 packages excluded"
    usb3_packages = @($usbPackages)
    nvme = [ordered]@{
        architecture = "amd64"
        install_order = $installOrder
        files = $nvmeFiles
    }
}
$json = $manifest | ConvertTo-Json -Depth 8
$destination = [IO.Path]::GetFullPath((Join-Path (Get-Location) $LockFile))
$parent = Split-Path -Parent $destination
if (-not (Test-Path -LiteralPath $parent -PathType Container)) {
    throw "Lock-file parent directory does not exist: $parent"
}
[IO.File]::WriteAllText($destination, $json + "`n", [Text.UTF8Encoding]::new($false))
Write-Host "Updated Windows 7 driver lock: $destination"
