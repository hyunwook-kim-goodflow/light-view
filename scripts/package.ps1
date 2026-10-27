<#
.SYNOPSIS
    Builds the release files of light-view: the portable zip, the installer's input folder,
    THIRD_PARTY_NOTICES.md and SHA256SUMS.txt.

.DESCRIPTION
    Runs under Windows PowerShell 5.1, after scripts/setup-deps.ps1:
        powershell -ExecutionPolicy Bypass -File scripts/package.ps1 -Version 0.1.0

    1. Checks that -Version is the workspace version (Cargo.toml), so About shows it.
    2. Builds light-view.exe (lv-app) and lv-worker.exe in release mode with the C runtime
       linked statically (-C target-feature=+crt-static): light-view.exe resolves its
       imports from System32 only, so it must not need vcruntime140.dll. Skipped with
       -SkipBuild (the release workflow signs the built exes first).
    3. Writes THIRD_PARTY_NOTICES.md: notices/native.md (the C libraries), then
       `cargo about generate` over about.hbs (every crate in the shipped executables), then
       the full license texts in notices/ as an appendix.
    4. Fills dist\stage (the installer's input, installer/light-view.iss): light-view.exe,
       lv-worker.exe, the vcpkg DLLs, the Visual C++ runtime DLLs those DLLs need,
       THIRD_PARTY_NOTICES.md, LICENSE-MIT, LICENSE-APACHE and README.txt.
    5. Zips dist\stage plus portable.txt into dist\light-view-<version>-portable.zip, unpacks
       it again and fails if the unpacked files take more than 30 MB (spec §7).
    6. Writes dist\SHA256SUMS.txt for the zip (and an installer already in dist).

    Other modes:
        -NoticesOnly [-NoticesOut <path>]   step 3 alone (default: THIRD_PARTY_NOTICES.md in
                                            the repository root)
        -ChecksumsOnly                       step 6 alone, over every zip and setup exe in
                                            dist (the release workflow runs it after ISCC)

    The build honours CARGO_TARGET_DIR. Needs cargo-about 0.9 (`cargo install --locked
    cargo-about --features cli`) and the Visual C++ build tools (found with vswhere, or
    pass -VcRedistDir <...\x64\Microsoft.VC14x.CRT>).
#>
[CmdletBinding()]
param(
    [string]$Version,
    [switch]$SkipBuild,
    [switch]$NoticesOnly,
    [string]$NoticesOut,
    [switch]$ChecksumsOnly,
    [string]$VcRedistDir
)

Set-StrictMode -Version 2.0
$ErrorActionPreference = 'Stop'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$DistDir = Join-Path $RepoRoot 'dist'
$StageDir = Join-Path $DistDir 'stage'
# Spec §7: installed size at most 30 MB (decimal megabytes, the stricter reading).
$MaxUnpackedBytes = 30 * 1000 * 1000
# The DLLs lv-worker.exe loads: heif.dll -> libde265.dll; turbojpeg.dll -> spng.dll -> z.dll.
$NativeDlls = @('heif.dll', 'libde265.dll', 'turbojpeg.dll', 'spng.dll', 'z.dll')
# The Visual C++ runtime those DLLs import (app-local; lv-worker.exe looks in its own folder).
$VcRuntimeDlls = @('vcruntime140.dll', 'vcruntime140_1.dll', 'msvcp140.dll')

function Invoke-Native {
    param([string]$Exe, [string[]]$Arguments)
    & $Exe @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "'$Exe $($Arguments -join ' ')' failed with exit code $LASTEXITCODE"
    }
}

function Get-WorkspaceVersion {
    $inPackage = $false
    foreach ($line in (Get-Content (Join-Path $RepoRoot 'Cargo.toml'))) {
        if ($line -match '^\s*\[(.+)\]\s*$') { $inPackage = $Matches[1] -eq 'workspace.package'; continue }
        if ($inPackage -and $line -match '^\s*version\s*=\s*"([^"]+)"') { return $Matches[1] }
    }
    throw 'No version in [workspace.package] of Cargo.toml'
}

function Get-TargetDir {
    $dir = $env:CARGO_TARGET_DIR
    if (-not $dir) { return (Join-Path $RepoRoot 'target') }
    if (-not [System.IO.Path]::IsPathRooted($dir)) { $dir = Join-Path $RepoRoot $dir }
    return $dir
}

function Write-Utf8NoBom {
    param([string]$Path, [string]$Text)
    [System.IO.File]::WriteAllText($Path, $Text, (New-Object System.Text.UTF8Encoding($false)))
}

function Read-Text {
    param([string]$Path)
    return ([System.IO.File]::ReadAllText($Path) -replace "`r`n", "`n").TrimEnd("`n")
}

# Step 3: THIRD_PARTY_NOTICES.md.
function New-Notices {
    param([string]$OutFile)
    $notices = Join-Path $RepoRoot 'notices'
    $crates = Join-Path ([System.IO.Path]::GetTempPath()) ("lv-about-" + [guid]::NewGuid() + '.md')
    Push-Location $RepoRoot
    try {
        # Every crate (all targets) present locally, so --frozen (no network, no license
        # lookups outside the crates' own files) sees the whole graph.
        Invoke-Native cargo @('fetch', '--locked')
        Invoke-Native cargo @('about', 'generate', '--workspace', '--frozen', '--fail',
            '-c', 'about.toml', '-o', $crates, 'about.hbs')
    } finally {
        Pop-Location
    }
    $appendix = @(
        @('A', 'GNU Lesser General Public License v3 (libheif, libde265)', 'LGPL-3.0.txt'),
        @('B', 'GNU General Public License v3 (LGPL-3.0이 참조, referenced by the LGPL)', 'GPL-3.0.txt'),
        @('C', 'libjpeg-turbo LICENSE.md (IJG, BSD-3-Clause, Zlib)', 'libjpeg-turbo-LICENSE.md'),
        @('D', 'libjpeg-turbo README.ijg (IJG License)', 'libjpeg-turbo-README.ijg.txt'),
        @('E', 'libspng (BSD-2-Clause)', 'libspng-LICENSE.txt'),
        @('F', 'libpng-2.0 (libspng의 libpng 유래 코드)', 'libpng-LICENSE.txt'),
        @('G', 'zlib', 'zlib-LICENSE.txt')
    )
    $parts = New-Object System.Collections.Generic.List[string]
    $parts.Add((Read-Text (Join-Path $notices 'native.md')))
    $parts.Add((Read-Text $crates))
    $parts.Add("## 부록: 라이선스 전문 (Appendix: license texts)")
    foreach ($entry in $appendix) {
        $text = Read-Text (Join-Path $notices $entry[2])
        if ($text.Contains('```')) { throw "$($entry[2]) contains a code fence" }
        $parts.Add("### 부록 $($entry[0]). $($entry[1])`n`n``````text`n$text`n``````")
    }
    Remove-Item -Force $crates
    $body = ($parts -join "`n`n") + "`n"
    Write-Utf8NoBom -Path $OutFile -Text ($body -replace "`n", "`r`n")
    Write-Host "Wrote $OutFile"
}

function Find-VcRedist {
    if ($VcRedistDir) { return $VcRedistDir }
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path $vswhere)) { throw 'vswhere.exe not found; pass -VcRedistDir' }
    $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $vs) { throw 'No Visual Studio with the C++ x64 tools found; pass -VcRedistDir' }
    $candidates = @(Get-ChildItem -Path (Join-Path $vs 'VC\Redist\MSVC') -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+(\.\d+)+$' } |
        Sort-Object { [version]$_.Name } -Descending)
    foreach ($ver in $candidates) {
        $crt = @(Get-ChildItem -Path (Join-Path $ver.FullName 'x64') -Directory -Filter 'Microsoft.VC*.CRT' -ErrorAction SilentlyContinue)
        foreach ($dir in $crt) {
            $complete = $true
            foreach ($dll in $VcRuntimeDlls) { if (-not (Test-Path (Join-Path $dir.FullName $dll))) { $complete = $false } }
            if ($complete) { return $dir.FullName }
        }
    }
    throw "No Visual C++ x64 redistributable folder under $vs\VC\Redist\MSVC; pass -VcRedistDir"
}

# A Rust exe built with +crt-static names no Visual C++ runtime DLL.
function Assert-NoVcRuntimeImport {
    param([string]$Exe)
    $text = [System.Text.Encoding]::ASCII.GetString([System.IO.File]::ReadAllBytes($Exe))
    if ($text -match '(?i)vcruntime140|msvcp140') {
        throw "$Exe needs the Visual C++ runtime DLLs; it must link the C runtime statically"
    }
}

function Write-Checksums {
    $files = @(Get-ChildItem -Path $DistDir -File | Where-Object { $_.Name -like '*-portable.zip' -or $_.Name -like '*-setup.exe' } | Sort-Object Name)
    if ($files.Count -eq 0) { throw "Nothing to checksum in $DistDir" }
    $lines = foreach ($f in $files) {
        '{0}  {1}' -f (Get-FileHash -Algorithm SHA256 $f.FullName).Hash.ToLowerInvariant(), $f.Name
    }
    $out = Join-Path $DistDir 'SHA256SUMS.txt'
    Write-Utf8NoBom -Path $out -Text (($lines -join "`n") + "`n")
    Write-Host "Wrote $out"
    $lines | ForEach-Object { Write-Host "  $_" }
}

function Get-FolderBytes {
    param([string]$Dir)
    $sum = (Get-ChildItem -Path $Dir -Recurse -File | Measure-Object -Property Length -Sum).Sum
    if (-not $sum) { return 0 }
    return [int64]$sum
}

# ---------------------------------------------------------------------------------------
if ($NoticesOnly) {
    if (-not $NoticesOut) { $NoticesOut = Join-Path $RepoRoot 'THIRD_PARTY_NOTICES.md' }
    New-Notices -OutFile $NoticesOut
    return
}
if ($ChecksumsOnly) {
    Write-Checksums
    return
}

# --- 1. Version --------------------------------------------------------------------------
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Pass -Version x.y.z (got '$Version')" }
$workspaceVersion = Get-WorkspaceVersion
if ($Version -ne $workspaceVersion) {
    throw "-Version $Version differs from the workspace version $workspaceVersion in Cargo.toml"
}

# --- 2. Build ------------------------------------------------------------------------------
$release = Join-Path (Get-TargetDir) 'release'
if (-not $SkipBuild) {
    $previous = $env:RUSTFLAGS
    $env:RUSTFLAGS = '-C target-feature=+crt-static'
    Push-Location $RepoRoot
    try {
        Invoke-Native cargo @('build', '--release', '--locked', '-p', 'lv-app', '-p', 'lv-worker')
    } finally {
        Pop-Location
        $env:RUSTFLAGS = $previous
    }
}
$appExe = Join-Path $release 'lv-app.exe'
$workerExe = Join-Path $release 'lv-worker.exe'
foreach ($exe in @($appExe, $workerExe)) {
    if (-not (Test-Path $exe)) { throw "$exe not found" }
    Assert-NoVcRuntimeImport $exe
}

# --- 3, 4. Stage -------------------------------------------------------------------------
New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
if (Test-Path $StageDir) { Remove-Item -Recurse -Force $StageDir }
New-Item -ItemType Directory -Path $StageDir | Out-Null

Copy-Item $appExe (Join-Path $StageDir 'light-view.exe')
Copy-Item $workerExe (Join-Path $StageDir 'lv-worker.exe')
$vcpkgBin = Join-Path $RepoRoot 'vcpkg_installed\x64-windows\bin'
foreach ($dll in $NativeDlls) {
    $src = Join-Path $vcpkgBin $dll
    if (-not (Test-Path $src)) { throw "$src not found: run scripts/setup-deps.ps1" }
    Copy-Item $src $StageDir
}
$redist = Find-VcRedist
Write-Host "Visual C++ runtime from $redist"
foreach ($dll in $VcRuntimeDlls) { Copy-Item (Join-Path $redist $dll) $StageDir }
New-Notices -OutFile (Join-Path $StageDir 'THIRD_PARTY_NOTICES.md')
Copy-Item (Join-Path $RepoRoot 'LICENSE-MIT') $StageDir
Copy-Item (Join-Path $RepoRoot 'LICENSE-APACHE') $StageDir
Copy-Item (Join-Path $RepoRoot 'installer\README.txt') $StageDir

# --- 5. Portable zip ---------------------------------------------------------------------
$name = "light-view-$Version-portable"
$zip = Join-Path $DistDir "$name.zip"
$work = Join-Path $DistDir "work-$([guid]::NewGuid())"
$portableDir = Join-Path $work $name
try {
    New-Item -ItemType Directory -Force -Path $portableDir | Out-Null
    Copy-Item -Path (Join-Path $StageDir '*') -Destination $portableDir -Recurse
    $marker = "light-view portable: settings and crash records are kept in the 'data' folder next to light-view.exe.`r`n" +
        "light-view 포터블판: 설정과 크래시 기록을 light-view.exe 옆의 data 폴더에 저장해요. 이 파일을 지우면 설치판처럼 %APPDATA%를 써요.`r`n"
    [System.IO.File]::WriteAllText((Join-Path $portableDir 'portable.txt'), $marker, (New-Object System.Text.UTF8Encoding($true)))

    if (Test-Path $zip) { Remove-Item -Force $zip }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    # One top-level folder in the zip, named like the zip.
    [System.IO.Compression.ZipFile]::CreateFromDirectory($portableDir, $zip,
        [System.IO.Compression.CompressionLevel]::Optimal, $true)

    $check = Join-Path $work 'unpacked'
    [System.IO.Compression.ZipFile]::ExtractToDirectory($zip, $check)
    $unpacked = Join-Path $check $name
    foreach ($required in @('light-view.exe', 'lv-worker.exe', 'portable.txt', 'THIRD_PARTY_NOTICES.md',
            'LICENSE-MIT', 'LICENSE-APACHE', 'README.txt') + $NativeDlls + $VcRuntimeDlls) {
        if (-not (Test-Path (Join-Path $unpacked $required))) { throw "$required missing from $zip" }
    }
    $bytes = Get-FolderBytes $check
    $mb = [math]::Round($bytes / 1000000, 2)
    if ($bytes -gt $MaxUnpackedBytes) {
        throw "Unpacked portable folder is $mb MB, over the 30 MB limit (spec §7)"
    }
    Write-Host "Unpacked portable folder: $mb MB ($bytes bytes), zip: $([math]::Round((Get-Item $zip).Length / 1000000, 2)) MB"
} finally {
    if (Test-Path $work) { Remove-Item -Recurse -Force $work }
}

# --- 6. Checksums --------------------------------------------------------------------------
Write-Checksums
Write-Host "Done: $zip, $StageDir"
