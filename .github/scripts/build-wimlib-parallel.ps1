param(
    [string] $OutputDll,
    [string] $SourceRepository
)

$ErrorActionPreference = "Stop"

$upstreamUrl = "https://wimlib.net/git/wimlib"
$upstreamCommit = "cd5e231c348c255ae5088873b5a66ee0eb96fa07"
$upstreamVersion = "1.14.5"
$repoRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$patch = Join-Path $repoRoot "docs\third-party\wimlib-1.14.5\letrecovery-parallel-decompression.patch"
if (-not $OutputDll) {
    $OutputDll = Join-Path $repoRoot "lr-core\vendor\libwim-15.dll"
}
$OutputDll = [IO.Path]::GetFullPath($OutputDll)

if (-not (Test-Path -LiteralPath $patch -PathType Leaf)) {
    throw "wimlib patch is missing: $patch"
}
if (-not (Get-Command git.exe -ErrorAction SilentlyContinue)) {
    throw "git.exe is required"
}
if (-not (Get-Command wsl.exe -ErrorAction SilentlyContinue)) {
    throw "WSL is required"
}

$tempRoot = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
if (-not $tempRoot) {
    throw "No temporary directory is available"
}
$workspace = Join-Path $tempRoot ("lr-wimlib-" + [guid]::NewGuid().ToString("N"))
$sourceClone = Join-Path $workspace "upstream"
$sourceArchive = Join-Path $workspace "wimlib.tar"
$sourceTree = Join-Path $workspace "source"

function Invoke-Git([string[]] $Arguments) {
    & git.exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "git failed with exit code ${LASTEXITCODE}: $($Arguments -join ' ')"
    }
}

function Convert-ToWslPath([string] $Path) {
    $portablePath = $Path.Replace("\", "/")
    $converted = (& wsl.exe wslpath -a $portablePath)
    if ($LASTEXITCODE -ne 0 -or -not $converted) {
        throw "wslpath failed for $Path"
    }
    return $converted.Trim()
}

function Quote-Sh([string] $Value) {
    return "'" + $Value.Replace("'", "'""'""'") + "'"
}

function Get-PeMachine([string] $Path) {
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 70 -or $bytes[0] -ne 0x4d -or $bytes[1] -ne 0x5a) {
        throw "Not a PE image: $Path"
    }
    $peOffset = [BitConverter]::ToInt32($bytes, 0x3c)
    if ($peOffset -lt 0 -or $peOffset + 6 -gt $bytes.Length -or
        $bytes[$peOffset] -ne 0x50 -or $bytes[$peOffset + 1] -ne 0x45 -or
        $bytes[$peOffset + 2] -ne 0 -or $bytes[$peOffset + 3] -ne 0) {
        throw "Invalid PE header: $Path"
    }
    return [BitConverter]::ToUInt16($bytes, $peOffset + 4)
}

try {
    New-Item -ItemType Directory -Force -Path $workspace, $sourceTree | Out-Null

    if ($SourceRepository) {
        $sourceClone = (Resolve-Path -LiteralPath $SourceRepository).Path
    } else {
        Invoke-Git @("clone", "--no-checkout", $upstreamUrl, $sourceClone)
        Invoke-Git @("-C", $sourceClone, "checkout", "--detach", $upstreamCommit)
    }

    $actualCommit = (& git.exe -c "safe.directory=$sourceClone" -C $sourceClone rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $actualCommit -ne $upstreamCommit) {
        throw "Expected upstream commit $upstreamCommit, got $actualCommit"
    }

    Invoke-Git @(
        "-c", "safe.directory=$sourceClone",
        "-C", $sourceClone,
        "archive", "--format=tar", "--output=$sourceArchive", $upstreamCommit
    )
    & tar.exe -xf $sourceArchive -C $sourceTree
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to extract the pinned wimlib source archive"
    }

    Push-Location $sourceTree
    try {
        & git.exe -c core.autocrlf=false -c core.eol=lf apply --check $patch
        if ($LASTEXITCODE -ne 0) {
            throw "The LetRecovery wimlib patch does not apply cleanly"
        }
        & git.exe -c core.autocrlf=false -c core.eol=lf apply $patch
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to apply the LetRecovery wimlib patch"
        }
    } finally {
        Pop-Location
    }

    $wslSource = Convert-ToWslPath $sourceTree
    $buildCommand = "cd $(Quote-Sh $wslSource) && tools/windows-build.sh --arch=x86_64 -- --without-fuse --without-ntfs-3g"
    & wsl.exe sh -lc $buildCommand
    if ($LASTEXITCODE -ne 0) {
        throw "The wimlib MinGW build failed with exit code $LASTEXITCODE"
    }

    $builtDll = Join-Path $sourceTree "wimlib-$upstreamVersion-windows-x86_64-bin\libwim-15.dll"
    if (-not (Test-Path -LiteralPath $builtDll -PathType Leaf)) {
        throw "The wimlib build did not produce $builtDll"
    }
    if ((Get-PeMachine $builtDll) -ne 0x8664) {
        throw "The built DLL is not x86_64"
    }

    $wslDll = Convert-ToWslPath $builtDll
    $exportCheck = (& wsl.exe sh -lc "x86_64-w64-mingw32-objdump -p $(Quote-Sh $wslDll) | grep -F wimlib_set_parallel_decompression")
    if ($LASTEXITCODE -ne 0 -or -not $exportCheck) {
        throw "The built DLL does not export wimlib_set_parallel_decompression"
    }

    $outputParent = Split-Path -Parent $OutputDll
    New-Item -ItemType Directory -Force -Path $outputParent | Out-Null
    $stagedDll = Join-Path $outputParent ("libwim-15." + [guid]::NewGuid().ToString("N") + ".tmp")
    Copy-Item -LiteralPath $builtDll -Destination $stagedDll
    if ((Get-FileHash -LiteralPath $builtDll -Algorithm SHA256).Hash -ne
        (Get-FileHash -LiteralPath $stagedDll -Algorithm SHA256).Hash) {
        throw "The staged DLL differs from the validated build output"
    }
    Move-Item -LiteralPath $stagedDll -Destination $OutputDll -Force

    $hash = (Get-FileHash -LiteralPath $OutputDll -Algorithm SHA256).Hash
    $size = (Get-Item -LiteralPath $OutputDll).Length
    Write-Host "Built $OutputDll"
    Write-Host "Size: $size bytes"
    Write-Host "SHA-256: $hash"
    Write-Host "Patch SHA-256: $((Get-FileHash -LiteralPath $patch -Algorithm SHA256).Hash)"
} finally {
    if (Test-Path -LiteralPath $workspace) {
        Remove-Item -LiteralPath $workspace -Recurse -Force
    }
}
