[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$Root
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$resolvedRoot = (Resolve-Path -LiteralPath $Root).Path
if (-not (Test-Path -LiteralPath $resolvedRoot -PathType Container)) {
    throw "Release tree does not exist: $Root"
}

$forbiddenExtensions = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase
)
foreach ($extension in @('.log', '.pdb', '.ilk', '.exp', '.lib', '.obj', '.tmp', '.dmp', '.swp')) {
    [void]$forbiddenExtensions.Add($extension)
}

$forbiddenFiles = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase
)
foreach ($name in @('GHOSTERR.TXT', 'ghstwarn.txt', 'Thumbs.db', '.DS_Store')) {
    [void]$forbiddenFiles.Add($name)
}

$forbiddenDirectories = [System.Collections.Generic.HashSet[string]]::new(
    [System.StringComparer]::OrdinalIgnoreCase
)
foreach ($name in @('.git', 'target', '.vs', '.idea', '__pycache__')) {
    [void]$forbiddenDirectories.Add($name)
}

$violations = [System.Collections.Generic.List[string]]::new()
$rootItem = Get-Item -LiteralPath $resolvedRoot -Force
if (($rootItem.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
    $violations.Add('reparse:<release-root>')
}
$items = @(Get-ChildItem -LiteralPath $resolvedRoot -Recurse -Force)
foreach ($item in $items) {
    $relative = $item.FullName.Substring($resolvedRoot.Length).TrimStart([char[]]@('\', '/'))
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        $violations.Add("reparse:$relative")
        continue
    }
    if ($item.PSIsContainer) {
        if ($forbiddenDirectories.Contains($item.Name)) {
            $violations.Add("directory:$relative")
        }
        continue
    }
    if ($forbiddenFiles.Contains($item.Name) -or $forbiddenExtensions.Contains($item.Extension)) {
        $violations.Add("file:$relative")
    }
}

if ($violations.Count -ne 0) {
    $sample = @($violations | Select-Object -First 20)
    $remaining = $violations.Count - $sample.Count
    $detail = $sample -join ', '
    if ($remaining -gt 0) {
        $detail += ", ... and $remaining more"
    }
    throw "Release tree contains forbidden runtime/development artifacts: $detail"
}

Write-Host "Release tree hygiene verified: $resolvedRoot ($($items.Count) entries)"
