<#
.SYNOPSIS
    Tests for the website scripts (scripts/site-lib.ps1), on the fixtures in testdata/site.

.DESCRIPTION
    Plain PowerShell, no Pester: runs under Windows PowerShell 5.1 and pwsh 7.
        powershell -ExecutionPolicy Bypass -File scripts/tests/site.Tests.ps1
    Prints one line per test and exits 1 if any failed.
#>
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\site-lib.ps1')

$Fixtures = Join-Path $PSScriptRoot '..\..\testdata\site'
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
    $parts = Split-PrivacyPolicy (Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot '..\..\PRIVACY.md'))
    Assert-True ($parts.en -like '*does not collect, store or send personal data*') 'english part'
    Assert-True ($parts.ko -like '*개인정보를 모으거나 저장하거나 보내지 않아요*') 'korean part'
    Assert-True ($parts.en -notlike '*개인정보를*') 'english part has no korean'
}

Test 'code_signing_section_is_extracted' {
    $text = Get-ReadmeSection (Get-Content -Raw -Encoding UTF8 (Join-Path $PSScriptRoot '..\..\README.md')) 'Code signing policy'
    Assert-True ($text -like '*Free code signing provided by*SignPath Foundation*') 'signpath sentence'
    Assert-True ($text -notlike '*## HEVC*') 'stops at the next section'
}

Test 'missing_readme_section_fails' {
    Assert-Throws { Get-ReadmeSection "# x`n## Other`ntext" 'Code signing policy' } 'Code signing policy'
}

Test 'site_data_script_writes_json_from_fixture' {
    $dir = Join-Path ([IO.Path]::GetTempPath()) ("lv-site-test-" + [guid]::NewGuid())
    try {
        $script = Join-Path $PSScriptRoot '..\site-data.ps1'
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

Write-Host ''
Write-Host "$script:Passed passed, $script:Failed failed"
if ($script:Failed -gt 0) { exit 1 }
