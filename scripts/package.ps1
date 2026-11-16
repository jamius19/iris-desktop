# Author: Jamius Siam
# Since: 16/11/2026
#
# Builds the Windows installer (NSIS, cargo-packager) with the VC++ runtime DLLs next to iris.exe.
# Config: crates/iris/Cargo.toml. Runs in Windows PowerShell 5.1 and PowerShell 7.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

if (-not (cargo --list | Select-String -Quiet '^\s+packager\b')) {
    throw 'cargo packager is missing. Install it with: cargo install cargo-packager --locked'
}

$metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
$version = ($metadata.packages | Where-Object { $_.name -eq 'iris' }).version

# build.rs copies msvcp140*/vcruntime140* from IRIS_VC_REDIST_DIRS into transcribe-libs/ (Handy's CI lookup:
# the redist of the Visual Studio that compiles the code).
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vsRoot = & $vswhere -latest -products * -property installationPath
if (-not $vsRoot) { throw 'vswhere found no Visual Studio installation' }
$versionFile = Join-Path $vsRoot 'VC\Auxiliary\Build\Microsoft.VCRedistVersion.default.txt'
$redistVersion = (Get-Content $versionFile -TotalCount 1).Trim()
$crt = Get-ChildItem (Join-Path $vsRoot "VC\Redist\MSVC\$redistVersion\x64") -Directory -Filter 'Microsoft.VC*.CRT' |
    Sort-Object Name | Select-Object -Last 1
if (-not $crt) { throw "No VC++ CRT redist dir under $vsRoot\VC\Redist\MSVC\$redistVersion\x64" }
$env:IRIS_VC_REDIST_DIRS = $crt.FullName

cargo build --release -p iris
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

# build.rs refills transcribe-libs/ only when it reruns, and WSL builds share the directory.
foreach ($dll in 'transcribe.dll', 'msvcp140.dll', 'vcruntime140.dll') {
    if (-not (Test-Path "crates\iris\transcribe-libs\$dll")) {
        throw "crates\iris\transcribe-libs\ has no $dll. Touch crates\iris\build.rs and retry."
    }
}

cargo packager --release -p iris --formats nsis
if ($LASTEXITCODE -ne 0) { throw 'cargo packager failed' }

$installer = Join-Path $metadata.target_directory "release\iris_${version}_x64-setup.exe"
if (-not (Test-Path $installer)) { throw "Expected $installer, but it wasn't produced" }
Write-Output "Installer for Iris ${version}:"
Write-Output "  $installer"
