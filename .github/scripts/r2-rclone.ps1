# Shared by upload-to-r2.ps1 and publish-update-objects.ps1 (dot-sourced into their script
# scope): the pinned, hash-verified rclone download, R2 settings validation and the R2 remote,
# which is configured only through environment variables of the current process.

# Pinned rclone release (published 2026-09-04). The hashes come from
# https://downloads.rclone.org/v1.75.1/SHA256SUMS and were checked against both mirrors.
$RcloneVersion = 'v1.75.1'
$RcloneSha256 = @{
    'windows-amd64' = '200eb602c126d82aa38b51e0f6b9ae837473ff99b51278d3f6f837574c494d6e'
    'linux-amd64'   = '982b5aa772841168f8e380f139e9e787b2a105403e32b94da8676a0e1c0a13ab'
}

$RemoteName = 'R2PUBLISH'

function Get-NormalizedPrefix {
    param([Parameter(Mandatory)][string] $Value)

    $normalized = ($Value -replace '\\', '/').Trim().Trim('/')
    if ([string]::IsNullOrWhiteSpace($normalized)) {
        throw 'The R2 folder prefix must not be empty.'
    }
    foreach ($segment in $normalized.Split('/')) {
        if ([string]::IsNullOrWhiteSpace($segment) -or
            $segment -eq '.' -or
            $segment -eq '..' -or
            $segment -ne $segment.Trim()) {
            throw "Invalid R2 folder prefix: '$Value'"
        }
    }
    return $normalized
}

function Get-RequiredSetting {
    param([Parameter(Mandatory)][string] $Name)

    $value = [Environment]::GetEnvironmentVariable($Name)
    if ([string]::IsNullOrWhiteSpace($value)) {
        throw "Missing required secret or environment variable: $Name"
    }
    return $value.Trim()
}

function Get-WorkDirectory {
    $base = if (-not [string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
        $env:RUNNER_TEMP
    } else {
        [System.IO.Path]::GetTempPath()
    }
    $directory = Join-Path $base 'r2-upload'
    New-Item -ItemType Directory -Force -Path $directory | Out-Null
    return $directory
}

function Get-Rclone {
    param([Parameter(Mandatory)][string] $WorkDirectory)

    if (-not [string]::IsNullOrWhiteSpace($env:R2_RCLONE_EXE)) {
        if (-not (Test-Path -LiteralPath $env:R2_RCLONE_EXE -PathType Leaf)) {
            throw "R2_RCLONE_EXE does not point to a file: $env:R2_RCLONE_EXE"
        }
        return (Resolve-Path -LiteralPath $env:R2_RCLONE_EXE).ProviderPath
    }

    $architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    if ($architecture -ne [System.Runtime.InteropServices.Architecture]::X64) {
        throw "No pinned rclone build for architecture $architecture; set R2_RCLONE_EXE."
    }
    if ($IsWindows) {
        $platform = 'windows-amd64'
        $exeName = 'rclone.exe'
    } elseif ($IsLinux) {
        $platform = 'linux-amd64'
        $exeName = 'rclone'
    } else {
        throw 'No pinned rclone build for this operating system; set R2_RCLONE_EXE.'
    }

    $folder = "rclone-$RcloneVersion-$platform"
    $exe = Join-Path (Join-Path $WorkDirectory $folder) $exeName
    if (Test-Path -LiteralPath $exe -PathType Leaf) {
        return $exe
    }

    $zip = Join-Path $WorkDirectory "$folder.zip"
    $urls = @(
        "https://github.com/rclone/rclone/releases/download/$RcloneVersion/$folder.zip",
        "https://downloads.rclone.org/$RcloneVersion/$folder.zip"
    )
    foreach ($url in $urls) {
        try {
            Write-Host "Downloading rclone $RcloneVersion from $url"
            Invoke-WebRequest -Uri $url -OutFile $zip -MaximumRetryCount 3 -RetryIntervalSec 5 -TimeoutSec 600
            $actual = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($actual -ne $RcloneSha256[$platform]) {
                throw "SHA-256 mismatch: expected $($RcloneSha256[$platform]), got $actual"
            }
            Expand-Archive -LiteralPath $zip -DestinationPath $WorkDirectory -Force
            if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
                throw "$exeName is missing from the archive"
            }
            if (-not $IsWindows) {
                & chmod '+x' $exe
                if ($LASTEXITCODE -ne 0) {
                    throw "chmod failed for $exe"
                }
            }
            return $exe
        } catch {
            Write-Warning "rclone download from $url failed: $($_.Exception.Message)"
        } finally {
            Remove-Item -LiteralPath $zip -Force -ErrorAction SilentlyContinue
        }
    }
    throw "Could not download a verified rclone $RcloneVersion."
}

function Get-R2Endpoint {
    $endpoint = (Get-RequiredSetting -Name 'R2_ENDPOINT').TrimEnd('/')
    $endpointUri = $null
    if (-not [Uri]::TryCreate($endpoint, [UriKind]::Absolute, [ref]$endpointUri) -or
        -not ($endpointUri.Scheme -eq 'https' -or ($endpointUri.Scheme -eq 'http' -and $endpointUri.IsLoopback)) -or
        $endpointUri.AbsolutePath -ne '/' -or
        -not [string]::IsNullOrEmpty($endpointUri.Query)) {
        throw 'R2_ENDPOINT must be the account endpoint without bucket or path, for example https://<account_id>.r2.cloudflarestorage.com'
    }
    return $endpoint
}

function Get-R2Bucket {
    $bucket = (Get-RequiredSetting -Name 'R2_BUCKET').Trim('/')
    if ($bucket -cnotmatch '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$') {
        throw 'R2_BUCKET is not a valid R2 bucket name (3-63 characters: a-z, 0-9 and -).'
    }
    return $bucket
}

function Set-R2RemoteEnvironment {
    param(
        [Parameter(Mandatory)][string] $WorkDirectory,
        [Parameter(Mandatory)][string] $Endpoint,
        [Parameter(Mandatory)][string] $AccessKeyId,
        [Parameter(Mandatory)][string] $SecretAccessKey
    )

    # An empty config file keeps a runner-wide rclone.conf out of the way; the remote is
    # defined only through environment variables of this process.
    $rcloneConfig = Join-Path $WorkDirectory 'rclone.conf'
    Set-Content -LiteralPath $rcloneConfig -Value '' -Encoding ascii
    $env:RCLONE_CONFIG = $rcloneConfig
    $env:RCLONE_CONFIG_R2PUBLISH_TYPE = 's3'
    $env:RCLONE_CONFIG_R2PUBLISH_PROVIDER = 'Cloudflare'
    $env:RCLONE_CONFIG_R2PUBLISH_ENV_AUTH = 'false'
    $env:RCLONE_CONFIG_R2PUBLISH_ACCESS_KEY_ID = $AccessKeyId
    $env:RCLONE_CONFIG_R2PUBLISH_SECRET_ACCESS_KEY = $SecretAccessKey
    $env:RCLONE_CONFIG_R2PUBLISH_ENDPOINT = $Endpoint
    $env:RCLONE_CONFIG_R2PUBLISH_REGION = 'auto'
    # Bucket-scoped "Object Read & Write" tokens may not check or create buckets.
    $env:RCLONE_CONFIG_R2PUBLISH_NO_CHECK_BUCKET = 'true'
}
