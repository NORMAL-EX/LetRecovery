[CmdletBinding()]
param(
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

function New-ReleaseTemplate {
    # Keep this deterministic object synchronized with AppConfig::default, InstallPrefs::default,
    # and the serializable fields of AdvancedOptionsData::default. Runtime-only values such as the
    # current username and passwords must never appear in a release package.
    return [pscustomobject][ordered]@{
        easy_mode_enabled = $false
        easy_mode_tip_dismissed = $false
        easy_mode_settings_tip_dismissed = $false
        log_enabled = $true
        log_retention_days = 7
        language = "zh-CN"
        pe_cache = [pscustomobject][ordered]@{
            pe_list = @()
            version = 0
        }
        wim_engine = 0
        # AppConfig no longer serializes this legacy field, but release packages keep an explicit
        # false value so old program versions can never interpret the package as Advanced Mode.
        enable_advanced_options = $false
        automation_export_enabled = $false
        pe_maintenance_entry_enabled = $false
        allow_insecure_http_downloads = $false
        download_threads = 16
        install_prefs = [pscustomobject][ordered]@{
            format_partition = $true
            repair_boot = $true
            unattended_install = $true
            export_drivers = $true
            auto_reboot = $true
            run_diskpart_scripts = $false
            boot_mode = "Auto"
            boot_pca_mode = "Auto"
            driver_action = "AutoImport"
            advanced_options = [pscustomobject][ordered]@{
                remove_shortcut_arrow = $true
                restore_classic_context_menu = $false
                bypass_nro = $true
                disable_windows_update = $false
                disable_windows_defender = $false
                disable_reserved_storage = $true
                disable_uac = $false
                disable_device_encryption = $true
                remove_uwp_apps = $false
                migrate_wifi = $true
                run_script_during_deploy = $false
                deploy_script_path = ""
                run_script_first_login = $false
                first_login_script_path = ""
                import_custom_drivers = $false
                custom_drivers_path = ""
                import_storage_controller_drivers = $false
                import_registry_file = $false
                registry_file_path = ""
                import_custom_files = $false
                custom_files_path = ""
                custom_username = $true
                builtin_administrator = [pscustomobject][ordered]@{
                    enabled = $false
                    account_name = "Administrator"
                    auto_logon = $true
                }
                custom_volume_label = $false
                volume_label = "OS"
                win7_inject_usb3_driver = $false
                win7_usb3_driver_path = ""
                win7_inject_nvme_driver = $false
                win7_nvme_driver_path = ""
                win7_fix_acpi_bsod = $false
                win7_fix_storage_bsod = $false
                win7_uefi_patch = $false
                xp_inject_usb3_driver = $false
                xp_inject_nvme_driver = $false
            }
        }
    }
}

function Assert-ReleaseTemplate {
    param([Parameter(Mandatory = $true)]$Template)

    foreach ($name in @("easy_mode_enabled", "easy_mode_tip_dismissed", "easy_mode_settings_tip_dismissed", "enable_advanced_options", "automation_export_enabled", "pe_maintenance_entry_enabled")) {
        $property = $Template.PSObject.Properties[$name]
        if ($null -eq $property -or $property.Value -ne $false) {
            throw "Release template must explicitly set $name to false"
        }
    }

    $logEnabled = $Template.PSObject.Properties["log_enabled"]
    if ($null -eq $logEnabled -or $logEnabled.Value -ne $true) {
        throw "Release template must explicitly set log_enabled to true"
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

$resolvedPackage = Resolve-JsonFile -Path $PackageConfigPath -Description "Package config"
$template = New-ReleaseTemplate
$package = Read-JsonObject -Path $resolvedPackage -Description "Package config"

Assert-ReleaseTemplate -Template $template
Assert-PeCache -Config $package -EntriesRequired:$RequirePeEntries -HashesRequired:$RequirePeHashes

# Reconstruct the complete release configuration from deterministic CI defaults. The only state
# retained from the external package is PE metadata, because the workflow recalculates those hashes
# for the WIM it has just built. No UI preference, local path, account choice, or dismissed-tip flag
# survives.
$expected = ConvertTo-CompactJson -Value $template | ConvertFrom-Json
$expected.pe_cache = $package.pe_cache
$expectedJson = ($expected | ConvertTo-Json -Depth 100) + "`n"

if ($VerifyOnly) {
    if ((ConvertTo-CompactJson -Value $package) -ne (ConvertTo-CompactJson -Value $expected)) {
        throw "Package config contains values outside the generated release defaults and PE metadata: $resolvedPackage"
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
    Write-Host "Rebuilt release config from generated defaults: $resolvedPackage"
} finally {
    if ($null -ne $temporary -and (Test-Path -LiteralPath $temporary)) {
        Remove-Item -LiteralPath $temporary -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $backup) {
        Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
    }
}
