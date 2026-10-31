<#
.SYNOPSIS
    Writes the website's build data: site/data/releases.json from the GitHub Releases API,
    and the text the site takes from the repository (PRIVACY.md, README's Code signing
    policy section).

.DESCRIPTION
    Runs under Windows PowerShell 5.1 and pwsh 7, before `zola --root site build`:
        powershell -ExecutionPolicy Bypass -File scripts/site-data.ps1

    1. Reads the releases of -Repo (GITHUB_TOKEN is used when set, read-only), keeps the
       published ones (no drafts, no prereleases), reads each release's SHA256SUMS.txt and
       checks every installer and zip against it and against GitHub's own asset digest.
       Any mismatch, or a draft left in the result, fails the run: the site is never built
       from data it cannot vouch for (design §4).
    2. Writes site/data/releases.json (newest version first; see ConvertTo-SiteReleases).
    3. Writes one page per release and language into site/content/releases
       (<version>.md, <version>.en.md; git-ignored) from docs/releases/<version>.md.
    4. Writes site/data/privacy.ko.md and privacy.en.md (PRIVACY.md by language) and
       site/data/code-signing.md (README's "Code signing policy" section).

    -FromFile and -SumsDir read a saved API response and SHA256SUMS-<version>.txt files
    instead of the network (tests, offline previews).
#>
param(
    [string]$Repo = 'hyunwook-kim-goodflow/light-view',
    [string]$FromFile,
    [string]$SumsDir,
    [string]$DataDir
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'site-lib.ps1')

$root = Split-Path -Parent $PSScriptRoot
if (-not $DataDir) { $DataDir = Join-Path $root 'site\data' }
$notesDir = Join-Path $root 'docs\releases'

if ($FromFile) {
    $api = Get-Content -Raw -Encoding UTF8 $FromFile | ConvertFrom-Json
    if (-not $SumsDir) { $SumsDir = Split-Path -Parent $FromFile }
    $getSums = { param($version) Get-Content -Raw -Encoding UTF8 (Join-Path $SumsDir "SHA256SUMS-$version.txt") }.GetNewClosure()
} else {
    # Windows PowerShell 5.1 does not offer TLS 1.2 by default.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    $headers = @{ 'User-Agent' = 'light-view-site-build'; 'Accept' = 'application/vnd.github+json' }
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    $api = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases?per_page=100" -Headers $headers
    $sumsUrls = @{}
    foreach ($r in @($api)) {
        foreach ($a in @($r.assets)) {
            if ($a.name -eq 'SHA256SUMS.txt' -and $r.tag_name -match '^v(.+)$') { $sumsUrls[$Matches[1]] = $a.browser_download_url }
        }
    }
    $getSums = {
        param($version)
        if (-not $sumsUrls.ContainsKey($version)) { throw "release $version has no SHA256SUMS.txt" }
        $bytes = (Invoke-WebRequest -Uri $sumsUrls[$version] -Headers @{ 'User-Agent' = 'light-view-site-build' } -UseBasicParsing).Content
        if ($bytes -is [byte[]]) { [Text.Encoding]::UTF8.GetString($bytes) } else { [string]$bytes }
    }.GetNewClosure()
}

$site = ConvertTo-SiteReleases -ApiReleases $api -GetSums $getSums -NotesDir $notesDir
Assert-SiteReleases -Site $site -ApiReleases $api
# Paths in the data are repository-relative, with forward slashes.
foreach ($r in @($site.releases)) {
    if ($r.notes_file) { $r.notes_file = 'docs/releases/' + (Split-Path -Leaf $r.notes_file) }
}
Write-Utf8File (Join-Path $DataDir 'releases.json') ($site | ConvertTo-Json -Depth 6)
if (-not $FromFile) { Write-ReleasePages -Site $site -ContentDir (Join-Path $root 'site/content/releases') }

$privacy = Split-PrivacyPolicy (Get-Content -Raw -Encoding UTF8 (Join-Path $root 'PRIVACY.md'))
Write-Utf8File (Join-Path $DataDir 'privacy.ko.md') $privacy.ko
Write-Utf8File (Join-Path $DataDir 'privacy.en.md') $privacy.en
$signing = Get-ReadmeSection (Get-Content -Raw -Encoding UTF8 (Join-Path $root 'README.md')) 'Code signing policy'
Write-Utf8File (Join-Path $DataDir 'code-signing.md') $signing

Write-Host "site data: $(@($site.releases).Count) release(s), latest $($site.latest) -> $DataDir"
