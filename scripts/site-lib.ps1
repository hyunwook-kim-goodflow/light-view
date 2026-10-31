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
                # Megabytes with one decimal, invariant culture ('4.5'), for the page text.
                size_mb = ([math]::Round([double]$a.size / 1MB, 1)).ToString('0.0', [Globalization.CultureInfo]::InvariantCulture)
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

# --- Checks over the built site (scripts/site-check.ps1) ------------------------------
# Each Test-* takes $Pages (hashtable: path under site/public, forward slashes -> HTML) and
# returns a list of problems (strings); none means the check passed.

# Every index.html under $PublicDir -> hashtable of relative path -> HTML.
function Get-SitePages([string]$PublicDir) {
    $pages = @{}
    $rootPath = (Resolve-Path -LiteralPath $PublicDir).Path.TrimEnd('\', '/')
    foreach ($f in Get-ChildItem -LiteralPath $rootPath -Recurse -File -Filter '*.html') {
        $rel = $f.FullName.Substring($rootPath.Length + 1).Replace([char]92, [char]47)
        $pages[$rel] = [IO.File]::ReadAllText($f.FullName, [Text.Encoding]::UTF8)
    }
    return $pages
}

# HTML -> text without tags, entities decoded, whitespace collapsed.
function ConvertTo-PlainText([string]$Html) {
    $t = $Html -replace '(?s)<(script|style)\b.*?</\1>', ' '
    # Block tags separate words; inline tags (a, strong, code...) do not.
    $t = $t -replace '</?(p|div|section|li|ul|ol|h[1-6]|td|th|tr|table|br|pre|details|summary|header|footer|main|nav)[^>]*>', ' '
    $t = $t -replace '<[^>]+>', ''
    $t = [Net.WebUtility]::HtmlDecode($t)
    return ($t -replace '\s+', ' ').Trim()
}

# Markdown -> its text lines (links reduced to their text, emphasis and list marks dropped).
function ConvertFrom-MarkdownToLines([string]$Markdown) {
    $lines = @()
    foreach ($line in ($Markdown -split "`r?`n")) {
        $l = $line -replace '\[([^\]]*)\]\([^)]*\)', '$1'
        $l = $l -replace '\*\*|__|`', ''
        $l = $l -replace '^\s*(#+|[-*]|\d+\.)\s+', ''
        $l = ($l -replace '\s+', ' ').Trim()
        if ($l) { $lines += $l }
    }
    return $lines
}

# Download links (data-download) must be GitHub release assets of $Repo; each page in
# $Required must have at least one.
function Test-DownloadLinks($Pages, [string]$Repo, [string[]]$Required = @()) {
    $problems = @()
    $prefix = "https://github.com/$Repo/releases/download/"
    foreach ($path in $Pages.Keys) {
        $links = [regex]::Matches($Pages[$path], '<a\b[^>]*\bdata-download\b[^>]*>')
        foreach ($m in $links) {
            $href = [regex]::Match($m.Value, '\bhref="([^"]*)"').Groups[1].Value
            if (-not $href.StartsWith($prefix)) { $problems += "${path}: download link $href is not a GitHub release asset of $Repo" }
        }
    }
    foreach ($path in $Required) {
        if (-not $Pages.ContainsKey($path)) { $problems += "${path}: page missing"; continue }
        if ($Pages[$path] -notmatch '\bdata-download\b') { $problems += "${path}: no download link" }
    }
    return $problems
}

# Nothing the browser loads by itself (scripts, images, frames, stylesheets, icons) may come
# from another site, except the Cloudflare Web Analytics beacon (design §8). Plain links
# (<a href>) and <link rel="canonical|alternate"> are not requests.
function Test-ExternalRequests($Pages, [string]$BaseUrl, [string[]]$Allowed = @('https://static.cloudflareinsights.com/')) {
    $problems = @()
    $base = $BaseUrl.TrimEnd('/') + '/'
    foreach ($path in $Pages.Keys) {
        $html = $Pages[$path]
        $urls = @()
        foreach ($m in [regex]::Matches($html, '<(script|img|iframe|source|video|audio|embed)\b[^>]*\bsrc="([^"]*)"')) { $urls += $m.Groups[2].Value }
        foreach ($m in [regex]::Matches($html, '<link\b[^>]*>')) {
            $rel = [regex]::Match($m.Value, '\brel="([^"]*)"').Groups[1].Value
            if ($rel -match '^(canonical|alternate)$') { continue }
            $urls += [regex]::Match($m.Value, '\bhref="([^"]*)"').Groups[1].Value
        }
        foreach ($u in $urls) {
            if ($u -notmatch '^(https?:)?//') { continue }
            if ($u.StartsWith($base) -or $u -eq $base.TrimEnd('/')) { continue }
            if (@($Allowed | Where-Object { $u.StartsWith($_) }).Count) { continue }
            $problems += "${path}: loads $u from another site"
        }
    }
    return $problems
}

# Every text line of $Markdown appears in page $Path (text copied at build time, such as
# PRIVACY.md or README's Code signing policy, must not drift or go missing).
function Test-CopiedText($Pages, [string]$Path, [string]$Markdown) {
    if (-not $Pages.ContainsKey($Path)) { return @("${Path}: page missing") }
    $text = ConvertTo-PlainText $Pages[$Path]
    foreach ($line in (ConvertFrom-MarkdownToLines $Markdown)) {
        if (-not $text.Contains($line)) { return @("${Path}: missing text '$line'") }
    }
    return @()
}

# --- Release notes pages ---------------------------------------------------------------

function ConvertTo-TomlString([string]$Text) {
    return '"' + $Text.Replace([string][char]92, [string][char]92 + [char]92).Replace('"', [string][char]92 + '"') + '"'
}

# Writes one Zola page per release and language into $ContentDir (site/content/releases):
# <major>-<minor>-<patch>.md (Korean) and .en.md, from docs/releases/<version>.md when it
# exists. Zola reads a dot in a file name as a language code, so the file name uses dashes
# and `slug` keeps the address /releases/<version>/.
# Earlier generated pages are removed first (the folder's _index files stay).
function Write-ReleasePages($Site, [string]$ContentDir) {
    if (-not (Test-Path -LiteralPath $ContentDir)) { New-Item -ItemType Directory -Path $ContentDir | Out-Null }
    Get-ChildItem -LiteralPath $ContentDir -File | Where-Object { $_.Name -match '^\d+-\d+-\d+(\.en)?\.md$' } | Remove-Item
    $repoRoot = Split-Path -Parent $PSScriptRoot
    foreach ($r in @($Site.releases)) {
        $notes = $null
        if ($r.notes_file) {
            $path = $r.notes_file
            if (-not [IO.Path]::IsPathRooted($path)) { $path = Join-Path $repoRoot $path }
            $notes = Read-ReleaseNotes $path
        }
        foreach ($lang in 'ko', 'en') {
            if ($notes) {
                $body = $notes[$lang]
            } elseif ($lang -eq 'en') {
                $body = "There are no separate notes for this version. See the [GitHub release]($($r.url))."
            } else {
                $body = "이 버전은 따로 쓴 노트가 없어요. [GitHub 릴리스]($($r.url))를 보세요."
            }
            # Notes are written with ### headings under the language part; on the page
            # they sit under the version title, one level up.
            $body = $body -replace '(?m)^###\s', '## '
            $summary = $r.summary_ko
            if ($lang -eq 'en') { $summary = $r.summary_en }
            $front = @(
                '+++'
                "title = $(ConvertTo-TomlString "light-view $($r.version)")"
                "date = $($r.date)"
                "slug = $(ConvertTo-TomlString $r.version)"
                'template = "release.html"'
                ''
                '[extra]'
                "version = $(ConvertTo-TomlString $r.version)"
                "summary = $(ConvertTo-TomlString $summary)"
                '+++'
            ) -join "`n"
            $stem = $r.version.Replace('.', '-')
            $name = "$stem.md"
            if ($lang -eq 'en') { $name = "$stem.en.md" }
            Write-Utf8File (Join-Path $ContentDir $name) ($front + "`n`n" + $body + "`n")
        }
    }
}

# Each page in $Paths must link to the Code signing policy page by that name (SignPath's
# terms: home, download and release pages; design §7).
function Test-CodeSigningLinks($Pages, [string[]]$Paths) {
    $problems = @()
    foreach ($path in $Paths) {
        if (-not $Pages.ContainsKey($path)) { $problems += "${path}: page missing"; continue }
        if ($Pages[$path] -notmatch '<a\b[^>]*href="[^"]*/code-signing/"[^>]*>\s*Code signing policy\s*</a>') {
            $problems += "${path}: no 'Code signing policy' link to /code-signing/"
        }
    }
    return $problems
}

# Every published release has a page in both languages and is linked from both lists.
function Test-ReleasePages($Pages, $Site) {
    $problems = @()
    foreach ($r in @($Site.releases)) {
        foreach ($prefix in '', 'en/') {
            $page = "${prefix}releases/$($r.version)/index.html"
            if (-not $Pages.ContainsKey($page)) { $problems += "${page}: page missing"; continue }
            $list = "${prefix}releases/index.html"
            if (-not $Pages.ContainsKey($list) -or $Pages[$list] -notmatch ('href="[^"]*/' + [regex]::Escape("${prefix}releases/$($r.version)/") + '"')) {
                $problems += "${list}: no link to $($r.version)"
            }
        }
    }
    return $problems
}
