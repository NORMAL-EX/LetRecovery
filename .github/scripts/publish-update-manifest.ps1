#Requires -Version 7.2
<#
.SYNOPSIS
    Publishes the incremental-update manifest through the authenticated release callback.

.DESCRIPTION
    Sends pkg/bin/release-manifest.json byte-for-byte as `lr_app_manifest` with
    `letrecovery_app=true`. Signature: HMAC-SHA256(UPDATE_API_SECRET, timestamp + rand + version +
    md5 + log + can_skip + payload_hash), where payload_hash is the SHA-256 and md5 the MD5 of the
    manifest bytes. The server validates the manifest with the desktop client's rules, refuses to
    replace a newer build_epoch and atomically writes it to
    https://letrecovery.cloud-pe.cn/update/index.json.

    Run this only after publish-update-objects.ps1 succeeded, so clients never receive a manifest
    whose objects are missing.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $ManifestPath,

    [Parameter(Mandatory)]
    [string] $Version
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

foreach ($name in @('UPDATE_API_SECRET', 'LETRECOVERY_UPDATE_API')) {
    if ([string]::IsNullOrWhiteSpace([Environment]::GetEnvironmentVariable($name))) {
        throw "Missing value: $name"
    }
}

function Convert-BytesToHexLower([byte[]] $Bytes) {
    return -join ($Bytes | ForEach-Object { $_.ToString('x2') })
}

$bytes = [IO.File]::ReadAllBytes((Resolve-Path -LiteralPath $ManifestPath).Path)
if ($bytes.Length -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
    throw 'The update manifest must not start with a UTF-8 byte order mark.'
}
# Strict decoding: the server hashes exactly these characters.
$text = [Text.UTF8Encoding]::new($false, $true).GetString($bytes)
$manifest = $text | ConvertFrom-Json
if ([string]$manifest.version -cne $Version) {
    throw "Manifest version '$($manifest.version)' does not match the release version '$Version'."
}

$md5Provider = [Security.Cryptography.MD5]::Create()
try {
    $md5 = (Convert-BytesToHexLower $md5Provider.ComputeHash($bytes)).ToUpperInvariant()
} finally {
    $md5Provider.Dispose()
}
$sha256 = [Security.Cryptography.SHA256]::Create()
try {
    $payloadHash = Convert-BytesToHexLower $sha256.ComputeHash($bytes)
} finally {
    $sha256.Dispose()
}
$timestamp = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds().ToString([Globalization.CultureInfo]::InvariantCulture)
$rand = [guid]::NewGuid().ToString('N')
$log = "Publish LetRecovery $Version incremental update"
$canSkip = 'true'
$hmac = [Security.Cryptography.HMACSHA256]::new([Text.Encoding]::UTF8.GetBytes($env:UPDATE_API_SECRET))
try {
    $signPayload = $timestamp + $rand + $Version + $md5 + $log + $canSkip + $payloadHash
    $sign = Convert-BytesToHexLower ($hmac.ComputeHash([Text.Encoding]::UTF8.GetBytes($signPayload)))
} finally {
    $hmac.Dispose()
}

$body = [ordered]@{
    timestamp       = $timestamp
    rand            = $rand
    version         = $Version
    md5             = $md5
    log             = $log
    can_skip        = $canSkip
    letrecovery_app = $true
    payload_hash    = $payloadHash
    sign            = $sign
    lr_app_manifest = $text
}
$bodyBytes = [Text.UTF8Encoding]::new($false).GetBytes(($body | ConvertTo-Json -Compress))
$headers = @{}
if (-not [string]::IsNullOrWhiteSpace($env:DEPLOY_TOKEN)) {
    $headers['X-Deploy-Token'] = $env:DEPLOY_TOKEN.Trim()
}

$response = Invoke-WebRequest -Method Post -Uri $env:LETRECOVERY_UPDATE_API -Body $bodyBytes `
    -ContentType 'application/json; charset=utf-8' -Headers $headers -TimeoutSec 120 -SkipHttpErrorCheck
$content = [string]$response.Content
try {
    $result = $content | ConvertFrom-Json
} catch {
    throw "Update API returned HTTP $($response.StatusCode) with a non-JSON body: $content"
}
$result | ConvertTo-Json -Depth 6
if ($response.StatusCode -ne 200 -or $result.code -ne 200 -or $result.letrecovery_app_written -ne $true) {
    throw "Update API rejected the incremental update manifest (HTTP $($response.StatusCode))."
}
Write-Host "Published incremental update manifest $Version (build_epoch $($manifest.build_epoch), $($manifest.files.Count) files)."
