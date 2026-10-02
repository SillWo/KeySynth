[CmdletBinding()]
param(
    [string]$ExecutablePath = (Join-Path $PSScriptRoot '..\target\x86_64-pc-windows-gnullvm\release\keysynth.exe'),
    [string]$CompilerPath,
    [string]$OutputDirectory = (Join-Path $env:TEMP 'KeySynth-installer')
)

$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$issPath = Join-Path $PSScriptRoot 'KeySynth.iss'
$exePath = [System.IO.Path]::GetFullPath($ExecutablePath)
$outputPath = [System.IO.Path]::GetFullPath($OutputDirectory)

if (-not (Test-Path -LiteralPath $exePath -PathType Leaf)) {
    throw "Release executable not found: $exePath. Build the Windows release executable first or pass -ExecutablePath."
}

if (-not $CompilerPath) {
    $fromPath = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($fromPath) {
        $CompilerPath = $fromPath.Source
    } else {
        $candidates = @(
            (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 7\ISCC.exe'),
            (Join-Path $env:ProgramFiles 'Inno Setup 7\ISCC.exe')
        ) | Where-Object { $_ -and (Test-Path -LiteralPath $_ -PathType Leaf) }
        if ($candidates.Count -gt 0) { $CompilerPath = $candidates[0] }
    }
}
if (-not $CompilerPath -or -not (Test-Path -LiteralPath $CompilerPath -PathType Leaf)) {
    throw 'Inno Setup 7 ISCC.exe was not found. Pass -CompilerPath or add ISCC.exe to PATH.'
}
if (-not (Test-Path -LiteralPath $issPath -PathType Leaf)) {
    throw "Installer definition not found: $issPath"
}

$repoPrefix = $repoRoot.TrimEnd('\') + '\'
if ($outputPath.Equals($repoRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
    $outputPath.StartsWith($repoPrefix, [System.StringComparison]::OrdinalIgnoreCase)) {
    throw "Installer output must be outside the repository: $outputPath"
}
New-Item -ItemType Directory -Path $outputPath -Force | Out-Null

& $CompilerPath "/DSourceExe=$exePath" "/DOutputDirectory=$outputPath" $issPath
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE"
}

$setup = Join-Path $outputPath 'KeySynth-0.2.0-Setup.exe'
if (-not (Test-Path -LiteralPath $setup -PathType Leaf)) {
    throw "Installer compiler succeeded but expected output is missing: $setup"
}
Write-Output $setup
