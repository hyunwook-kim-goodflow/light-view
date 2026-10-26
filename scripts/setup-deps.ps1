<#
.SYNOPSIS
    Fetches and builds the C dependencies of light-view (libheif + libde265) with vcpkg
    and copies their DLLs next to the cargo build outputs.

.DESCRIPTION
    Runs under Windows PowerShell 5.1:
        powershell -ExecutionPolicy Bypass -File scripts/setup-deps.ps1

    1. Clones vcpkg at a pinned commit into .vcpkg\ and bootstraps it.
    2. Runs `vcpkg install --triplet x64-windows` in manifest mode (vcpkg.json).
    3. Fails if any forbidden codec (x265, aom, dav1d, x264, kvazaar, openjpeg, libjpeg-turbo)
       was installed.
    4. Copies the runtime DLLs into target\debug and target\release, next to the executables.
       (`cargo test`/`cargo run` also find them through libheif-sys's OUT_DIR, which cargo
       puts on PATH.)
    5. Prints the environment the build uses (pinned in .cargo\config.toml [env]).
#>
[CmdletBinding()]
param()

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

# vcpkg release tag 2026.10.16. Keep in sync with "baseline" in vcpkg-configuration.json.
$VcpkgCommit = '9eec002df1cd109f8f104cd966a763daf9751489'
$VcpkgRepo = 'https://github.com/microsoft/vcpkg'
$Triplet = 'x64-windows'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$VcpkgDir = Join-Path $RepoRoot '.vcpkg'
$VcpkgExe = Join-Path $VcpkgDir 'vcpkg.exe'
$InstallRoot = Join-Path $RepoRoot 'vcpkg_installed'
$TripletDir = Join-Path $InstallRoot $Triplet

function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "'$Exe $($Arguments -join ' ')' failed with exit code $LASTEXITCODE"
    }
}

# --- 1. vcpkg at the pinned commit -------------------------------------------------------
if (-not (Test-Path (Join-Path $VcpkgDir '.git'))) {
    Write-Host "Cloning vcpkg $VcpkgCommit into $VcpkgDir"
    New-Item -ItemType Directory -Force -Path $VcpkgDir | Out-Null
    Invoke-Native git @('-C', $VcpkgDir, 'init', '--quiet')
}
# Idempotent even if an earlier run stopped between `init` and `remote add`.
$origin = & git -C $VcpkgDir config --get remote.origin.url
if (-not $origin) {
    Invoke-Native git @('-C', $VcpkgDir, 'remote', 'add', 'origin', $VcpkgRepo)
} elseif ($origin -ne $VcpkgRepo) {
    Invoke-Native git @('-C', $VcpkgDir, 'remote', 'set-url', 'origin', $VcpkgRepo)
}
$head = & git -C $VcpkgDir rev-parse --verify --quiet HEAD
if ($head -ne $VcpkgCommit) {
    Write-Host "Checking out vcpkg $VcpkgCommit"
    Invoke-Native git @('-C', $VcpkgDir, 'fetch', '--quiet', '--depth', '1', 'origin', $VcpkgCommit)
    Invoke-Native git @('-C', $VcpkgDir, 'checkout', '--quiet', '--force', $VcpkgCommit)
    if (Test-Path $VcpkgExe) { Remove-Item -Force $VcpkgExe }
}
if (-not (Test-Path $VcpkgExe)) {
    Write-Host 'Bootstrapping vcpkg'
    Invoke-Native (Join-Path $VcpkgDir 'bootstrap-vcpkg.bat') @('-disableMetrics')
}

# --- 2. Manifest-mode install -------------------------------------------------------------
# Keep every vcpkg cache inside the repo: by default the binary cache lives in
# %LOCALAPPDATA%\vcpkg\archives. Root, downloads and buildtrees are passed explicitly so a
# global VCPKG_ROOT / VCPKG_DOWNLOADS cannot redirect them.
$ArchivesDir = Join-Path $VcpkgDir 'archives'
$DownloadsDir = Join-Path $VcpkgDir 'downloads'
New-Item -ItemType Directory -Force -Path $ArchivesDir, $DownloadsDir | Out-Null
$env:VCPKG_DISABLE_METRICS = '1'
Write-Host "Installing vcpkg.json dependencies for $Triplet"
Invoke-Native $VcpkgExe @(
    'install',
    '--triplet', $Triplet,
    "--vcpkg-root=$VcpkgDir",
    "--downloads-root=$DownloadsDir",
    "--x-buildtrees-root=$(Join-Path $VcpkgDir 'buildtrees')",
    "--x-packages-root=$(Join-Path $VcpkgDir 'packages')",
    "--x-manifest-root=$RepoRoot",
    "--x-install-root=$InstallRoot",
    "--binarysource=clear;files,$ArchivesDir,readwrite",
    '--disable-metrics'
)

# The `vcpkg` crate (used by libheif-sys's build script) only looks for packages under
# $VCPKG_ROOT\installed, so expose the manifest-mode install tree there.
$classicInstalled = Join-Path $VcpkgDir 'installed'
if (-not (Test-Path $classicInstalled)) {
    New-Item -ItemType Junction -Path $classicInstalled -Target $InstallRoot | Out-Null
}
$linkTarget = (Get-Item $classicInstalled).Target
if (-not $linkTarget -or ((@($linkTarget)[0]).TrimEnd('\') -ne $InstallRoot.TrimEnd('\'))) {
    throw "$classicInstalled exists but is not a junction to $InstallRoot. Delete it and rerun."
}

# --- 3. Forbidden codecs ------------------------------------------------------------------
# GPL encoders (x265, x264) and codecs light-view does not ship (aom, dav1d, kvazaar,
# openjpeg, libjpeg-turbo) must never be part of the installed tree.
$forbidden = @('x265', 'aom', 'dav1d', 'x264', 'kvazaar', 'openjpeg', 'libjpeg-turbo')
$violations = @()
foreach ($name in $forbidden) {
    $hits = @()
    $hits += @(Get-ChildItem -Path (Join-Path $InstallRoot 'vcpkg\info') -Filter "${name}_*.list" -ErrorAction SilentlyContinue)
    $hits += @(Get-ChildItem -Path (Join-Path $TripletDir 'share') -Filter $name -Directory -ErrorAction SilentlyContinue)
    $hits += @(Get-ChildItem -Path (Join-Path $TripletDir 'include') -Filter "$name*" -ErrorAction SilentlyContinue)
    $hits += @(Get-ChildItem -Path (Join-Path $TripletDir 'bin') -Filter "*$name*.dll" -ErrorAction SilentlyContinue)
    foreach ($hit in $hits) { $violations += $hit.FullName }
}
if ($violations.Count -gt 0) {
    $violations | ForEach-Object { Write-Host "forbidden: $_" }
    throw 'Forbidden codec artifacts found in vcpkg_installed (see above). Check vcpkg.json features.'
}
Write-Host "Forbidden-codec check passed ($($forbidden -join ', ') absent)."

# --- 4. Runtime DLLs ----------------------------------------------------------------------
$binDir = Join-Path $TripletDir 'bin'
foreach ($required in @('heif.dll', 'libde265.dll')) {
    if (-not (Test-Path (Join-Path $binDir $required))) {
        throw "$required not found in $binDir"
    }
}
$dlls = @(Get-ChildItem -Path $binDir -Filter '*.dll')
$targets = @('target\debug', 'target\release')
foreach ($rel in $targets) {
    $dest = Join-Path $RepoRoot $rel
    New-Item -ItemType Directory -Force -Path $dest | Out-Null
    foreach ($dll in $dlls) { Copy-Item -Force -Path $dll.FullName -Destination $dest }
}
Write-Host "Copied $(($dlls | ForEach-Object { $_.Name }) -join ', ') to $($targets -join ', ')"

# --- 5. Build environment -----------------------------------------------------------------
Write-Host ''
Write-Host 'Build environment (already pinned in .cargo\config.toml [env]):'
Write-Host "  VCPKG_ROOT=$VcpkgDir"
Write-Host "  (packages: $InstallRoot, reached through $classicInstalled)"
Write-Host "  VCPKGRS_TRIPLET=$Triplet"
Write-Host '  VCPKGRS_DYNAMIC=1'
Write-Host 'Done.'
