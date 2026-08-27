param(
    [string]$Toolchain = '1.88.0'
)

$ErrorActionPreference = 'Stop'

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $scriptRoot '..\..'))
$artifact = Join-Path $repositoryRoot 'target\x86_64-win7-windows-msvc\release\LetRecovery.exe'
$toolchainArgument = "+$Toolchain"

if (-not (Test-Path -LiteralPath (Join-Path $repositoryRoot 'Cargo.toml') -PathType Leaf)) {
    throw "The repository root could not be resolved from $scriptRoot"
}

& rustup run $Toolchain rustc -V | Out-Null
if ($LASTEXITCODE -ne 0) {
    throw "Rust toolchain $Toolchain is not installed. Run: rustup toolchain install $Toolchain --component rust-src"
}

$installedComponents = @(& rustup component list --toolchain $Toolchain --installed)
if ($LASTEXITCODE -ne 0 -or -not ($installedComponents -match '^rust-src(?:-|$)')) {
    throw "rust-src is not installed for Rust $Toolchain. Run: rustup component add rust-src --toolchain $Toolchain"
}

$previousBootstrap = $env:RUSTC_BOOTSTRAP
$previousRustFlags = $env:RUSTFLAGS

Push-Location $repositoryRoot
try {
    # The Win7 target has no prebuilt standard library. Rebuild std for the Win7
    # loader boundary and keep the UCRT inside the executable.
    $env:RUSTC_BOOTSTRAP = '1'
    $env:RUSTFLAGS = '-C target-feature=+crt-static'

    & cargo $toolchainArgument build `
        -p LetRecovery `
        --release `
        --locked `
        --target x86_64-win7-windows-msvc `
        -Z build-std=std,panic_abort
    if ($LASTEXITCODE -ne 0) {
        throw 'The Windows 7 normal-endpoint build failed.'
    }

    if (-not (Test-Path -LiteralPath $artifact -PathType Leaf)) {
        throw "The Windows 7 build completed without producing the expected executable: $artifact"
    }

    & (Join-Path $scriptRoot 'verify-win7-imports.ps1') -Executable $artifact
    if ($LASTEXITCODE -ne 0) {
        throw 'The Windows 7 import-boundary verification failed.'
    }

    Write-Host "Built and verified the Windows 7-11 normal endpoint: $artifact"
}
finally {
    Pop-Location

    if ($null -eq $previousBootstrap) {
        Remove-Item Env:RUSTC_BOOTSTRAP -ErrorAction SilentlyContinue
    }
    else {
        $env:RUSTC_BOOTSTRAP = $previousBootstrap
    }

    if ($null -eq $previousRustFlags) {
        Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
    }
    else {
        $env:RUSTFLAGS = $previousRustFlags
    }
}
