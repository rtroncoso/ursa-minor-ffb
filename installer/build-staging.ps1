# Local MSI build helper — run from repo root after building release binaries.
# Requires Advanced Installer (GUI) to open/build installer\ursa-minor-ffb.aip.

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

Push-Location $Root
try {
    Write-Host "Building release binaries..."
    cargo build --release --features app --locked
    cargo build --release --bin ursa-minor-updater --features updater --locked

    $stage = Join-Path $Root "dist\msi-staging"
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item "target\release\ursa-minor-ffb.exe" "$stage\Ursa Minor FFB.exe" -Force
    Copy-Item "lib\SimConnect.dll" "$stage\SimConnect.dll" -Force
    Copy-Item "target\release\ursa-minor-updater.exe" "$stage\ursa-minor-updater.exe" -Force

    Write-Host "Staged files in $stage"
    Get-ChildItem $stage | Format-Table Name, Length

    Write-Host ""
    Write-Host "Next steps:"
    Write-Host "  1. Open installer\ursa-minor-ffb.aip in Advanced Installer"
    Write-Host "  2. Build -> Build (output: dist\UrsaMinorFFB-<version>.msi)"
    Write-Host "  3. Test: msiexec /i `"dist\UrsaMinorFFB-2.0.1-x64.msi`" /passive"
    Write-Host "  4. Verify: dir `"C:\Program Files\Ursa Minor FFB`""
}
finally {
    Pop-Location
}
