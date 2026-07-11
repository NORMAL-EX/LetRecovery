param(
    [Parameter(Mandatory = $true)]
    [string] $SourceWim,
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 100)]
    [int] $ImageIndex,
    [Parameter(Mandatory = $true)]
    [ValidateSet("x86", "amd64")]
    [string] $Architecture,
    [Parameter(Mandatory = $true)]
    [string] $OutputWim
)

$ErrorActionPreference = "Stop"
$SourceWim = (Resolve-Path -LiteralPath $SourceWim).Path
$OutputWim = [IO.Path]::GetFullPath($OutputWim)
$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
if (-not $tempRoot) {
    throw "No temporary directory is available"
}
$workspace = Join-Path $tempRoot ("lr-pca2023-" + [guid]::NewGuid().ToString("N"))
$mount = Join-Path $workspace "mount"
$payload = Join-Path $workspace "payload"
$mounted = $false

function Invoke-Dism([string[]] $Arguments) {
    & dism.exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "DISM failed with exit code ${LASTEXITCODE}: $($Arguments -join ' ')"
    }
}

function Get-PeArchitecture([string] $Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 70 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) {
        throw "Not a PE image: $Path"
    }
    $pe = [BitConverter]::ToInt32($bytes, 0x3c)
    if ($pe -lt 0 -or $pe + 6 -gt $bytes.Length -or
        $bytes[$pe] -ne 0x50 -or $bytes[$pe + 1] -ne 0x45 -or
        $bytes[$pe + 2] -ne 0 -or $bytes[$pe + 3] -ne 0) {
        throw "Invalid PE header: $Path"
    }
    switch ([BitConverter]::ToUInt16($bytes, $pe + 4)) {
        0x014c { return "x86" }
        0x8664 { return "amd64" }
        default { throw "Unsupported PE machine in $Path" }
    }
}

function Assert-MicrosoftSignature([string] $Path) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne "Valid" -or
        -not $signature.SignerCertificate.Subject.Contains("O=Microsoft Corporation")) {
        throw "Microsoft signature validation failed: $Path ($($signature.Status))"
    }
}

try {
    New-Item -ItemType Directory -Force -Path $mount, $payload | Out-Null
    Invoke-Dism @(
        "/Mount-Image", "/ImageFile:$SourceWim", "/Index:$ImageIndex",
        "/MountDir:$mount", "/ReadOnly"
    )
    $mounted = $true

    $efiEx = Join-Path $mount "Windows\Boot\EFI_EX"
    $fontsEx = Join-Path $mount "Windows\Boot\FONTS_EX"
    $bootStl = Join-Path $mount "Windows\Boot\EFI\boot.stl"
    $bootmgfw = Join-Path $efiEx "bootmgfw_EX.efi"
    $bootmgr = Join-Path $efiEx "bootmgr_EX.efi"
    foreach ($required in @($efiEx, $fontsEx, $bootmgfw)) {
        if (-not (Test-Path -LiteralPath $required)) {
            throw "Serviced source WIM is missing required BootEx resource: $required"
        }
    }
    Assert-MicrosoftSignature $bootmgfw
    if (Test-Path -LiteralPath $bootmgr) {
        Assert-MicrosoftSignature $bootmgr
    }
    if (Test-Path -LiteralPath $bootStl) {
        Assert-MicrosoftSignature $bootStl
    }
    if ((Get-PeArchitecture $bootmgfw) -ne $Architecture) {
        throw "bootmgfw_EX.efi architecture does not match $Architecture"
    }
    if ((Get-ChildItem -LiteralPath $fontsEx -File -Filter "*_EX.ttf").Count -eq 0) {
        throw "FONTS_EX contains no *_EX.ttf resources"
    }

    $payloadBoot = Join-Path $payload "Windows\Boot"
    New-Item -ItemType Directory -Force -Path (Join-Path $payloadBoot "EFI") | Out-Null
    Copy-Item -LiteralPath $efiEx -Destination $payloadBoot -Recurse -Force
    Copy-Item -LiteralPath $fontsEx -Destination $payloadBoot -Recurse -Force
    if (Test-Path -LiteralPath $bootStl) {
        Copy-Item -LiteralPath $bootStl -Destination (Join-Path $payloadBoot "EFI\boot.stl") -Force
    }

    $outputParent = Split-Path -Parent $OutputWim
    if ($outputParent) {
        New-Item -ItemType Directory -Force -Path $outputParent | Out-Null
    }
    if (Test-Path -LiteralPath $OutputWim) {
        Remove-Item -LiteralPath $OutputWim -Force
    }
    Invoke-Dism @(
        "/Capture-Image", "/ImageFile:$OutputWim", "/CaptureDir:$payload",
        "/Name:LetRecovery PCA2023 $Architecture", "/Compress:max", "/CheckIntegrity"
    )
    Write-Host "Created: $OutputWim"
    Write-Host "SHA-256: $((Get-FileHash -LiteralPath $OutputWim -Algorithm SHA256).Hash)"
}
finally {
    if ($mounted) {
        & dism.exe /Unmount-Image "/MountDir:$mount" /Discard | Out-Host
    }
    $resolvedTemp = [IO.Path]::GetFullPath($tempRoot).TrimEnd('\') + '\'
    $resolvedWorkspace = [IO.Path]::GetFullPath($workspace)
    if ($resolvedWorkspace.StartsWith($resolvedTemp, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedWorkspace -Recurse -Force -ErrorAction SilentlyContinue
    }
}
