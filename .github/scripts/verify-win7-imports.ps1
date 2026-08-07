param(
    [string]$Executable = (Join-Path $PSScriptRoot '..\..\target\x86_64-win7-windows-msvc\release\LetRecovery.exe')
)

$ErrorActionPreference = 'Stop'
$executablePath = [System.IO.Path]::GetFullPath($Executable)
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    throw "The normal endpoint executable was not found: $executablePath"
}

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
if (-not (Test-Path -LiteralPath $vswhere -PathType Leaf)) {
    throw 'vswhere.exe was not found; Visual Studio Build Tools with C++ are required.'
}
$installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $installation) {
    throw 'Visual Studio C++ x64 tools were not found.'
}
$dumpbin = Get-ChildItem -LiteralPath (Join-Path $installation 'VC\Tools\MSVC') -Filter dumpbin.exe -File -Recurse |
    Where-Object { $_.FullName -match '\\bin\\Hostx64\\x64\\dumpbin\.exe$' } |
    Sort-Object FullName -Descending |
    Select-Object -First 1
if (-not $dumpbin) {
    throw 'dumpbin.exe was not found for the Win7 import-boundary check.'
}

$imports = (& $dumpbin.FullName /imports $executablePath | Out-String)
if ($LASTEXITCODE -ne 0) {
    throw 'dumpbin.exe failed while checking the normal endpoint compatibility boundary.'
}
$forbidden = @(
    'api-ms-win-',
    'ext-ms-win-',
    'ucrtbase.dll',
    'combase.dll',
    'GetSystemTimePreciseAsFileTime',
    'WaitOnAddress',
    'WakeByAddress',
    'SetThreadDescription',
    'GetDpiForWindow',
    'SetProcessDpiAwarenessContext'
)
foreach ($name in $forbidden) {
    if ($imports.IndexOf($name, [System.StringComparison]::OrdinalIgnoreCase) -ge 0) {
        throw "The normal endpoint imports a Windows 8+ or UCRT loader dependency: $name"
    }
}
Write-Host "Verified Win7 import boundary: $executablePath"
