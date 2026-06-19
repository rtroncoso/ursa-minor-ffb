# Bump app version across Cargo.toml, Windows VERSIONINFO, and the MSI project.
#
# Usage:
#   .\scripts\bump-version.ps1 patch
#   .\scripts\bump-version.ps1 minor
#   .\scripts\bump-version.ps1 major
#   .\scripts\bump-version.ps1 2.1.0
#   .\scripts\bump-version.ps1 patch -DryRun

param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Bump,

    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

function Read-CurrentVersion {
    param([string]$CargoTomlPath)
    $content = Get-Content -Raw -Path $CargoTomlPath
    if ($content -match '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"') {
        return [version]$Matches[1]
    }
    throw "Could not read semver from $CargoTomlPath"
}

function Resolve-TargetVersion {
    param(
        [version]$Current,
        [string]$BumpArg
    )

    if ($BumpArg -match '^\d+\.\d+\.\d+$') {
        return [version]$BumpArg
    }

    switch ($BumpArg.ToLowerInvariant()) {
        "major" { return [version]"$($Current.Major + 1).0.0" }
        "minor" { return [version]"$($Current.Major).$($Current.Minor + 1).0" }
        "patch" { return [version]"$($Current.Major).$($Current.Minor).$($Current.Build + 1)" }
        default {
            throw "Unknown bump '$BumpArg'. Use major, minor, patch, or an explicit X.Y.Z version."
        }
    }
}

function Format-Win32VersionTuple {
    param([version]$Version)
    return "$($Version.Major),$($Version.Minor),$($Version.Build),0"
}

function Update-FileContent {
    param(
        [string]$Path,
        [scriptblock]$Transform
    )

    if (-not (Test-Path $Path)) {
        throw "Missing file: $Path"
    }

    $original = Get-Content -Raw -Path $Path
    $updated = & $Transform $original
    if ($updated -eq $original) {
        throw "No changes applied to $Path"
    }

    if ($DryRun) {
        Write-Host "[dry-run] would update $Path"
        return
    }

    Set-Content -Path $Path -Value $updated -NoNewline
    Write-Host "Updated $Path"
}

$current = Read-CurrentVersion (Join-Path $Root "Cargo.toml")
$target = Resolve-TargetVersion -Current $current -BumpArg $Bump
$semver = $target.ToString()
$winTuple = Format-Win32VersionTuple -Version $target

Write-Host "Version: $current -> $semver"
if ($DryRun) {
    Write-Host "(dry run - no files will be written)"
}

Update-FileContent (Join-Path $Root "Cargo.toml") {
    param($text)
    $text -replace '(?m)^(version\s*=\s*")(\d+\.\d+\.\d+)(")', "`${1}$semver`${3}"
}

Update-FileContent (Join-Path $Root "windows\resource.rc") {
    param($text)
    $text = $text -replace '(?m)^FILEVERSION\s+\d+,\d+,\d+,\d+', "FILEVERSION     $winTuple"
    $text = $text -replace '(?m)^PRODUCTVERSION\s+\d+,\d+,\d+,\d+', "PRODUCTVERSION  $winTuple"
    $text = $text -replace '(?m)(VALUE "FileVersion",\s+)"\d+\.\d+\.\d+\\0"', "`${1}`"$semver\0`""
    $text = $text -replace '(?m)(VALUE "ProductVersion",\s+)"\d+\.\d+\.\d+\\0"', "`${1}`"$semver\0`""
    $text
}

$aipPath = Join-Path $Root "installer\Ursa Minor FFB.aip"
Update-FileContent $aipPath {
    param($text)
    $text -replace '(<ROW Property="ProductVersion" Value=")(\d+\.\d+\.\d+)(" Options="32"/>)', "`${1}$semver`${3}"
}

if (-not $DryRun) {
    Push-Location $Root
    try {
        Write-Host "Refreshing Cargo.lock..."
        cargo generate-lockfile | Out-Null
    }
    finally {
        Pop-Location
    }
}

Write-Host ""
Write-Host "Done. Release checklist:"
Write-Host "  1. Update CHANGELOG.md for v$semver"
Write-Host "  2. .\installer\build-staging.ps1   # refresh MSI inputs"
Write-Host "  3. Build MSI in Advanced Installer (optional local test)"
Write-Host "  4. git add -A"
Write-Host "  5. git commit -m `"chore: release v$semver`""
Write-Host "  6. git tag v$semver"
Write-Host "  7. git push && git push origin v$semver"
