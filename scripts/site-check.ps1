<#
.SYNOPSIS
    Checks the built website (site/public) against the rules in the design: downloads only
    from GitHub Releases, nothing loaded from other sites, copied text (PRIVACY.md, the
    Code signing policy) present and unchanged.

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
    [string]$BaseUrl = 'https://lightview.goodflow.dev'
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'site-lib.ps1')

$root = Split-Path -Parent $PSScriptRoot
if (-not $PublicDir) { $PublicDir = Join-Path $root 'site\public' }
if (-not $DataDir) { $DataDir = Join-Path $root 'site\data' }

function Read-Data([string]$Name) { [IO.File]::ReadAllText((Join-Path $DataDir $Name), [Text.Encoding]::UTF8) }

$pages = Get-SitePages $PublicDir
$problems = @()
$problems += Test-DownloadLinks $pages $Repo -Required @('index.html', 'en/index.html', 'download/index.html', 'en/download/index.html')
$problems += Test-ExternalRequests $pages $BaseUrl
$problems += Test-CopiedText $pages 'privacy/index.html' (Read-Data 'privacy.ko.md')
$problems += Test-CopiedText $pages 'en/privacy/index.html' (Read-Data 'privacy.en.md')
$problems += Test-CopiedText $pages 'code-signing/index.html' (Read-Data 'code-signing.md')
$problems += Test-CopiedText $pages 'en/code-signing/index.html' (Read-Data 'code-signing.md')

if ($problems.Count) {
    $problems | ForEach-Object { Write-Host "problem: $_" }
    Write-Host "$($problems.Count) problem(s) in $PublicDir"
    exit 1
}
Write-Host "site check: $($pages.Count) pages, no problems"
