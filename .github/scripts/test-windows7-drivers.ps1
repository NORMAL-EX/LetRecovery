param(
    [Parameter(Mandatory = $true)][string]$Root,
    [string]$LockFile = "docs\WINDOWS7_DRIVERS.lock.json",
    [switch]$VerifySignatures
)

$ErrorActionPreference = "Stop"
$resolvedRoot = (Resolve-Path -LiteralPath $Root -ErrorAction Stop).Path
$resolvedLock = (Resolve-Path -LiteralPath $LockFile -ErrorAction Stop).Path
$manifest = Get-Content -LiteralPath $resolvedLock -Raw -Encoding UTF8 | ConvertFrom-Json
if ([int]$manifest.version -ne 1 -or @($manifest.usb3_packages).Count -lt 1) {
    throw "Invalid Windows 7 driver lock manifest: $resolvedLock"
}

function Test-RegularDirectory([string]$Path) {
    $item = Get-Item -LiteralPath $Path -ErrorAction Stop
    if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Driver package root must be a regular directory: $Path"
    }
}

function Test-LockedTree([string]$TreeRoot, $Files, [switch]$CheckSignatures) {
    Test-RegularDirectory $TreeRoot
    $expected = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($file in $Files) {
        $relative = ([string]$file.path).Replace('/', '\')
        if ([IO.Path]::IsPathRooted($relative) -or $relative.Contains('..')) {
            throw "Invalid locked driver path: $relative"
        }
        if (-not $expected.Add($relative)) {
            throw "Duplicate locked driver path: $relative"
        }
        $path = Join-Path $TreeRoot $relative
        $item = Get-Item -LiteralPath $path -ErrorAction Stop
        if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Driver package member must be a regular file: $path"
        }
        if ($item.Length -ne [int64]$file.size) {
            throw "Driver package member size mismatch: $path"
        }
        $hash = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        if (-not $hash.Equals([string]$file.sha256, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Driver package member SHA-256 mismatch: $path"
        }
        $signerProperty = $file.PSObject.Properties['signer_contains']
        if ($CheckSignatures -and $null -ne $signerProperty) {
            $signature = Get-AuthenticodeSignature -LiteralPath $path
            if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
                throw "Driver package signature is not valid: $path ($($signature.Status))"
            }
            $subject = [string]$signature.SignerCertificate.Subject
            if ($subject.IndexOf([string]$signerProperty.Value, [StringComparison]::OrdinalIgnoreCase) -lt 0) {
                throw "Unexpected driver signer for $path`: $subject"
            }
        }
    }
    $actual = @(
        Get-ChildItem -LiteralPath $TreeRoot -Recurse -File | ForEach-Object {
            $_.FullName.Substring($TreeRoot.Length).TrimStart('\')
        }
    )
    if ($actual.Count -ne $expected.Count) {
        throw "Driver tree contains missing or extra files: $TreeRoot"
    }
    foreach ($relative in $actual) {
        if (-not $expected.Contains($relative)) {
            throw "Unexpected driver package member: $TreeRoot\$relative"
        }
    }
}

$usbRoot = Join-Path $resolvedRoot "usb3"
Test-RegularDirectory $usbRoot
$unexpectedUsbRootFiles = @(Get-ChildItem -LiteralPath $usbRoot -Force | Where-Object { -not $_.PSIsContainer })
if ($unexpectedUsbRootFiles.Count -gt 0) {
    throw "Windows 7 USB3 root contains loose files: $usbRoot"
}
$expectedPackages = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($package in $manifest.usb3_packages) {
    $directory = [string]$package.directory
    if ([IO.Path]::GetFileName($directory) -ne $directory -or -not $expectedPackages.Add($directory)) {
        throw "Invalid or duplicate USB3 package directory: $directory"
    }
    $architectures = @($package.architectures)
    if ($architectures.Count -lt 1 -or @($architectures | Where-Object { $_ -notin @('x86', 'amd64') }).Count -gt 0) {
        throw "Invalid architecture set for USB3 package: $directory"
    }
    $packageRoot = Join-Path $usbRoot $directory
    Test-LockedTree $packageRoot @($package.files)
    $infCount = @(Get-ChildItem -LiteralPath $packageRoot -Recurse -File -Filter *.inf).Count
    $catFiles = @(Get-ChildItem -LiteralPath $packageRoot -Recurse -File -Filter *.cat)
    if ($infCount -lt 1 -or $catFiles.Count -lt 1) {
        throw "USB3 package lacks an INF or catalog: $directory"
    }
    if ($VerifySignatures) {
        foreach ($cat in $catFiles) {
            $signature = Get-AuthenticodeSignature -LiteralPath $cat.FullName
            if ($signature.Status -ne [Management.Automation.SignatureStatus]::Valid) {
                throw "USB3 catalog signature is not valid: $($cat.FullName) ($($signature.Status))"
            }
            $subject = [string]$signature.SignerCertificate.Subject
            if ($subject.IndexOf('Microsoft Windows Hardware Compatibility Publisher', [StringComparison]::OrdinalIgnoreCase) -lt 0) {
                throw "USB3 catalog is not WHQL-signed by Microsoft: $($cat.FullName) ($subject)"
            }
        }
    }
}
$actualPackages = @(Get-ChildItem -LiteralPath $usbRoot -Directory)
if ($actualPackages.Count -ne $expectedPackages.Count) {
    throw "USB3 tree contains missing or extra package directories"
}
foreach ($directory in $actualPackages) {
    if (($directory.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        -not $expectedPackages.Contains($directory.Name)) {
        throw "Unexpected USB3 package directory: $($directory.FullName)"
    }
}

$nvmeRoot = Join-Path $resolvedRoot "nvme"
if ([string]$manifest.nvme.architecture -ne 'amd64') {
    throw "The reviewed Windows 7 NVMe update pair must remain amd64-only"
}
Test-LockedTree $nvmeRoot @($manifest.nvme.files) -CheckSignatures:$VerifySignatures
$order = @($manifest.nvme.install_order)
$lockedNvme = @($manifest.nvme.files | ForEach-Object { [string]$_.path })
if ($order.Count -ne 2 -or @(Compare-Object $order $lockedNvme).Count -ne 0) {
    throw "Windows 7 NVMe install order does not match the locked CAB set"
}
Write-Host "Validated $($expectedPackages.Count) Windows 7 USB3 packages and $($lockedNvme.Count) NVMe updates at $resolvedRoot"
