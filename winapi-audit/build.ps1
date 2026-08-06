param(
    [string]$OutputDirectory = (Join-Path $PSScriptRoot '..\target\release')
)

$ErrorActionPreference = 'Stop'
$source = Join-Path $PSScriptRoot 'LetRecoveryWinApiAudit.c'
$inventory = Join-Path $PSScriptRoot 'LetRecoveryWinApiDynamicApis.inc'
$repository = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$output = Join-Path ([System.IO.Path]::GetFullPath($OutputDirectory)) 'LetRecoveryWinApiAudit.exe'
if (-not (Test-Path -LiteralPath $inventory -PathType Leaf)) {
    throw 'The runtime-resolved API inventory is missing.'
}

# Prevent new literal libloading/GetProcAddress bindings from silently escaping the auditor.
$sourceRoots = Get-ChildItem -LiteralPath $repository -Directory | ForEach-Object {
    $candidate = Join-Path $_.FullName 'src'
    if (Test-Path -LiteralPath $candidate -PathType Container) { $candidate }
}
$runtimeSources = Get-ChildItem -LiteralPath $sourceRoots -Recurse -Filter *.rs -File
$patterns = @(
    '\.get(?:\s*::<[^>]+>)?\s*\(\s*b"([^"\\]+)(?:\\0)?"',
    '(?:req!|opt!|load_sym!|load_optional_sym(?:\s*::<[^>]+>)?)\s*\([\s\S]{0,300}?b"([^"\\]+)(?:\\0)?"',
    'procedure\s*\(\s*b"([^"\\]+)(?:\\0)?"',
    'load_catalog_proc\s*\([^,]+,\s*b"([^"\\]+)(?:\\0)?"',
    'GetProcAddress\s*\([^,]+,\s*PCSTR\s*\(\s*c"([^"]+)"'
)
$declared = [System.IO.File]::ReadAllText($inventory)
$unregistered = [System.Collections.Generic.SortedSet[string]]::new([System.StringComparer]::Ordinal)
foreach ($runtimeSource in $runtimeSources) {
    $text = [System.IO.File]::ReadAllText($runtimeSource.FullName)
    foreach ($pattern in $patterns) {
        foreach ($match in [regex]::Matches($text, $pattern)) {
            $symbol = $match.Groups[1].Value
            if ($declared.IndexOf(('"{0}"' -f $symbol), [System.StringComparison]::Ordinal) -lt 0) {
                [void]$unregistered.Add($symbol)
            }
        }
    }
}
if ($unregistered.Count -ne 0) {
    throw "Runtime-resolved APIs are missing from LetRecoveryWinApiDynamicApis.inc: $($unregistered -join ', ')"
}
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
    throw 'vswhere.exe was not found; Visual Studio Build Tools with C++ are required.'
}
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $installation) {
    throw 'Visual Studio C++ x64 tools were not found.'
}
$vsDevCmd = Join-Path $installation 'Common7\Tools\VsDevCmd.bat'
if (-not (Test-Path -LiteralPath $vsDevCmd -PathType Leaf)) {
    throw 'VsDevCmd.bat was not found in the selected Visual Studio installation.'
}
New-Item -ItemType Directory -Path (Split-Path -Parent $output) -Force | Out-Null
$object = Join-Path (Split-Path -Parent $output) 'LetRecoveryWinApiAudit.obj'
$command = 'call "{0}" -arch=x64 -host_arch=x64 >nul && cl.exe /nologo /O2 /W4 /WX /DUNICODE /D_UNICODE /D_WIN32_WINNT=0x0601 /MT /guard:cf- /Fo:"{3}" "{1}" /link /SUBSYSTEM:CONSOLE,6.01 /OUT:"{2}"' -f $vsDevCmd, $source, $output, $object
& $env:ComSpec /d /s /c $command
if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $output -PathType Leaf)) {
    throw "Failed to build $output"
}
$dumpbin = Get-ChildItem -LiteralPath (Join-Path $installation 'VC\Tools\MSVC') -Filter dumpbin.exe -File -Recurse |
    Where-Object { $_.FullName -match '\\bin\\Hostx64\\x64\\dumpbin\.exe$' } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if (-not $dumpbin) {
    throw 'dumpbin.exe was not found for the Win7 import-boundary check.'
}
$imports = (& $dumpbin.FullName /imports $output | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw 'dumpbin.exe failed while checking the compatibility auditor.'
}
$forbidden = @(
    'api-ms-win-',
    'ext-ms-win-',
    'ucrtbase.dll',
    'VCRUNTIME140.dll',
    'GetSystemTimePreciseAsFileTime',
    'WaitOnAddress',
    'WakeByAddress',
    'SetThreadDescription',
    'GetDpiForWindow',
    'SetProcessDpiAwarenessContext'
)
foreach ($name in $forbidden) {
    if ($imports.IndexOf($name, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw "The compatibility auditor imports a Windows 8+ or external runtime dependency: $name"
    }
}
Write-Host "Built $output"
