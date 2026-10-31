<#
.SYNOPSIS
    Checks the built website (site/public) against the rules in the design: downloads only
    from GitHub Releases, nothing loaded from other sites, copied text (PRIVACY.md, the
    Code signing policy) present and unchanged, a page for every published release, and a
    "Code signing policy" link on every page (SignPath's terms; design §7). Ads (design §9):
    with ads off in site/config.toml, no page has ad code and there is no ads.txt; with ads
    on, slots stay off the download, policy and 404 pages, one per page, away from
    download links and the highlighter.

    -AdsTest also builds a copy of the site with ads turned on (a fake publisher ID, into a
    temporary folder, with -Zola) and runs the ad checks on it, so the slot rules are
    checked while ads are still off.

.DESCRIPTION
    Runs under Windows PowerShell 5.1 and pwsh 7, after scripts/site-data.ps1 and
    `zola --root site build`:
        powershell -ExecutionPolicy Bypass -File scripts/site-check.ps1
    Prints every problem and exits 1 if there is any.
#>
param(
    [string]$PublicDir,
    [string]$DataDir,
    [string]$Repo = 'hyunwook-kim-goodflow/light-view',
    [string]$BaseUrl = 'https://lightview.goodflow.dev',
    [switch]$AdsTest,
    [string]$Zola = 'zola'
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'site-lib.ps1')

$root = Split-Path -Parent $PSScriptRoot
if (-not $PublicDir) { $PublicDir = Join-Path $root 'site/public' }
if (-not $DataDir) { $DataDir = Join-Path $root 'site/data' }

function Read-Data([string]$Name) { [IO.File]::ReadAllText((Join-Path $DataDir $Name), [Text.Encoding]::UTF8) }

$config = [IO.File]::ReadAllText((Join-Path $root 'site/config.toml'), [Text.Encoding]::UTF8)
$adsOn = Test-AdsEnabled $config
$allowed = @('https://static.cloudflareinsights.com/')
if ($adsOn) { $allowed += 'https://pagead2.googlesyndication.com/' }

$pages = Get-SitePages $PublicDir
$problems = @()
if ($adsOn) {
    $problems += Test-AdSlots $pages
} else {
    $problems += Test-NoAds $pages
    if (Test-Path -LiteralPath (Join-Path $PublicDir 'ads.txt')) { $problems += 'ads.txt: present while ads are off' }
}
if ($AdsTest) {
    # Same site, ads on with a fake publisher ID, built into a temporary folder.
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('lv-site-adstest-' + [guid]::NewGuid())
    $testConfig = Join-Path $root 'site/config.adstest.toml'
    try {
        $on = ($config -replace "`r`n", "`n") -replace '(?ms)(\[extra\.ads\]\s*\nenabled\s*=\s*)false', '${1}true'
        $on = $on -replace '(?ms)(\[extra\.ads\][^\[]*?\nclient\s*=\s*)""', '${1}"ca-pub-0000000000000000"'
        if (-not (Test-AdsEnabled $on)) { throw 'could not turn ads on in a copy of site/config.toml' }
        Write-Utf8File $testConfig $on
        & $Zola --root (Join-Path $root 'site') --config $testConfig build --output-dir $tmp --force | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "zola build with ads on failed ($LASTEXITCODE)" }
        $adPages = Get-SitePages $tmp
        $slots = @($adPages.Keys | Where-Object { $adPages[$_] -match '\bdata-site-ad=' }).Count
        if ($slots -eq 0) { $problems += 'ads test: no page has an ad slot with ads on' }
        $problems += Test-AdSlots $adPages | ForEach-Object { "ads test: $_" }
        $problems += Test-ExternalRequests $adPages $BaseUrl ($allowed + 'https://pagead2.googlesyndication.com/') | ForEach-Object { "ads test: $_" }
    } finally {
        if (Test-Path -LiteralPath $testConfig) { Remove-Item -LiteralPath $testConfig }
        if (Test-Path -LiteralPath $tmp) { Remove-Item -Recurse -Force -LiteralPath $tmp }
    }
}
$problems += Test-DownloadLinks $pages $Repo -Required @('index.html', 'en/index.html', 'download/index.html', 'en/download/index.html')
$problems += Test-ExternalRequests $pages $BaseUrl $allowed
$problems += Test-CopiedText $pages 'privacy/index.html' (Read-Data 'privacy.ko.md')
$problems += Test-CopiedText $pages 'en/privacy/index.html' (Read-Data 'privacy.en.md')
$problems += Test-CopiedText $pages 'code-signing/index.html' (Read-Data 'code-signing.md')
$problems += Test-CopiedText $pages 'en/code-signing/index.html' (Read-Data 'code-signing.md')

$site = [IO.File]::ReadAllText((Join-Path $DataDir 'releases.json'), [Text.Encoding]::UTF8) | ConvertFrom-Json
$problems += Test-ReleasePages $pages $site
# SignPath's terms: "Code signing policy" on the home, download and release pages.
$required = @()
foreach ($prefix in '', 'en/') {
    $required += "${prefix}index.html", "${prefix}download/index.html", "${prefix}releases/index.html"
    foreach ($r in @($site.releases)) { $required += "${prefix}releases/$($r.version)/index.html" }
}
$problems += Test-CodeSigningLinks $pages $required
# The footer of every page links it too.
$problems += Test-CodeSigningLinks $pages @($pages.Keys)

if ($problems.Count) {
    $problems | ForEach-Object { Write-Host "problem: $_" }
    Write-Host "$($problems.Count) problem(s) in $PublicDir"
    exit 1
}
Write-Host "site check: $($pages.Count) pages, no problems"
