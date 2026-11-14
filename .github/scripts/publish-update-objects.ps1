#Requires -Version 7.2
<#
.SYNOPSIS
    Uploads the content-addressed incremental-update objects to Cloudflare R2.

.DESCRIPTION
    Every file in -ObjectsDirectory is named by the lowercase SHA-256 of its content (produced by
    `release_update_tool generate`, lr-core/examples/release_update_tool.rs). Objects are
    immutable: an object that already exists under the prefix is skipped (rclone
    --ignore-existing) and nothing is ever overwritten or deleted, so clients that are updating
    while a release is published keep downloading consistent bytes. After the upload one listing
    of the prefix proves that every local object exists remotely with the same size; the desktop
    updater additionally verifies size and SHA-256 of every object it downloads.

    Uses the same pinned rclone and environment-only R2 remote as upload-to-r2.ps1.

.PARAMETER ObjectsDirectory
    Directory produced by `release_update_tool generate`.

.PARAMETER Prefix
    Folder inside the bucket, for example "LetRecovery/update/objects".

.PARAMETER PublicBaseUrl
    Optional public download base. A sample of the objects is checked with HEAD requests; a
    mismatch only produces a warning.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $ObjectsDirectory,

    [Parameter(Mandatory)]
    [string] $Prefix,

    [string] $PublicBaseUrl = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PSNativeCommandUseErrorActionPreference = $false
$ProgressPreference = 'SilentlyContinue'

. (Join-Path $PSScriptRoot 'r2-rclone.ps1')

$normalizedPrefix = Get-NormalizedPrefix -Value $Prefix
$endpoint = Get-R2Endpoint
$bucket = Get-R2Bucket
$accessKeyId = Get-RequiredSetting -Name 'R2_ACCESS_KEY_ID'
$secretAccessKey = Get-RequiredSetting -Name 'R2_SECRET_ACCESS_KEY'

if (-not (Test-Path -LiteralPath $ObjectsDirectory -PathType Container)) {
    throw "Objects directory does not exist: $ObjectsDirectory"
}
$objects = @(Get-ChildItem -LiteralPath $ObjectsDirectory -Force)
foreach ($object in $objects) {
    if ($object.PSIsContainer -or $object.Name -cnotmatch '^[0-9a-f]{64}$') {
        throw "Unexpected entry in the objects directory: $($object.Name)"
    }
}
if ($objects.Count -eq 0) {
    throw 'The objects directory is empty.'
}
$totalBytes = [long](($objects | Measure-Object -Property Length -Sum).Sum)

$workDirectory = Get-WorkDirectory
$rclone = Get-Rclone -WorkDirectory $workDirectory
& $rclone version | Select-Object -First 1 | Out-Host
Set-R2RemoteEnvironment -WorkDirectory $workDirectory -Endpoint $endpoint -AccessKeyId $accessKeyId -SecretAccessKey $secretAccessKey
$destination = "$($RemoteName):$bucket/$normalizedPrefix"

Write-Host "Uploading $($objects.Count) content-addressed object(s), $totalBytes bytes, to $normalizedPrefix/ (objects that already exist are skipped)"
& $rclone copy $ObjectsDirectory $destination `
    --ignore-existing `
    --header-upload 'Content-Type: application/octet-stream' `
    --header-upload 'Cache-Control: public, max-age=31536000, immutable' `
    --transfers 16 `
    --checkers 32 `
    --retries 5 `
    --retries-sleep 10s `
    --low-level-retries 20 `
    --stats 30s `
    --stats-one-line `
    --stats-log-level NOTICE `
    --log-level INFO | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw "rclone copy to $normalizedPrefix/ failed with exit code $LASTEXITCODE"
}

$listing = & $rclone lsjson --files-only --no-modtime --no-mimetype --low-level-retries 10 --log-level ERROR $destination
if ($LASTEXITCODE -ne 0) {
    throw "Could not list $normalizedPrefix/ (rclone exit code $LASTEXITCODE)"
}
$remoteSizes = @{}
foreach ($entry in @(($listing -join "`n") | ConvertFrom-Json)) {
    $remoteSizes[[string]$entry.Path] = [long]$entry.Size
}
$problems = [System.Collections.Generic.List[string]]::new()
foreach ($object in $objects) {
    if (-not $remoteSizes.ContainsKey($object.Name)) {
        $problems.Add("$($object.Name) is missing")
    } elseif ($remoteSizes[$object.Name] -ne $object.Length) {
        $problems.Add("$($object.Name) has $($remoteSizes[$object.Name]) bytes, expected $($object.Length)")
    }
}
if ($problems.Count -gt 0) {
    throw "R2 update objects failed verification: $(@($problems | Select-Object -First 10) -join '; ')"
}
Write-Host "Verified $($objects.Count) object(s) under $normalizedPrefix/ ($($remoteSizes.Count) objects in the prefix)."

$publicBase = $PublicBaseUrl.Trim().TrimEnd('/')
if (-not [string]::IsNullOrEmpty($publicBase)) {
    foreach ($object in @($objects | Sort-Object -Property Length | Select-Object -First 3)) {
        $url = "$publicBase/$normalizedPrefix/$($object.Name)"
        try {
            $response = Invoke-WebRequest -Method Head -Uri $url -TimeoutSec 60 -MaximumRedirection 5 -SkipHttpErrorCheck
            if ($response.StatusCode -eq 200) {
                Write-Host "Public object reachable: $url"
            } else {
                Write-Host "::warning::$url returned HTTP $($response.StatusCode)"
            }
        } catch {
            Write-Host "::warning::$url is not reachable: $($_.Exception.Message)"
        }
    }
}

if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_STEP_SUMMARY)) {
    $summary = @(
        "### Incremental update objects: $normalizedPrefix/",
        '',
        "- Objects in this release: $($objects.Count) ($totalBytes bytes, existing objects skipped)",
        "- Objects now in the prefix: $($remoteSizes.Count)"
    )
    Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value $summary -Encoding utf8
}

# GitHub's pwsh wrapper exits with $LASTEXITCODE; do not leak a probe's exit code.
$global:LASTEXITCODE = 0
