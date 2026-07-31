<#
.SYNOPSIS
    Download the vetted CC0 brush packs into assets/brushes/.

.DESCRIPTION
    Umber does not vendor brush packs into git. They are large, they are not
    ours, and a repository carries its history forever — so the packs are
    fetched on demand into assets/brushes/, which is git-ignored, and the
    library that actually ships is *generated* from them into
    crates/umber-core/assets/builtin-brushes.ron.

    Every pack is checked against its own licence file before anything is kept.
    A pack whose licence cannot be read from the download itself is refused,
    not downloaded-and-hoped-about: see docs/brush-sources.md for the ones that
    were rejected on exactly that ground.

    Preview thumbnails (*_prev.png) are dropped rather than stored. In the
    MyPaint pack the brush *settings* are CC0 but some previews are CC-BY, and
    the cheapest way not to get that wrong is never to have the files.

.PARAMETER Force
    Re-download packs that are already present.

.EXAMPLE
    pwsh tools/fetch-brushes.ps1
    cargo run -p umber-core --example build-brush-library
#>

[CmdletBinding()]
param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Windows PowerShell renders a progress bar for every chunk Invoke-WebRequest
# and Expand-Archive handle, and when the host is not an interactive console
# that rendering dominates the runtime — a two-second download takes minutes.
# Turning it off is not cosmetic.
$ProgressPreference = 'SilentlyContinue'

$RepoRoot = Split-Path -Parent $PSScriptRoot
$BrushRoot = Join-Path $RepoRoot 'assets/brushes'

# One row per pack. `LicenceMarkers` are strings that must all appear in
# `LicenceFile` for the download to be accepted — that is the licence check,
# and it is deliberately done against the archive's own files rather than
# against a note in this script.
$Packs = @(
    [pscustomobject]@{
        Id             = 'mypaint'
        Name           = 'MyPaint default brushes 2.0.2'
        Url            = 'https://github.com/mypaint/mypaint-brushes/archive/refs/tags/v2.0.2.zip'
        Home           = 'https://github.com/mypaint/mypaint-brushes'
        Root           = 'mypaint-brushes-2.0.2'
        Licence        = 'CC0-1.0'
        LicenceFile    = 'Licenses.dep5'
        LicenceMarkers = @('Files: brushes/*', 'License: CC0-1.0')
        Authors        = 'Martin Renold and the MyPaint Development Team; Ramón Miranda; Marcelo "Tanda" Cerviño; David Revoy; Guillaume Loussarévian; Brien Dieterle'
        Keep           = @('brushes/**/*.myb', 'COPYING', 'Licenses.dep5', 'Licenses.md', 'AUTHORS')
        Format         = '.myb (MyPaint, JSON)'
    }
)

function Get-Pack {
    param($Pack)

    $target = Join-Path $BrushRoot $Pack.Id
    if ((Test-Path $target) -and -not $Force) {
        Write-Host "  already present, skipping (use -Force to refresh)"
        return $true
    }

    $work = Join-Path ([System.IO.Path]::GetTempPath()) ("umber-brushes-" + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $work -Force | Out-Null
    try {
        $archive = Join-Path $work 'pack.zip'
        Write-Host "  downloading $($Pack.Url)"
        Invoke-WebRequest -Uri $Pack.Url -OutFile $archive -UseBasicParsing

        Expand-Archive -Path $archive -DestinationPath $work -Force
        $extracted = Join-Path $work $Pack.Root
        if (-not (Test-Path $extracted)) {
            Write-Warning "  archive did not contain '$($Pack.Root)'; skipping"
            return $false
        }

        # The licence check. Everything below this point is conditional on it.
        $licencePath = Join-Path $extracted $Pack.LicenceFile
        if (-not (Test-Path $licencePath)) {
            Write-Warning "  no $($Pack.LicenceFile) in the download; refusing to keep it"
            return $false
        }
        $licenceText = Get-Content $licencePath -Raw
        foreach ($marker in $Pack.LicenceMarkers) {
            if ($licenceText -notlike "*$marker*") {
                Write-Warning "  $($Pack.LicenceFile) does not state '$marker'; refusing to keep it"
                return $false
            }
        }
        Write-Host "  licence verified: $($Pack.Licence)"

        if (Test-Path $target) { Remove-Item $target -Recurse -Force }
        New-Item -ItemType Directory -Path $target -Force | Out-Null

        $kept = 0
        foreach ($pattern in $Pack.Keep) {
            foreach ($file in Get-ChildItem -Path $extracted -Recurse -File) {
                $relative = $file.FullName.Substring($extracted.Length + 1).Replace('\', '/')
                if ($relative -notlike $pattern) { continue }
                # Belt and braces: the previews are CC-BY where the settings are
                # CC0, so they never get copied even if a pattern would match.
                if ($relative -like '*_prev.png') { continue }
                $dest = Join-Path $target $relative
                New-Item -ItemType Directory -Path (Split-Path -Parent $dest) -Force | Out-Null
                Copy-Item $file.FullName $dest -Force
                $kept++
            }
        }
        Write-Host "  kept $kept files in assets/brushes/$($Pack.Id)"
        return $true
    }
    finally {
        Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
    }
}

New-Item -ItemType Directory -Path $BrushRoot -Force | Out-Null

$fetched = @()
foreach ($pack in $Packs) {
    Write-Host "$($pack.Name)"
    if (Get-Pack -Pack $pack) { $fetched += $pack }
}

if ($fetched.Count -eq 0) {
    Write-Warning 'Nothing was fetched; assets/brushes/LICENSES.md left alone.'
    exit 1
}

# The licence record. Written from the same table the download used, so it
# cannot drift from what is actually on disk.
$lines = @(
    '# Brush pack licences',
    '',
    'Generated by `tools/fetch-brushes.ps1` (or its `.sh` twin). Do not edit by hand.',
    '',
    'These packs are downloaded, not vendored: everything under this directory',
    'except this file is git-ignored. The library Umber ships is generated from',
    'them by `cargo run -p umber-core --example build-brush-library`.',
    '',
    'Preview thumbnails (`*_prev.png`) are deliberately **not** downloaded. In the',
    'MyPaint pack the brush settings are CC0 but some previews are CC-BY, and not',
    'having the files is the surest way not to ship them.',
    ''
)
foreach ($pack in $fetched) {
    $lines += @(
        "## $($pack.Name)",
        '',
        "- **Directory:** ``assets/brushes/$($pack.Id)/``",
        "- **Source:** <$($pack.Home)>",
        "- **Downloaded from:** <$($pack.Url)>",
        "- **Licence:** $($pack.Licence), verified against ``$($pack.LicenceFile)`` in the download itself",
        "- **Authors:** $($pack.Authors)",
        "- **Format:** $($pack.Format)",
        ''
    )
}
$licenceFile = Join-Path $BrushRoot 'LICENSES.md'
# Not Set-Content -Encoding utf8: Windows PowerShell 5.1 writes a byte-order
# mark with that, and a BOM in a committed Markdown file shows up as a stray
# glyph everywhere it is rendered. (This script itself is saved *with* a BOM,
# which is the opposite problem — 5.1 reads an unmarked .ps1 as the system
# codepage, which mangles the accented author names below.)
[System.IO.File]::WriteAllText(
    $licenceFile,
    ($lines -join "`n"),
    (New-Object System.Text.UTF8Encoding $false)
)
Write-Host "wrote $licenceFile"
Write-Host ''
Write-Host 'Next: cargo run -p umber-core --example build-brush-library'
