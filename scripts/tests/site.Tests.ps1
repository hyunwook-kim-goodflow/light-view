<#
.SYNOPSIS
    Tests for the website scripts (scripts/site-lib.ps1), on the fixtures in testdata/site.

.DESCRIPTION
    Plain PowerShell, no Pester: runs under Windows PowerShell 5.1 and pwsh 7.
        powershell -ExecutionPolicy Bypass -File scripts/tests/site.Tests.ps1
    Prints one line per test and exits 1 if any failed.
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../site-lib.ps1')

$Fixtures = Join-Path $PSScriptRoot '../../testdata/site'
$script:Passed = 0
$script:Failed = 0

function Test([string]$Name, [scriptblock]$Body) {
    try {
        & $Body
        $script:Passed++
        Write-Host "ok   $Name"
    } catch {
        $script:Failed++
        Write-Host "FAIL $Name : $($_.Exception.Message)"
    }
}

function Assert-Equal($Expected, $Actual, [string]$What = '') {
    if ($Expected -ne $Actual) { throw "$What expected '$Expected', got '$Actual'" }
}

function Assert-True($Value, [string]$What) {
    if (-not $Value) { throw "expected true: $What" }
}

function Assert-Throws([scriptblock]$Body, [string]$Like) {
    $threw = $false
    try { & $Body } catch {
        $threw = $true
        if ($Like -and $_.Exception.Message -notlike "*$Like*") {
            throw "threw '$($_.Exception.Message)', expected *$Like*"
        }
    }
    if (-not $threw) { throw "did not throw (expected *$Like*)" }
}

# Assigned first: Windows PowerShell 5.1's ConvertFrom-Json emits a JSON array as one object.
function Get-FixtureApi {
    $releases = Get-Content -Raw -Encoding UTF8 (Join-Path $Fixtures 'releases-api.json') | ConvertFrom-Json
    $releases
}

# Sums for a version from the fixture folder (SHA256SUMS-<version>.txt).
$FixtureSums = {
    param($version)
    Get-Content -Raw -Encoding UTF8 (Join-Path $Fixtures "SHA256SUMS-$version.txt")
}

function New-FixtureSite {
    ConvertTo-SiteReleases -ApiReleases (Get-FixtureApi) -GetSums $FixtureSums -NotesDir (Join-Path $Fixtures 'notes')
}

function Copy-Release($release, [string]$version, [string]$date) {
    $json = $release | ConvertTo-Json -Depth 6
    $copy = ($json -replace '0\.2\.0', $version) | ConvertFrom-Json
    $copy.published_at = $date
    $copy
}

# --- Task 2: release data -------------------------------------------------------------

Test 'drafts_and_prereleases_are_dropped' {
    $site = New-FixtureSite
    Assert-Equal 1 @($site.releases).Count 'release count'
    Assert-Equal '0.2.0' $site.releases[0].version 'version'
    Assert-Equal '0.2.0' $site.latest 'latest'
}

Test 'draft_left_in_output_fails' {
    $api = Get-FixtureApi
    $site = New-FixtureSite
    $site.releases += [pscustomobject]@{ version = '0.3.0'; tag = 'v0.3.0'; assets = @() }
    Assert-Throws { Assert-SiteReleases -Site $site -ApiReleases $api } 'not a published release'
}

Test 'checksums_match_sums_file' {
    $site = New-FixtureSite
    $setup = @($site.releases[0].assets | Where-Object kind -eq 'setup')[0]
    $portable = @($site.releases[0].assets | Where-Object kind -eq 'portable')[0]
    Assert-Equal 'a773cfb94d889140b176345155196c1f1495ed75cd717fc0f654100bc6d39feb' $setup.sha256 'setup'
    Assert-Equal '11abe1a9d727daaec9f577d6f34e568d3ed7d476bf64ede294a0ff9a9bdc0426' $portable.sha256 'portable'
    Assert-Equal 4702391 $setup.size 'setup size'
    Assert-Equal '4.5' $setup.size_mb 'setup size in MB'
    Assert-Equal 'https://github.com/hyunwook-kim-goodflow/light-view/releases/download/v0.2.0/light-view-0.2.0-setup.exe' $setup.url 'url'
}

Test 'missing_checksum_line_fails' {
    $sums = { param($v) "a773cfb94d889140b176345155196c1f1495ed75cd717fc0f654100bc6d39feb  light-view-$v-setup.exe`n" }
    Assert-Throws { ConvertTo-SiteReleases -ApiReleases (Get-FixtureApi) -GetSums $sums -NotesDir $Fixtures } 'portable.zip'
}

Test 'duplicate_checksum_line_fails' {
    $text = "a773cfb94d889140b176345155196c1f1495ed75cd717fc0f654100bc6d39feb  a.exe`n" +
            "a773cfb94d889140b176345155196c1f1495ed75cd717fc0f654100bc6d39feb  a.exe`n"
    Assert-Throws { Read-Sha256Sums $text } 'twice'
}

Test 'malformed_checksum_line_fails' {
    Assert-Throws { Read-Sha256Sums "abc  a.exe`n" } 'line 1'
}

Test 'sums_must_match_asset_digest' {
    $sums = {
        param($v)
        "0000000000000000000000000000000000000000000000000000000000000000  light-view-$v-setup.exe`n" +
        "11abe1a9d727daaec9f577d6f34e568d3ed7d476bf64ede294a0ff9a9bdc0426  light-view-$v-portable.zip`n"
    }
    Assert-Throws { ConvertTo-SiteReleases -ApiReleases (Get-FixtureApi) -GetSums $sums -NotesDir $Fixtures } 'digest'
}

Test 'asset_kinds_are_classified' {
    Assert-Equal 'setup' (Get-AssetKind 'light-view-0.2.0-setup.exe' '0.2.0')
    Assert-Equal 'portable' (Get-AssetKind 'light-view-0.2.0-portable.zip' '0.2.0')
    Assert-Equal 'sums' (Get-AssetKind 'SHA256SUMS.txt' '0.2.0')
    Assert-Equal $null (Get-AssetKind 'light-view-0.1.0-setup.exe' '0.2.0')
    Assert-Equal $null (Get-AssetKind 'notes.txt' '0.2.0')
}

Test 'latest_is_newest_semver' {
    $api = @(Get-FixtureApi | Where-Object { -not $_.draft -and -not $_.prerelease })[0]
    # 0.10.0 published before 0.9.0: latest is still 0.10.0 (version order, not date).
    $a = Copy-Release $api '0.10.0' '2026-11-01T00:00:00Z'
    $b = Copy-Release $api '0.9.0' '2026-11-02T00:00:00Z'
    $sums = { param($v) (& $FixtureSums '0.2.0') -replace '0\.2\.0', $v }
    $site = ConvertTo-SiteReleases -ApiReleases @($b, $a) -GetSums $sums -NotesDir $Fixtures
    Assert-Equal '0.10.0' $site.latest 'latest'
    Assert-Equal '0.10.0' $site.releases[0].version 'first'
}

Test 'signed_comes_from_notes_front_matter' {
    $site = New-FixtureSite
    Assert-Equal $true $site.releases[0].signed 'signed'
    Assert-Equal 'notes/0.2.0.md' (($site.releases[0].notes_file -replace '\\', '/') -replace '^.*(notes/)', '$1') 'notes file'
    Assert-Equal 'Fixture summary' $site.releases[0].summary_en 'summary_en'
}

Test 'release_without_notes_has_no_notes_file' {
    $site = ConvertTo-SiteReleases -ApiReleases (Get-FixtureApi) -GetSums $FixtureSums -NotesDir (Join-Path $Fixtures 'none')
    Assert-Equal $null $site.releases[0].notes_file 'notes_file'
    Assert-Equal $false $site.releases[0].signed 'signed'
}

Test 'privacy_md_splits_by_language' {
    $parts = Split-PrivacyPolicy (Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot '../../PRIVACY.md'))
    Assert-True ($parts.en -like '*does not collect, store or send personal data*') 'english part'
    Assert-True ($parts.ko -like '*개인정보를 모으거나 저장하거나 보내지 않아요*') 'korean part'
    Assert-True ($parts.en -notlike '*개인정보를*') 'english part has no korean'
}

Test 'code_signing_section_is_extracted' {
    $text = Get-ReadmeSection (Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot '../../README.md')) 'Code signing policy'
    Assert-True ($text -like '*Free code signing provided by*SignPath Foundation*') 'signpath sentence'
    Assert-True ($text -notlike '*## HEVC*') 'stops at the next section'
}

Test 'missing_readme_section_fails' {
    Assert-Throws { Get-ReadmeSection "# x`n## Other`ntext" 'Code signing policy' } 'Code signing policy'
}

Test 'site_data_script_writes_json_from_fixture' {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("lv-site-test-" + [guid]::NewGuid())
    try {
        $script = Join-Path $PSScriptRoot '../site-data.ps1'
        # The same PowerShell that runs the tests (powershell.exe locally, pwsh on CI).
        $null = & (Get-Process -Id $PID).Path -NoProfile -ExecutionPolicy Bypass -File $script -FromFile (Join-Path $Fixtures 'releases-api.json') -DataDir $dir
        Assert-Equal 0 $LASTEXITCODE 'exit code'
        $json = [IO.File]::ReadAllText((Join-Path $dir 'releases.json')) | ConvertFrom-Json
        Assert-Equal '0.2.0' $json.latest 'latest'
        Assert-True (Test-Path (Join-Path $dir 'privacy.en.md')) 'privacy.en.md'
        Assert-True (Test-Path (Join-Path $dir 'code-signing.md')) 'code-signing.md'
    } finally {
        if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    }
}

# --- Task 3: built pages -------------------------------------------------------------

$Repo = 'hyunwook-kim-goodflow/light-view'
$Base = 'https://lightview.goodflow.dev'

function Page([string]$Body) { "<!doctype html><html><head></head><body>$Body</body></html>" }

Test 'download_button_points_to_github_asset' {
    $good = @{ 'index.html' = (Page "<a class=`"btn-download`" href=`"https://github.com/$Repo/releases/download/v0.2.0/light-view-0.2.0-setup.exe`" data-download>x</a>") }
    Assert-Equal 0 @(Test-DownloadLinks $good $Repo).Count 'good page'
    $bad = @{ 'index.html' = (Page '<a href="https://downloads.example/light-view.exe" data-download>x</a>') }
    Assert-Equal 1 @(Test-DownloadLinks $bad $Repo).Count 'bad page'
    $none = @{ 'download/index.html' = (Page '<p>no button</p>') }
    Assert-Equal 1 @(Test-DownloadLinks $none $Repo -Required @('download/index.html')).Count 'missing button'
}

Test 'no_external_requests_when_ads_off' {
    $ok = @{ 'index.html' = (Page "<link rel=`"stylesheet`" href=`"$Base/site.css`"><img src=`"/img/a.png`"><script defer src=`"https://static.cloudflareinsights.com/beacon.min.js`"></script><a href=`"https://github.com/x`">link</a>") }
    Assert-Equal 0 @(Test-ExternalRequests $ok $Base).Count 'allowed'
    $font = @{ 'index.html' = (Page '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=X">') }
    Assert-Equal 1 @(Test-ExternalRequests $font $Base).Count 'font'
    $script = @{ 'a/index.html' = (Page '<script src="https://cdn.example/x.js"></script><img src="https://img.example/a.png">') }
    Assert-Equal 2 @(Test-ExternalRequests $script $Base).Count 'script and image'
}

Test 'privacy_page_contains_privacy_md' {
    $md = "light-view does not collect, store or send personal data. It has no analytics.`n`n**The one network feature** is the update check."
    $good = @{ 'en/privacy/index.html' = (Page '<p>light-view does not collect, store or send personal data. It has no analytics.</p><p><strong>The one network feature</strong> is the update check.</p>') }
    Assert-Equal 0 @(Test-CopiedText $good 'en/privacy/index.html' $md).Count 'copied'
    $bad = @{ 'en/privacy/index.html' = (Page '<p>We collect nothing.</p>') }
    Assert-Equal 1 @(Test-CopiedText $bad 'en/privacy/index.html' $md).Count 'not copied'
}

Test 'code_signing_page_matches_readme_section' {
    $md = 'Free code signing provided by [SignPath.io](https://about.signpath.io), certificate by [SignPath Foundation](https://signpath.org).'
    $good = @{ 'code-signing/index.html' = (Page '<p>Free code signing provided by <a href="https://about.signpath.io">SignPath.io</a>, certificate by <a href="https://signpath.org">SignPath Foundation</a>.</p>') }
    Assert-Equal 0 @(Test-CopiedText $good 'code-signing/index.html' $md).Count 'copied'
    Assert-Equal 1 @(Test-CopiedText @{} 'code-signing/index.html' $md).Count 'page missing'
}

Test 'plain_text_keeps_block_boundaries' {
    Assert-Equal 'one two three.' (ConvertTo-PlainText '<p>one</p><p>two <a href="x">three</a>.</p>')
}

# --- Task 4: release notes pages ------------------------------------------------------

Test 'notes_split_by_language' {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("lv-site-test-" + [guid]::NewGuid())
    try {
        Write-ReleasePages -Site (New-FixtureSite) -ContentDir $dir
        $ko = [IO.File]::ReadAllText((Join-Path $dir '0-2-0.md'), [Text.Encoding]::UTF8)
        $en = [IO.File]::ReadAllText((Join-Path $dir '0-2-0.en.md'), [Text.Encoding]::UTF8)
        Assert-True ($ko -like '*한국어 노트*') 'korean notes'
        Assert-True ($ko -notlike '*English note*') 'no english in korean page'
        Assert-True ($en -like '*English note*') 'english notes'
        Assert-True ($en -notlike '*한국어 노트*') 'no korean in english page'
        Assert-True ($ko -like '*version = "0.2.0"*') 'front matter version'
        Assert-True ($ko -like '*slug = "0.2.0"*') 'slug keeps the dotted address'
        Assert-True ($ko -like '*date = 2026-10-31*') 'front matter date'
    } finally {
        if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    }
}

Test 'release_without_notes_gets_a_page' {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("lv-site-test-" + [guid]::NewGuid())
    try {
        $site = ConvertTo-SiteReleases -ApiReleases (Get-FixtureApi) -GetSums $FixtureSums -NotesDir (Join-Path $Fixtures 'none')
        Write-ReleasePages -Site $site -ContentDir $dir
        Assert-True (Test-Path (Join-Path $dir '0-2-0.md')) 'korean page'
        Assert-True (Test-Path (Join-Path $dir '0-2-0.en.md')) 'english page'
    } finally {
        if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    }
}

Test 'code_signing_policy_on_required_pages' {
    $link = '<a href="https://lightview.goodflow.dev/code-signing/">Code signing policy</a>'
    $pages = @{ 'index.html' = (Page $link); 'download/index.html' = (Page '<p>Code signing policy</p>') }
    $problems = @(Test-CodeSigningLinks $pages @('index.html', 'download/index.html', 'releases/index.html'))
    Assert-Equal 2 $problems.Count 'download has the words but no link; releases missing'
    $en = @{ 'en/index.html' = (Page '<a href="https://lightview.goodflow.dev/en/code-signing/">Code signing policy</a>') }
    Assert-Equal 0 @(Test-CodeSigningLinks $en @('en/index.html')).Count 'english link'
}

Test 'release_page_lists_every_published_version' {
    $site = New-FixtureSite
    $list = Page '<a href="https://lightview.goodflow.dev/releases/0.2.0/">0.2.0</a>'
    $pages = @{ 'releases/index.html' = $list; 'releases/0.2.0/index.html' = (Page 'x') }
    Assert-Equal 1 @(Test-ReleasePages $pages $site).Count 'english page missing'
    $pages['en/releases/index.html'] = (Page '<a href="https://lightview.goodflow.dev/en/releases/0.2.0/">0.2.0</a>')
    $pages['en/releases/0.2.0/index.html'] = (Page 'x')
    Assert-Equal 0 @(Test-ReleasePages $pages $site).Count 'complete'
    $pages['releases/index.html'] = (Page '<p>empty</p>')
    Assert-Equal 1 @(Test-ReleasePages $pages $site).Count 'list without the version'
}

# --- Task 5: release notes for the release workflow ----------------------------------

Test 'release_needs_notes_file' {
    $notes = Join-Path $Fixtures 'notes'
    Assert-Throws { Get-ReleaseNotesBody -NotesDir $notes -Version '9.9.9' } 'docs/releases/9.9.9.md'
    $body = Get-ReleaseNotesBody -NotesDir $notes -Version '0.2.0'
    Assert-True ($body -like '## 한국어*한국어 노트*## English*English note*') 'both parts, in order'
    Assert-True ($body -notlike '*summary_ko*') 'no front matter'
}

Test 'release_notes_version_must_match_tag' {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("lv-site-test-" + [guid]::NewGuid())
    try {
        New-Item -ItemType Directory -Path $dir | Out-Null
        Write-Utf8File (Join-Path $dir '1.0.0.md') "---`nversion: 1.0.1`n---`n## 한국어`n`n- a`n`n## English`n`n- b`n"
        Assert-Throws { Get-ReleaseNotesBody -NotesDir $dir -Version '1.0.0' } "version '1.0.1'"
        Write-Utf8File (Join-Path $dir '1.0.0.md') "---`nversion: 1.0.0`n---`n## English`n`n- b`n"
        Assert-Throws { Get-ReleaseNotesBody -NotesDir $dir -Version '1.0.0' } '## 한국어'
    } finally {
        if (Test-Path $dir) { Remove-Item -Recurse -Force $dir }
    }
}

Test 'repository_release_notes_are_valid' {
    $dir = Join-Path $PSScriptRoot '../../docs/releases'
    foreach ($f in Get-ChildItem -LiteralPath $dir -Filter '*.md' | Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.md$' }) {
        $null = Get-ReleaseNotesBody -NotesDir $dir -Version ($f.BaseName)
    }
}

# --- Task 7: ad slots -----------------------------------------------------------------

$Ad = '<aside class="ad-slot" data-site-ad="home"><span class="ad-label">광고</span><ins class="adsbygoogle"></ins></aside>'
$Hero = '<section class="hero"><h1>x</h1><a class="btn-download" href="https://github.com/a" data-download>Download</a></section>'
$Features = '<section><h2>Features</h2><p>text</p></section>'

Test 'ad_slot_never_on_download_page' {
    $pages = @{ 'download/index.html' = (Page "$Hero$Features$Ad"); 'en/privacy/index.html' = (Page "$Features$Ad") }
    Assert-Equal 2 @(Test-AdSlots $pages).Count 'download and privacy'
}

Test 'ad_slot_not_in_download_section' {
    $next = @{ 'index.html' = (Page "$Hero$Ad$Features") }
    Assert-Equal 1 @(Test-AdSlots $next).Count 'ad right after the download section'
    $apart = @{ 'index.html' = (Page "$Hero$Features$Ad<section><h2>FAQ</h2></section>") }
    Assert-Equal 0 @(Test-AdSlots $apart).Count 'a headed section in between'
}

Test 'one_ad_slot_per_page' {
    $two = @{ 'faq/index.html' = (Page "$Features$Ad$Features$Ad") }
    Assert-Equal 1 @(Test-AdSlots $two).Count 'two slots'
}

Test 'no_highlighter_near_ad' {
    $marked = @{ 'releases/index.html' = (Page "$Features<section><h2>y</h2><p><mark>free</mark></p>$Ad</section>") }
    Assert-Equal 1 @(Test-AdSlots $marked).Count 'mark next to the ad'
}

Test 'ads_off_leaves_no_trace' {
    $clean = @{ 'index.html' = (Page $Features) }
    Assert-Equal 0 @(Test-NoAds $clean).Count 'clean'
    $script = @{ 'index.html' = (Page '<script async src="https://pagead2.googlesyndication.com/pagead/js/adsbygoogle.js"></script>') }
    Assert-Equal 1 @(Test-NoAds $script).Count 'adsense script'
    $slot = @{ 'faq/index.html' = (Page $Ad) }
    Assert-Equal 1 @(Test-NoAds $slot).Count 'slot'
}

Test 'ads_txt_follows_config' {
    Assert-Equal 'google.com, pub-1234567890123456, DIRECT, f08c47fec0942fa0' (Get-AdsTxt "[extra.ads]`nenabled = true`nclient = `"ca-pub-1234567890123456`"`n")
    Assert-Equal $null (Get-AdsTxt "[extra.ads]`nenabled = false`nclient = `"ca-pub-1234567890123456`"`n")
    Assert-Equal $null (Get-AdsTxt "[extra.ads]`nenabled = true`nclient = `"`"`n")
}

Write-Host ''
Write-Host "$script:Passed passed, $script:Failed failed"
if ($script:Failed -gt 0) { exit 1 }
