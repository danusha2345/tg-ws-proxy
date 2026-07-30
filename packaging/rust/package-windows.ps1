param(
    [Parameter(Mandatory = $true)]
    [string]$Version,

    [Parameter(Mandatory = $true)]
    [ValidateSet("x64", "arm64")]
    [string]$ArchSuffix
)

$ErrorActionPreference = "Stop"
$ProjectDir = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$DistDir = Join-Path $ProjectDir "dist"
$ReleaseDir = Join-Path $ProjectDir "target\release"
$CliSource = Join-Path $ReleaseDir "tg-ws-proxy.exe"
$DesktopSource = Join-Path $ReleaseDir "tg-ws-proxy-desktop.exe"

foreach ($Binary in @($CliSource, $DesktopSource)) {
    if (-not (Test-Path -PathType Leaf $Binary)) {
        throw "Missing release binary: $Binary"
    }
}

New-Item -ItemType Directory -Force $DistDir | Out-Null

$DesktopAsset = Join-Path $DistDir "TgWsProxy_windows_$ArchSuffix.exe"
$CliAsset = Join-Path $DistDir "tg-ws-proxy_cli_windows_$ArchSuffix.exe"
Copy-Item $DesktopSource $DesktopAsset -Force
Copy-Item $CliSource $CliAsset -Force

$ScratchBase = if ($env:RUNNER_TEMP) {
    $env:RUNNER_TEMP
} else {
    Join-Path $ProjectDir ".scratch"
}
New-Item -ItemType Directory -Force $ScratchBase | Out-Null
$StageDir = Join-Path $ScratchBase ("tg-ws-proxy-windows-" + [guid]::NewGuid())

try {
    New-Item -ItemType Directory -Force $StageDir | Out-Null
    Copy-Item $CliSource (Join-Path $StageDir "tg-ws-proxy.exe")
    Copy-Item $DesktopSource (Join-Path $StageDir "tg-ws-proxy-desktop.exe")
    Copy-Item (Join-Path $ProjectDir "LICENSE") (Join-Path $StageDir "LICENSE")
    Copy-Item (Join-Path $ProjectDir "packaging\rust\README.md") `
        (Join-Path $StageDir "README.md")
    Copy-Item (Join-Path $ProjectDir "docs\RUST_PORT.md") `
        (Join-Path $StageDir "RUST_PORT.md")

    $Archive = Join-Path $DistDir "TgWsProxy_windows_$ArchSuffix.zip"
    Compress-Archive -Path (Join-Path $StageDir "*") -DestinationPath $Archive -Force
} finally {
    if (Test-Path $StageDir) {
        Remove-Item -Recurse -Force $StageDir
    }
}

Write-Host "Packaged TG WS Proxy Rust $Version for Windows $ArchSuffix"
