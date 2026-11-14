#Requires -Version 7.2
<#
.SYNOPSIS
    Publishes build outputs to a Cloudflare R2 folder with a rollback-safe overwrite.

.DESCRIPTION
    R2 is S3-compatible object storage. Folders are only key prefixes, so a folder that
    does not exist yet is created by the first upload into it, and there is no native
    rename: renaming an object means a server-side copy followed by a delete.

    One invocation is one publish transaction for every file passed in -Path:

      1. Each local file is renamed to "<name>.r2-uploading-<run>".
      2. Each renamed file is uploaded to "<prefix>/<name>.r2-uploading-<run>" and the
         uploaded object is verified (size and MD5).
      3. Each object that already exists at "<prefix>/<name>" is copied server-side to
         "<prefix>/<name>.r2-backup-<run>" and the copy is verified, so the old file can
         always be restored.
      4. Each uploaded object is renamed back to "<prefix>/<name>". The server-side copy
         replaces the old object in a single step, so the public key never disappears.
         The result is verified again.
      5. The temporary objects and the backups (the old files) are deleted and the local
         files get their original names back.

    Downloads therefore keep getting the previous file until step 4 switches the key to
    the new one, and the previous file is gone from R2 once the run succeeds. Objects are
    stored with "Cache-Control: no-cache" so a CDN in front of the bucket (for example an
    R2 custom domain) revalidates instead of serving the previous file from its cache.

    If anything in steps 1 to 4 fails, every file is rolled back: public objects are
    restored from their backups (or removed again when they did not exist before),
    temporary objects and backups are deleted, local names are restored and the script
    fails. Problems while deleting leftovers in step 5 only produce warnings, because
    the new files are already published and verified at that point.

    Required environment variables (normally GitHub Actions secrets):
      R2_ENDPOINT           https://<account_id>.r2.cloudflarestorage.com
      R2_BUCKET             Bucket name
      R2_ACCESS_KEY_ID      Access key ID of an R2 API token with Object Read & Write
      R2_SECRET_ACCESS_KEY  Secret access key of that token

    Optional:
      R2_RCLONE_EXE         Use this rclone binary instead of the pinned download.

.PARAMETER PublicBaseUrl
    Optional public download base, for example https://r2.cloud-pe.cn. After publishing,
    every public URL is checked with a HEAD request; a mismatch only produces a warning.

.PARAMETER Prefix
    Folder inside the bucket, for example "LetRecovery" or "Cloud-PE/Cloud-MGR".

.PARAMETER Path
    Local files to publish. The file names become the object names.

.EXAMPLE
    ./upload-to-r2.ps1 -Prefix LetRecovery -Path .\LetRecovery.7z, .\installer\output\LetRecovery-Setup-x64.exe -PublicBaseUrl https://r2.cloud-pe.cn
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $Prefix,

    [Parameter(Mandatory)]
    [string[]] $Path,

    [string] $PublicBaseUrl = ''
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$PSNativeCommandUseErrorActionPreference = $false
$ProgressPreference = 'SilentlyContinue'

# Pinned rclone, R2 settings validation and the environment-only remote are shared with
# publish-update-objects.ps1.
. (Join-Path $PSScriptRoot 'r2-rclone.ps1')

$UploadMarker = '.r2-uploading-'
$BackupMarker = '.r2-backup-'

# Flags for transfers. rclone stays quiet about the secrets because the remote is
# configured through environment variables.
$RcloneTransferFlags = @(
    '--retries', '5',
    '--retries-sleep', '10s',
    '--low-level-retries', '20',
    '--s3-chunk-size', '64Mi',
    '--s3-upload-concurrency', '4',
    '--stats', '30s',
    '--stats-one-line',
    '--stats-log-level', 'NOTICE',
    '--log-level', 'INFO'
)
$RcloneQueryFlags = @(
    '--low-level-retries', '10',
    '--log-level', 'ERROR'
)

function Get-ContentType {
    param([Parameter(Mandatory)][string] $FileName)

    switch ([System.IO.Path]::GetExtension($FileName).ToLowerInvariant()) {
        '.7z' { return 'application/x-7z-compressed' }
        '.zip' { return 'application/zip' }
        '.exe' { return 'application/vnd.microsoft.portable-executable' }
        '.msi' { return 'application/x-msi' }
        '.iso' { return 'application/x-iso9660-image' }
        '.json' { return 'application/json' }
        '.txt' { return 'text/plain; charset=utf-8' }
        default { return 'application/octet-stream' }
    }
}

function Get-RemoteSpec {
    param([Parameter(Mandatory)][string] $Key)

    return "$($script:RemoteName):$($script:Bucket)/$Key"
}

function Invoke-RcloneTransfer {
    param([Parameter(Mandatory)][string[]] $Arguments)

    & $script:Rclone @Arguments @RcloneTransferFlags | Out-Host
    return $LASTEXITCODE
}

function Invoke-RcloneQuery {
    param([Parameter(Mandatory)][string[]] $Arguments)

    # stdout carries the JSON or the listing; stderr carries rclone's log lines.
    $previousPreference = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $output = & $script:Rclone @Arguments @RcloneQueryFlags 2>&1
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousPreference
    }

    $stdout = [System.Collections.Generic.List[string]]::new()
    $stderr = [System.Collections.Generic.List[string]]::new()
    foreach ($line in @($output)) {
        if ($null -eq $line) {
            continue
        }
        if ($line -is [System.Management.Automation.ErrorRecord]) {
            $stderr.Add($line.ToString())
        } else {
            $stdout.Add([string]$line)
        }
    }
    return [pscustomobject]@{
        ExitCode = $exitCode
        Stdout   = ($stdout -join "`n")
        Stderr   = ($stderr -join ' | ')
    }
}

function Get-RemoteObject {
    # Returns $null when no object exists at the key, otherwise its size and MD5.
    param([Parameter(Mandatory)][string] $Key)

    $result = Invoke-RcloneQuery -Arguments @('lsjson', '--stat', '--hash-type', 'md5', (Get-RemoteSpec $Key))
    if ($result.ExitCode -eq 3 -or $result.ExitCode -eq 4) {
        return $null
    }
    if ($result.ExitCode -ne 0) {
        throw "Could not query R2 object '$Key' (rclone exit code $($result.ExitCode)): $($result.Stderr)"
    }

    $json = $result.Stdout.Trim()
    if ([string]::IsNullOrEmpty($json)) {
        return $null
    }
    $entry = $json | ConvertFrom-Json
    # On bucket storage every missing key also looks like an (empty) directory.
    if ($entry.IsDir) {
        return $null
    }

    $md5 = ''
    $hashes = $entry.PSObject.Properties['Hashes']
    if ($null -ne $hashes -and $null -ne $hashes.Value) {
        $md5Property = $hashes.Value.PSObject.Properties['md5']
        if ($null -ne $md5Property -and $null -ne $md5Property.Value) {
            $md5 = ([string]$md5Property.Value).ToLowerInvariant()
        }
    }
    return [pscustomobject]@{
        Size = [long]$entry.Size
        Md5  = $md5
    }
}

function Assert-RemoteObject {
    param(
        [Parameter(Mandatory)][string] $Key,
        [Parameter(Mandatory)][long] $Size,
        [AllowEmptyString()][string] $Md5,
        [Parameter(Mandatory)][string] $Label
    )

    $remote = Get-RemoteObject -Key $Key
    if ($null -eq $remote) {
        throw "$Label is missing in R2: $Key"
    }
    if ($remote.Size -ne $Size) {
        throw "$Label has the wrong size in R2: $Key is $($remote.Size) bytes, expected $Size"
    }
    if ([string]::IsNullOrEmpty($Md5)) {
        return
    }
    if ([string]::IsNullOrEmpty($remote.Md5)) {
        Write-Host "::warning::R2 did not report an MD5 for $Key; only its size could be verified."
        return
    }
    if ($remote.Md5 -ne $Md5) {
        throw "$Label has the wrong MD5 in R2: $Key is $($remote.Md5), expected $Md5"
    }
}

function Copy-RemoteObject {
    param(
        [Parameter(Mandatory)][string] $SourceKey,
        [Parameter(Mandatory)][string] $DestinationKey
    )

    # Server-side copy inside the bucket; --ignore-times forces the copy even when
    # rclone would consider both objects identical.
    $exitCode = Invoke-RcloneTransfer -Arguments @(
        'copyto', '--ignore-times', (Get-RemoteSpec $SourceKey), (Get-RemoteSpec $DestinationKey)
    )
    if ($exitCode -ne 0) {
        throw "Server-side copy $SourceKey -> $DestinationKey failed (rclone exit code $exitCode)"
    }
}

function Remove-RemoteObject {
    # Returns $true when no object is left at the key.
    param([Parameter(Mandatory)][string] $Key)

    if ($null -eq (Get-RemoteObject -Key $Key)) {
        return $true
    }
    $exitCode = Invoke-RcloneTransfer -Arguments @('deletefile', (Get-RemoteSpec $Key))
    if ($exitCode -ne 0) {
        Write-Host "rclone deletefile $Key returned exit code $exitCode"
    }
    return ($null -eq (Get-RemoteObject -Key $Key))
}

function Restore-LocalName {
    param([Parameter(Mandatory)] $Items)

    $problems = [System.Collections.Generic.List[string]]::new()
    foreach ($item in $Items) {
        if (-not $item.LocalRenamed) {
            continue
        }
        try {
            if (Test-Path -LiteralPath $item.LocalTempPath -PathType Leaf) {
                if (Test-Path -LiteralPath $item.LocalPath) {
                    throw "both '$($item.LocalPath)' and '$($item.LocalTempPath)' exist"
                }
                Move-Item -LiteralPath $item.LocalTempPath -Destination $item.LocalPath
            }
            if (-not (Test-Path -LiteralPath $item.LocalPath -PathType Leaf)) {
                throw "'$($item.LocalPath)' is missing after restoring its name"
            }
            $item.LocalRenamed = $false
            Write-Host "Local file name restored: $($item.LocalPath)"
        } catch {
            $problems.Add("$($item.Name): $($_.Exception.Message)")
        }
    }
    return , $problems.ToArray()
}

function Undo-Publish {
    param([Parameter(Mandatory)] $Items)

    $problems = [System.Collections.Generic.List[string]]::new()
    foreach ($item in $Items) {
        $publicStateKnownGood = $true
        try {
            if ($item.FinalTouched) {
                if ($item.HadOldObject) {
                    if (-not $item.BackupReady) {
                        throw "no verified backup exists, $($item.Key) cannot be restored"
                    }
                    Write-Host "Rollback: restoring the previous $($item.Key) from $($item.BackupKey)"
                    Copy-RemoteObject -SourceKey $item.BackupKey -DestinationKey $item.Key
                    Assert-RemoteObject -Key $item.Key -Size $item.OldSize -Md5 $item.OldMd5 -Label 'Restored object'
                } else {
                    Write-Host "Rollback: removing $($item.Key), which did not exist before"
                    if (-not (Remove-RemoteObject -Key $item.Key)) {
                        throw "could not remove $($item.Key)"
                    }
                }
            }
        } catch {
            $publicStateKnownGood = $false
            $problems.Add("$($item.Key): $($_.Exception.Message)")
        }

        try {
            if ($item.TempTouched -and -not (Remove-RemoteObject -Key $item.TempKey)) {
                $problems.Add("could not delete temporary object $($item.TempKey)")
            }
        } catch {
            $problems.Add("could not delete temporary object $($item.TempKey): $($_.Exception.Message)")
        }

        if ($item.BackupTouched) {
            if ($publicStateKnownGood) {
                try {
                    if (-not (Remove-RemoteObject -Key $item.BackupKey)) {
                        $problems.Add("could not delete backup object $($item.BackupKey)")
                    }
                } catch {
                    $problems.Add("could not delete backup object $($item.BackupKey): $($_.Exception.Message)")
                }
            } else {
                $problems.Add("kept $($item.BackupKey) (the previous $($item.Name)) for manual recovery")
            }
        }
    }
    return , $problems.ToArray()
}

function Write-LeftoverWarning {
    param([Parameter(Mandatory)] $Items)

    try {
        $listing = Invoke-RcloneQuery -Arguments @(
            'lsf', '--files-only', '--max-depth', '1', (Get-RemoteSpec "$($script:Prefix)/")
        )
        if ($listing.ExitCode -ne 0) {
            return
        }
        foreach ($name in ($listing.Stdout -split "`n")) {
            $name = $name.Trim()
            if ([string]::IsNullOrEmpty($name)) {
                continue
            }
            foreach ($item in $Items) {
                $isLeftover = $name.StartsWith("$($item.Name)$UploadMarker", [StringComparison]::Ordinal) -or
                    $name.StartsWith("$($item.Name)$BackupMarker", [StringComparison]::Ordinal)
                if ($isLeftover) {
                    Write-Host "::warning::Leftover object from an interrupted run: $($script:Prefix)/$name (safe to delete)"
                }
            }
        }
    } catch {
        Write-Host "Could not check for leftovers: $($_.Exception.Message)"
    }
}

function Get-PublicUrl {
    param([Parameter(Mandatory)][string] $Key)

    $escaped = ($Key.Split('/') | ForEach-Object { [Uri]::EscapeDataString($_) }) -join '/'
    return "$($script:PublicBaseUrl)/$escaped"
}

function Get-HeaderValue {
    param(
        [Parameter(Mandatory)] $Headers,
        [Parameter(Mandatory)][string] $Name
    )

    foreach ($headerName in @($Headers.Keys)) {
        if ([string]::Equals([string]$headerName, $Name, [StringComparison]::OrdinalIgnoreCase)) {
            return [string](@($Headers[$headerName]) | Select-Object -First 1)
        }
    }
    return ''
}

function Test-PublicDownload {
    # Warning only: the R2 object itself is already verified. This catches a custom domain
    # or a CDN cache rule that still hands out the previous file.
    param([Parameter(Mandatory)] $Item)

    $url = Get-PublicUrl -Key $Item.Key
    $problem = ''
    for ($attempt = 1; $attempt -le 3; $attempt++) {
        $problem = ''
        try {
            $response = Invoke-WebRequest -Method Head -Uri $url -TimeoutSec 60 -MaximumRedirection 5 -SkipHttpErrorCheck
            if ($response.StatusCode -ne 200) {
                $problem = "HTTP $($response.StatusCode)"
            } else {
                $length = Get-HeaderValue -Headers $response.Headers -Name 'Content-Length'
                $etag = (Get-HeaderValue -Headers $response.Headers -Name 'ETag') -replace '^W/', ''
                $etag = $etag.Trim('"').ToLowerInvariant()
                if ($length -ne [string]$Item.Size) {
                    $problem = "Content-Length is '$length', expected $($Item.Size)"
                } elseif ($etag -match '^[0-9a-f]{32}$' -and $etag -ne $Item.Md5) {
                    $problem = "ETag is $etag, expected MD5 $($Item.Md5)"
                }
            }
        } catch {
            $problem = $_.Exception.Message
        }
        if ([string]::IsNullOrEmpty($problem)) {
            Write-Host "Public download serves the new file: $url"
            return
        }
        if ($attempt -lt 3) {
            Start-Sleep -Seconds 5
        }
    }
    Write-Host "::warning::$url does not serve the new $($Item.Name) yet ($problem). The R2 object is correct; check the custom domain and any Cache Rule that ignores Cache-Control."
}

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

$script:Prefix = Get-NormalizedPrefix -Value $Prefix

$endpoint = Get-R2Endpoint

$script:Bucket = Get-R2Bucket
$accessKeyId = Get-RequiredSetting -Name 'R2_ACCESS_KEY_ID'
$script:PublicBaseUrl = $PublicBaseUrl.Trim().TrimEnd('/')
if (-not [string]::IsNullOrEmpty($script:PublicBaseUrl)) {
    $publicUri = $null
    if (-not [Uri]::TryCreate($script:PublicBaseUrl, [UriKind]::Absolute, [ref]$publicUri) -or
        -not ($publicUri.Scheme -eq 'https' -or ($publicUri.Scheme -eq 'http' -and $publicUri.IsLoopback)) -or
        -not [string]::IsNullOrEmpty($publicUri.Query)) {
        throw "-PublicBaseUrl must be an https URL such as https://r2.cloud-pe.cn"
    }
}
$secretAccessKey = Get-RequiredSetting -Name 'R2_SECRET_ACCESS_KEY'

$runToken = if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_RUN_ID)) {
    $attempt = if ([string]::IsNullOrWhiteSpace($env:GITHUB_RUN_ATTEMPT)) { '1' } else { $env:GITHUB_RUN_ATTEMPT }
    "$($env:GITHUB_RUN_ID)-$attempt"
} else {
    'local-{0}-{1}' -f [DateTimeOffset]::UtcNow.ToUnixTimeSeconds(), [guid]::NewGuid().ToString('N').Substring(0, 8)
}

$items = [System.Collections.Generic.List[object]]::new()
foreach ($entry in $Path) {
    if ([string]::IsNullOrWhiteSpace($entry)) {
        throw 'Empty file path passed to -Path.'
    }
    if (-not (Test-Path -LiteralPath $entry -PathType Leaf)) {
        throw "File to upload does not exist: $entry"
    }
    $file = Get-Item -LiteralPath $entry -Force
    $localTempPath = Join-Path $file.DirectoryName "$($file.Name)$UploadMarker$runToken"
    if (Test-Path -LiteralPath $localTempPath) {
        throw "Temporary local path already exists: $localTempPath"
    }
    $items.Add([pscustomobject]@{
        Name          = $file.Name
        LocalPath     = $file.FullName
        LocalTempPath = $localTempPath
        Size          = [long]$file.Length
        Md5           = (Get-FileHash -LiteralPath $file.FullName -Algorithm MD5).Hash.ToLowerInvariant()
        ContentType   = Get-ContentType -FileName $file.Name
        Key           = "$($script:Prefix)/$($file.Name)"
        TempKey       = "$($script:Prefix)/$($file.Name)$UploadMarker$runToken"
        BackupKey     = "$($script:Prefix)/$($file.Name)$BackupMarker$runToken"
        OldSize       = [long]0
        OldMd5        = ''
        LocalRenamed  = $false
        TempTouched   = $false
        HadOldObject  = $false
        BackupTouched = $false
        BackupReady   = $false
        FinalTouched  = $false
    })
}
$duplicates = @($items | Group-Object -Property Name | Where-Object { $_.Count -gt 1 } | ForEach-Object { $_.Name })
if ($duplicates.Count -gt 0) {
    throw "Several files would publish to the same object name: $($duplicates -join ', ')"
}

$workDirectory = Get-WorkDirectory
$script:Rclone = Get-Rclone -WorkDirectory $workDirectory
& $script:Rclone version | Select-Object -First 1 | Out-Host

Set-R2RemoteEnvironment -WorkDirectory $workDirectory -Endpoint $endpoint -AccessKeyId $accessKeyId -SecretAccessKey $secretAccessKey

Write-Host "Publishing $($items.Count) file(s) to R2 folder $($script:Prefix)/ (run $runToken)"

# ---------------------------------------------------------------------------
# Publish transaction
# ---------------------------------------------------------------------------

$failure = $null
try {
    # 1. Rename the local files.
    foreach ($item in $items) {
        Move-Item -LiteralPath $item.LocalPath -Destination $item.LocalTempPath
        $item.LocalRenamed = $true
        Write-Host "Local file renamed: $($item.Name) -> $([System.IO.Path]::GetFileName($item.LocalTempPath))"
    }

    # 2. Upload under the temporary names and verify.
    foreach ($item in $items) {
        Write-Host "Uploading $($item.Name) ($($item.Size) bytes, MD5 $($item.Md5)) to $($item.TempKey)"
        $item.TempTouched = $true
        $exitCode = Invoke-RcloneTransfer -Arguments @(
            'copyto',
            '--header-upload', "Content-Type: $($item.ContentType)",
            '--header-upload', 'Cache-Control: no-cache',
            $item.LocalTempPath,
            (Get-RemoteSpec $item.TempKey)
        )
        if ($exitCode -ne 0) {
            throw "Upload of $($item.Name) failed (rclone exit code $exitCode)"
        }
        Assert-RemoteObject -Key $item.TempKey -Size $item.Size -Md5 $item.Md5 -Label 'Uploaded temporary object'
    }

    # 3. Keep a verified backup of every object that will be replaced.
    foreach ($item in $items) {
        $old = Get-RemoteObject -Key $item.Key
        if ($null -eq $old) {
            Write-Host "$($item.Key) does not exist yet; it will be created."
            continue
        }
        $item.HadOldObject = $true
        $item.OldSize = $old.Size
        $item.OldMd5 = $old.Md5
        Write-Host "Backing up the existing $($item.Key) to $($item.BackupKey)"
        $item.BackupTouched = $true
        Copy-RemoteObject -SourceKey $item.Key -DestinationKey $item.BackupKey
        Assert-RemoteObject -Key $item.BackupKey -Size $item.OldSize -Md5 $item.OldMd5 -Label 'Backup object'
        $item.BackupReady = $true
    }

    # 4. Rename the uploaded objects back to their real names (replaces old objects).
    foreach ($item in $items) {
        Write-Host "Renaming $($item.TempKey) -> $($item.Key)"
        $item.FinalTouched = $true
        Copy-RemoteObject -SourceKey $item.TempKey -DestinationKey $item.Key
        Assert-RemoteObject -Key $item.Key -Size $item.Size -Md5 $item.Md5 -Label 'Published object'
    }
} catch {
    $failure = $_
}

if ($null -ne $failure) {
    $reason = $failure.Exception.Message
    Write-Host "::error::R2 publish to $($script:Prefix)/ failed: $reason"
    Write-Host 'Rolling back...'
    # Both functions return a (possibly empty) string array; do not wrap them in @().
    $rollbackProblems = Undo-Publish -Items $items
    $localProblems = Restore-LocalName -Items $items
    foreach ($problem in @($rollbackProblems + $localProblems)) {
        Write-Host "::error::Rollback problem: $problem"
    }
    if ($rollbackProblems.Count -gt 0 -or $localProblems.Count -gt 0) {
        throw "R2 publish failed ($reason) and the rollback is incomplete; see the errors above."
    }
    throw "R2 publish failed and was rolled back; R2 is unchanged. Cause: $reason"
}

# ---------------------------------------------------------------------------
# 5. Delete the temporary objects and the old files, restore local names.
# ---------------------------------------------------------------------------

foreach ($item in $items) {
    foreach ($key in @($item.TempKey, $item.BackupKey)) {
        if ($key -eq $item.BackupKey -and -not $item.BackupTouched) {
            continue
        }
        try {
            if (-not (Remove-RemoteObject -Key $key)) {
                Write-Host "::warning::Could not delete $key; the new file is published, delete it manually."
            }
        } catch {
            Write-Host "::warning::Could not delete $key ($($_.Exception.Message)); the new file is published, delete it manually."
        }
    }
}

$localProblems = Restore-LocalName -Items $items
if ($localProblems.Count -gt 0) {
    throw "The files were published to R2, but local file names could not be restored: $($localProblems -join '; ')"
}

Write-LeftoverWarning -Items $items

foreach ($item in $items) {
    $action = if ($item.HadOldObject) { 'replaced' } else { 'created' }
    Write-Host "Published $($item.Key): $($item.Size) bytes, MD5 $($item.Md5) ($action)"
}
if (-not [string]::IsNullOrEmpty($script:PublicBaseUrl)) {
    foreach ($item in $items) {
        Test-PublicDownload -Item $item
    }
}
if (-not [string]::IsNullOrWhiteSpace($env:GITHUB_STEP_SUMMARY)) {
    $summary = [System.Collections.Generic.List[string]]::new()
    $summary.Add("### Cloudflare R2: $($script:Prefix)/")
    $summary.Add('')
    $summary.Add('| Object | Size (bytes) | MD5 | Result | Download |')
    $summary.Add('| --- | ---: | --- | --- | --- |')
    foreach ($item in $items) {
        $action = if ($item.HadOldObject) { 'replaced' } else { 'created' }
        $download = if ([string]::IsNullOrEmpty($script:PublicBaseUrl)) { '-' } else { Get-PublicUrl -Key $item.Key }
        $summary.Add("| ``$($item.Key)`` | $($item.Size) | ``$($item.Md5)`` | $action | $download |")
    }
    Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY -Value $summary -Encoding utf8
}

# GitHub's pwsh wrapper exits with $LASTEXITCODE; do not leak a probe's exit code.
$global:LASTEXITCODE = 0
