<#
.SYNOPSIS
    Functions for the website build (dot-sourced by site-data.ps1, site-check.ps1 and
    scripts/tests/site.Tests.ps1). Runs under Windows PowerShell 5.1 and pwsh 7.

.DESCRIPTION
    Release data comes from the GitHub Releases API at build time (never from visitors'
    browsers): only published releases (no drafts, no prereleases), and each installer and
    portable zip must have exactly one line in that release's SHA256SUMS.txt that also
    matches GitHub's own digest of the asset. Release notes come from
    docs/releases/<version>.md (front matter + a Korean and an English part).
    Design: docs/superpowers/specs/2026-10-31-light-view-site-design.md §4, §6.
#>

Set-StrictMode -Version 2.0

# 'setup', 'portable', 'sums', or $null for an asset name of release $Version.
function Get-AssetKind([string]$Name, [string]$Version) {
    if ($Name -ceq "light-view-$Version-setup.exe") { return 'setup' }
    if ($Name -ceq "light-view-$Version-portable.zip") { return 'portable' }
    if ($Name -ceq 'SHA256SUMS.txt') { return 'sums' }
    return $null
}

# SHA256SUMS.txt text -> hashtable file name -> lower-case hex hash. Throws on a malformed
# or repeated line.
function Read-Sha256Sums([string]$Text) {
    $sums = @{}
    $n = 0
    foreach ($line in ($Text -split "`r?`n")) {
        $n++
        if ($line.Trim() -eq '') { continue }
        if ($line -notmatch '^([0-9a-fA-F]{64}) [ *](\S.*)$') {
            throw "SHA256SUMS.txt line ${n} is not '<64 hex digits>  <file name>': $line"
        }
        $name = $Matches[2].Trim()
        if ($sums.ContainsKey($name)) { throw "SHA256SUMS.txt lists $name twice" }
        $sums[$name] = $Matches[1].ToLowerInvariant()
    }
    return $sums
}

# docs/releases/<version>.md -> @{ front = hashtable; ko = text; en = text }, or $null when
# the file does not exist. Front matter is '---' lines around 'key: value' lines.
function Read-ReleaseNotes([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $text = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $Path), [Text.Encoding]::UTF8) -replace "`r`n", "`n"
    if ($text -notmatch '(?s)^---\n(.*?)\n---\n(.*)$') { throw "$Path does not start with a '---' front matter block" }
    $frontText = $Matches[1]
    $body = $Matches[2]
    $front = @{}
    foreach ($line in ($frontText -split "`n")) {
        if ($line -match '^\s*([A-Za-z_]+)\s*:\s*(.*?)\s*$') { $front[$Matches[1]] = $Matches[2] }
    }
    $ko = ''
    $en = ''
    if ($body -match '(?s)^\s*## 한국어\s*\n(.*?)\n## English\s*\n(.*)$') {
        $ko = $Matches[1].Trim()
        $en = $Matches[2].Trim()
    } else {
        throw "$Path needs a '## 한국어' part followed by a '## English' part"
    }
    return @{ front = $front; ko = $ko; en = $en }
}

# The API's release list -> the site's release data (newest version first). $GetSums is a
# script block taking a version and returning that release's SHA256SUMS.txt text.
function ConvertTo-SiteReleases($ApiReleases, [scriptblock]$GetSums, [string]$NotesDir) {
    $out = @()
    foreach ($r in @($ApiReleases)) {
        if ($r.draft -or $r.prerelease) { continue }
        if ($r.tag_name -notmatch '^v(\d+\.\d+\.\d+)$') { throw "release tag $($r.tag_name) is not v<major>.<minor>.<patch>" }
        $version = $Matches[1]
        $sums = Read-Sha256Sums (& $GetSums $version)
        $assets = @()
        foreach ($a in @($r.assets)) {
            $kind = Get-AssetKind $a.name $version
            if (-not $kind) { continue }
            $sha = $null
            if ($kind -ne 'sums') {
                if (-not $sums.ContainsKey($a.name)) { throw "release $version`: SHA256SUMS.txt has no line for $($a.name)" }
                $sha = $sums[$a.name]
                $digest = $null
                if ($a.PSObject.Properties['digest']) { $digest = $a.digest }
                if ($digest -and $digest -ne "sha256:$sha") {
                    throw "release $version`: SHA256SUMS.txt says $sha for $($a.name) but GitHub's digest is $digest"
                }
            }
            $assets += [pscustomobject][ordered]@{
                kind   = $kind
                name   = $a.name
                size   = [int64]$a.size
                url    = $a.browser_download_url
                sha256 = $sha
            }
        }
        foreach ($need in 'setup', 'portable', 'sums') {
            if (-not @($assets | Where-Object { $_.kind -eq $need }).Count) {
                $file = @{ setup = "light-view-$version-setup.exe"; portable = "light-view-$version-portable.zip"; sums = 'SHA256SUMS.txt' }[$need]
                throw "release $version has no asset $file"
            }
        }
        $notesPath = Join-Path $NotesDir "$version.md"
        $notes = Read-ReleaseNotes $notesPath
        $signed = $false
        $summaryKo = ''
        $summaryEn = ''
        $notesFile = $null
        if ($notes) {
            if ($notes.front['version'] -ne $version) { throw "$notesPath says version '$($notes.front['version'])', expected $version" }
            $signed = ($notes.front['signed'] -eq 'true')
            $summaryKo = [string]$notes.front['summary_ko']
            $summaryEn = [string]$notes.front['summary_en']
            $notesFile = $notesPath
        }
        $out += [pscustomobject][ordered]@{
            version    = $version
            tag        = $r.tag_name
            date       = ([string]$r.published_at).Substring(0, 10)
            url        = $r.html_url
            notes_file = $notesFile
            summary_ko = $summaryKo
            summary_en = $summaryEn
            signed     = $signed
            assets     = $assets
        }
    }
    $sorted = @($out | Sort-Object -Property @{ Expression = { [version]$_.version } } -Descending)
    $latest = $null
    if ($sorted.Count) { $latest = $sorted[0].version }
    return [pscustomobject][ordered]@{ latest = $latest; releases = $sorted }
}

# Throws unless every release in $Site is a published, non-prerelease release in
# $ApiReleases and every installer and zip has a checksum (design §2 anti-goals).
function Assert-SiteReleases($Site, $ApiReleases) {
    foreach ($s in @($Site.releases)) {
        $api = @(@($ApiReleases) | Where-Object { $_.tag_name -eq $s.tag })
        if ($api.Count -ne 1 -or $api[0].draft -or $api[0].prerelease) {
            throw "release $($s.version) is not a published release; drafts and prereleases must not reach the site"
        }
        foreach ($a in @($s.assets)) {
            if ($a.kind -ne 'sums' -and $a.sha256 -notmatch '^[0-9a-f]{64}$') { throw "release $($s.version): $($a.name) has no checksum" }
        }
    }
}

# PRIVACY.md -> @{ en = '...'; ko = '...' } (the text under '## English' and '## 한국어').
function Split-PrivacyPolicy([string]$Text) {
    $t = $Text -replace "`r`n", "`n"
    if ($t -notmatch '(?s)\n## English\s*\n(.*?)\n## 한국어\s*\n(.*)$') {
        throw "PRIVACY.md needs a '## English' part followed by a '## 한국어' part"
    }
    return @{ en = $Matches[1].Trim(); ko = $Matches[2].Trim() }
}

# The body of README section '## <Title>' up to the next '## ' heading.
function Get-ReadmeSection([string]$Text, [string]$Title) {
    $t = $Text -replace "`r`n", "`n"
    $pattern = '(?s)\n## ' + [regex]::Escape($Title) + '[ \t]*\n(.*?)(?=\n## |\z)'
    if ($t -notmatch $pattern) { throw "README.md has no '## $Title' section" }
    return $Matches[1].Trim()
}

# Writes UTF-8 without a byte order mark (Zola and the browser expect plain UTF-8).
function Write-Utf8File([string]$Path, [string]$Text) {
    $dir = Split-Path -Parent $Path
    if ($dir -and -not (Test-Path -LiteralPath $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }
    [IO.File]::WriteAllText($Path, $Text, (New-Object Text.UTF8Encoding $false))
}
