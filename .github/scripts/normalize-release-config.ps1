[CmdletBinding()]
param(
    [string]$TemplatePath = "config.json",
    [string]$PackageConfigPath = "pkg\config.json",
    [switch]$RequirePeEntries,
    [switch]$RequirePeHashes,
    [switch]$VerifyOnly
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Resolve-JsonFile {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description does not exist: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

function Read-JsonObject {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,
        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    try {
        $value = Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json
    } catch {
        throw "$Description is not valid UTF-8 JSON: $Path ($($_.Exception.Message))"
    }
    if ($null -eq $value -or $value -isnot [pscustomobject]) {
        throw "$Description must contain a JSON object: $Path"
    }
    return $value
}

function ConvertTo-CompactJson {
    param([Parameter(Mandatory = $true)]$Value)
    return ($Value | ConvertTo-Json -Depth 100 -Compress)
}

function Assert-ReleaseTemplate {
    param([Parameter(Mandatory = $true)]$Template)

    foreach ($name in @("easy_mode_enabled", "easy_mode_tip_dismissed", "easy_mode_settings_tip_dismissed", "enable_advanced_options")) {
        $property = $Template.PSObject.Properties[$name]
        if ($null -eq $property -or $property.Value -ne $false) {
            throw "Release template must explicitly set $name to false"
        }
    }
    if ($null -eq $Template.PSObject.Properties["install_prefs"] -or $null -eq $Template.install_prefs) {
        throw "Release template is missing install_prefs"
    }
    if ($null -eq $Template.install_prefs.PSObject.Properties["advanced_options"] -or $null -eq $Template.install_prefs.advanced_options) {
        throw "Release template is missing install_prefs.advanced_options"
    }

    $advanced = $Template.install_prefs.advanced_options
    foreach ($name in @("deploy_script_path", "first_login_script_path", "custom_drivers_path", "registry_file_path", "custom_files_path", "username")) {
        $property = $advanced.PSObject.Properties[$name]
        if ($null -ne $property -and -not [string]::IsNullOrEmpty([string]$property.Value)) {
            throw "Release template contains a machine-specific value in install_prefs.advanced_options.$name"
        }
    }
}

function Assert-PeCache {
    param(
        [Parameter(Mandatory = $true)]$Config,
        [switch]$EntriesRequired,
        [switch]$HashesRequired
    )

    if ($null -eq $Config.PSObject.Properties["pe_cache"] -or $null -eq $Config.pe_cache) {
        throw "Package config is missing pe_cache"
    }
    if ($null -eq $Config.pe_cache.PSObject.Properties["pe_list"]) {
        throw "Package config is missing pe_cache.pe_list"
    }
    $entries = @($Config.pe_cache.pe_list)
    if ($EntriesRequired -and $entries.Count -eq 0) {
        throw "Package config contains no PE entries"
    }
    foreach ($entry in $entries) {
        $filename = [string]$entry.filename
        if ([string]::IsNullOrWhiteSpace($filename) -or [IO.Path]::GetFileName($filename) -ne $filename) {
            throw "Package config contains an unsafe PE filename: $filename"
        }
        if ($HashesRequired) {
            $md5 = [string]$entry.md5
            $sha256 = [string]$entry.sha256
            if ($md5 -notmatch '^[0-9A-Fa-f]{32}$') {
                throw "Package config contains an invalid PE MD5 for $filename"
            }
            if ($sha256 -notmatch '^[0-9A-Fa-f]{64}$') {
                throw "Package config contains an invalid PE SHA-256 for $filename"
            }
        }
    }
}

$resolvedTemplate = Resolve-JsonFile -Path $TemplatePath -Description "Tracked release config template"
$resolvedPackage = Resolve-JsonFile -Path $PackageConfigPath -Description "Package config"
$template = Read-JsonObject -Path $resolvedTemplate -Description "Tracked release config template"
$package = Read-JsonObject -Path $resolvedPackage -Description "Package config"

Assert-ReleaseTemplate -Template $template
Assert-PeCache -Config $package -EntriesRequired:$RequirePeEntries -HashesRequired:$RequirePeHashes

# Reconstruct the complete release configuration from the tracked template. The only state retained
# from the external package is PE metadata, because the workflow recalculates those hashes for the
# WIM it has just built. No UI preference, local path, account choice, or dismissed-tip flag survives.
$expected = ConvertTo-CompactJson -Value $template | ConvertFrom-Json
$expected.pe_cache = $package.pe_cache
$expectedJson = ($expected | ConvertTo-Json -Depth 100) + "`n"

if ($VerifyOnly) {
    if ((ConvertTo-CompactJson -Value $package) -ne (ConvertTo-CompactJson -Value $expected)) {
        throw "Package config contains values outside the tracked release defaults and PE metadata: $resolvedPackage"
    }
    Write-Host "Verified release config defaults: $resolvedPackage"
    return
}

$directory = Split-Path -Parent $resolvedPackage
$temporary = Join-Path $directory ("config.json.normalize-" + [guid]::NewGuid().ToString("N") + ".tmp")
$backup = Join-Path $directory ("config.json.normalize-" + [guid]::NewGuid().ToString("N") + ".bak")
try {
    [IO.File]::WriteAllText($temporary, $expectedJson, [Text.UTF8Encoding]::new($false))
    $written = Read-JsonObject -Path $temporary -Description "Normalized temporary package config"
    if ((ConvertTo-CompactJson -Value $written) -ne (ConvertTo-CompactJson -Value $expected)) {
        throw "Normalized temporary package config failed its read-back check"
    }

    [IO.File]::Replace($temporary, $resolvedPackage, $backup, $true)
    $temporary = $null
    $final = Read-JsonObject -Path $resolvedPackage -Description "Normalized package config"
    if ((ConvertTo-CompactJson -Value $final) -ne (ConvertTo-CompactJson -Value $expected)) {
        throw "Normalized package config failed its final read-back check"
    }
    Assert-PeCache -Config $final -EntriesRequired:$RequirePeEntries -HashesRequired:$RequirePeHashes
    Write-Host "Rebuilt release config from tracked defaults: $resolvedPackage"
} finally {
    if ($null -ne $temporary -and (Test-Path -LiteralPath $temporary)) {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $backup) {
        Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
    }
}
