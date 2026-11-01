<#
.SYNOPSIS
    Checks docs/releases/<version>.md and writes its text for the GitHub release.

.DESCRIPTION
    Runs under Windows PowerShell 5.1 and pwsh 7 (the release workflow, before building):
        powershell -ExecutionPolicy Bypass -File scripts/release-notes.ps1 -Version 1.2.3 -Out dist/release-notes.md
    Fails when the notes file is missing, names another version, or lacks the Korean or
    English part (docs/releases/README.md). -Out gets the text without the front matter.
    -Signed 'true' or 'false' (whether this build is code-signed) must match the notes'
    `signed:` flag, so the website never calls an unsigned release signed or the reverse.
#>
param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$Out,
    [ValidateSet('', 'true', 'false')][string]$Signed = ''
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'site-lib.ps1')

$root = Split-Path -Parent $PSScriptRoot
$body = Get-ReleaseNotesBody -NotesDir (Join-Path $root 'docs/releases') -Version $Version -Signed $Signed
if ($Out) {
    if (-not [IO.Path]::IsPathRooted($Out)) { $Out = Join-Path (Get-Location) $Out }
    Write-Utf8File $Out ($body + "`n")
}
Write-Host "release notes: docs/releases/$Version.md"
