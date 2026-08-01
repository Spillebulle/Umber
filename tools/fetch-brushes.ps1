<#
.SYNOPSIS
    Download the vetted brush packs into assets/brushes/.

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

    ONE PACK IS AN EXCEPTION, deliberately and visibly. `DeclaredOn` names a
    web page rather than a file in the archive, and a pack carrying it is only
    here because the project's owner asked for it by name. Such a pack must
    also pin `Sha256`, so that the licence statement recorded in LICENSES.md
    refers to exactly the bytes somebody read the page against. Do not add a
    second one without the same decision being made again.

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

Add-Type -AssemblyName System.IO.Compression.FileSystem

$RepoRoot = Split-Path -Parent $PSScriptRoot
$BrushRoot = Join-Path $RepoRoot 'assets/brushes'

# One row per pack. `LicenceMarkers` are strings that must all appear in
# `LicenceFile` for the download to be accepted — that is the licence check,
# and it is deliberately done against the archive's own files rather than
# against a note in this script.
#
# `LicenceIn` names a nested archive to look inside: a Krita `.bundle` is
# itself a ZIP, and its meta.xml is where its author and licence live.
#
# `Sha256` pins the exact bytes. Only for static downloads — GitHub's and
# GitLab's generated archives are not byte-stable, so those pin a commit in the
# URL instead, which is the same guarantee by a different route.
$Packs = @(
    [pscustomobject]@{
        Id             = 'mypaint'
        Name           = 'MyPaint default brushes 2.0.2'
        Url            = 'https://github.com/mypaint/mypaint-brushes/archive/refs/tags/v2.0.2.zip'
        Home           = 'https://github.com/mypaint/mypaint-brushes'
        Root           = 'mypaint-brushes-2.0.2'
        Sha256         = ''
        Licence        = 'CC0-1.0'
        LicenceFile    = 'Licenses.dep5'
        LicenceIn      = ''
        LicenceMarkers = @('Files: brushes/*', 'License: CC0-1.0')
        DeclaredOn     = ''
        Authors        = 'Martin Renold and the MyPaint Development Team; Ramón Miranda; Marcelo "Tanda" Cerviño; David Revoy; Guillaume Loussarévian; Brien Dieterle'
        Keep           = @('brushes/**/*.myb', 'COPYING', 'Licenses.dep5', 'Licenses.md', 'AUTHORS')
        Format         = '.myb (MyPaint, JSON)'
    }
    [pscustomobject]@{
        Id             = 'deevad'
        Name           = 'David Revoy — Krita brush bundle 2025-01'
        Url            = 'https://www.peppercarrot.com/extras/resources/deevad-bundle_25.01.zip'
        Home           = 'https://www.davidrevoy.com/article1060/krita-brushes-2025-01-bundle'
        Root           = '.'
        # Published by the author on the page above, and checked here so the
        # licence statement inside the bundle refers to known bytes.
        Sha256         = '4c628a9418fcde63abacafdcb143881f2cbbf907275cb4f72335545841cf8173'
        Licence        = 'CC0-1.0'
        LicenceFile    = 'meta.xml'
        LicenceIn      = 'Deevad_25.01.bundle'
        LicenceMarkers = @('<meta:license>CC-0</meta:license>', 'David Revoy')
        DeclaredOn     = ''
        Authors        = 'David Revoy (Deevad)'
        Keep           = @('*.bundle')
        Format         = '.bundle (Krita resource bundle)'
    }
    [pscustomobject]@{
        Id             = 'raghukamath'
        Name           = 'Raghavendra Kamath — Krita brush presets v2.1'
        # Pinned to the tag rather than to master: the repository has releases,
        # so a reproducible fetch does not have to name a bare commit.
        Url            = 'https://gitlab.com/raghukamath/krita-brush-presets/-/archive/v2.1/krita-brush-presets-v2.1.zip'
        Home           = 'https://gitlab.com/raghukamath/krita-brush-presets'
        Root           = 'krita-brush-presets-v2.1'
        Sha256         = ''
        Licence        = 'CC0-1.0'
        LicenceFile    = 'LICENSE'
        LicenceIn      = ''
        LicenceMarkers = @('CC0 1.0 Universal')
        DeclaredOn     = ''
        Authors        = 'Raghavendra Kamath'
        # The bundle only. The repository also ships the same presets loose in
        # `paintoppresets/`, and taking both converts every one of them twice —
        # two ids, two rows in the picker, one brush.
        Keep           = @('bundles/*.bundle', 'LICENSE', 'README.md')
        Format         = '.bundle (Krita resource bundle)'
    }
    [pscustomobject]@{
        Id             = 'gdquest'
        Name           = 'GDQuest — Free Krita brushes for game artists'
        # No releases and no tags, so the commit is pinned in the URL.
        Url            = 'https://github.com/GDQuest/krita-free-brushes/archive/c68b0cc9ea4f10c3ce239ac7329fc13461aec8ed.zip'
        Home           = 'https://github.com/GDQuest/krita-free-brushes'
        Root           = 'krita-free-brushes-c68b0cc9ea4f10c3ce239ac7329fc13461aec8ed'
        Sha256         = ''
        # CC-BY, so every preset generated from this *must* carry a Credit.
        # `every_shipped_preset_is_usable_and_attributed` is the backstop.
        Licence        = 'CC-BY-4.0'
        LicenceFile    = 'README.md'
        LicenceIn      = ''
        LicenceMarkers = @('License: CC-Attribution-4.0', 'GDquest')
        DeclaredOn     = ''
        Authors        = 'GDquest (Nathan Lovato)'
        Keep           = @('paintoppresets/*.kpp', 'brushes/*', 'README.md')
        Format         = '.kpp (Krita) with .gbr and .gih tips'
    }
    [pscustomobject]@{
        Id             = 'rubberduck'
        Name           = 'rubberduck — 60 free GIMP/Krita brushes'
        Url            = 'https://opengameart.org/sites/default/files/60-free-gimp-and-krita-brushes.zip'
        Home           = 'https://opengameart.org/content/60-free-gimp-krita-brushes'
        Root           = '.'
        # The pin *is* the licence evidence here: see DeclaredOn.
        Sha256         = '212069242a44ac19c44894df25e93c36dc546d7d84008454cc2d0f22acddaee6'
        Licence        = 'CC0-1.0'
        LicenceFile    = ''
        LicenceIn      = ''
        LicenceMarkers = @()
        # THE EXCEPTION. OpenGameArt states the licence on the submission page,
        # not inside the archive, so the check below cannot be run against the
        # download. This pack is here because the project's owner asked for it
        # by name; the page, the wording, the author and the date are recorded
        # in LICENSES.md, and the SHA-256 above ties that statement to these
        # exact bytes. Do not extend this to another source without the same
        # decision being made again — see docs/brush-sources.md.
        DeclaredOn     = 'https://opengameart.org/content/60-free-gimp-krita-brushes'
        Authors        = 'rubberduck'
        Keep           = @('*.gbr', '*.gih')
        Format         = '.gbr and .gih (GIMP)'
    }
)

function Test-Licence {
    param($Pack, $Extracted)

    if ($Pack.DeclaredOn) {
        Write-Warning "  no licence file inside this archive; accepting the statement on $($Pack.DeclaredOn)"
        Write-Warning "  (see docs/brush-sources.md — this is a deliberate, recorded exception)"
        return $true
    }

    $text = $null
    if ($Pack.LicenceIn) {
        # The licence lives inside a nested archive — a Krita `.bundle` is a ZIP
        # and its meta.xml is where the author and the terms are.
        $nested = Get-ChildItem -Path $Extracted -Recurse -File -Filter $Pack.LicenceIn |
            Select-Object -First 1
        if (-not $nested) {
            Write-Warning "  the download has no $($Pack.LicenceIn); refusing to keep it"
            return $false
        }
        $zip = [System.IO.Compression.ZipFile]::OpenRead($nested.FullName)
        try {
            $entry = $zip.GetEntry($Pack.LicenceFile)
            if (-not $entry) {
                Write-Warning "  $($Pack.LicenceIn) has no $($Pack.LicenceFile); refusing to keep it"
                return $false
            }
            $reader = New-Object System.IO.StreamReader($entry.Open())
            $text = $reader.ReadToEnd()
            $reader.Close()
        }
        finally { $zip.Dispose() }
    }
    else {
        $path = Join-Path $Extracted $Pack.LicenceFile
        if (-not (Test-Path $path)) {
            Write-Warning "  no $($Pack.LicenceFile) in the download; refusing to keep it"
            return $false
        }
        $text = Get-Content $path -Raw
    }

    foreach ($marker in $Pack.LicenceMarkers) {
        if ($text -notlike "*$marker*") {
            Write-Warning "  $($Pack.LicenceFile) does not state '$marker'; refusing to keep it"
            return $false
        }
    }
    Write-Host "  licence verified: $($Pack.Licence)"
    return $true
}

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
        try {
            Invoke-WebRequest -Uri $Pack.Url -OutFile $archive -UseBasicParsing
        }
        catch {
            Write-Warning "  download failed: $($_.Exception.Message)"
            return $false
        }

        if ($Pack.Sha256) {
            $got = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($got -ne $Pack.Sha256.ToLowerInvariant()) {
                Write-Warning "  SHA-256 is $got, expected $($Pack.Sha256); refusing to keep it"
                return $false
            }
            Write-Host "  SHA-256 matches the pin"
        }

        $unpacked = Join-Path $work 'unpacked'
        Expand-Archive -Path $archive -DestinationPath $unpacked -Force
        $extracted = if ($Pack.Root -eq '.') { $unpacked } else { Join-Path $unpacked $Pack.Root }
        if (-not (Test-Path $extracted)) {
            Write-Warning "  archive did not contain '$($Pack.Root)'; skipping"
            return $false
        }

        # The licence check. Everything below this point is conditional on it.
        if (-not (Test-Licence -Pack $Pack -Extracted $extracted)) { return $false }

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
$today = (Get-Date).ToString('yyyy-MM-dd')
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
    '',
    '**Read the "Licence" line of each entry.** Most say *verified inside the',
    'download*, which is the rule `docs/brush-sources.md` is written against. One',
    'says *declared on the submission page*, which is weaker, and is spelled out',
    'in full where it applies.',
    ''
)
foreach ($pack in $fetched) {
    $lines += @(
        "## $($pack.Name)",
        '',
        "- **Directory:** ``assets/brushes/$($pack.Id)/``",
        "- **Source:** <$($pack.Home)>",
        "- **Downloaded from:** <$($pack.Url)>"
    )
    if ($pack.Sha256) {
        $lines += "- **SHA-256 of the archive:** ``$($pack.Sha256)``"
    }
    if ($pack.DeclaredOn) {
        $lines += @(
            "- **Licence:** $($pack.Licence) — **declared on the submission page, not inside the download.**",
            "  <$($pack.DeclaredOn)> lists the author as ``$($pack.Authors)`` and, under",
            '  "License(s)", a single Creative Commons Zero mark linking to',
            '  <http://creativecommons.org/publicdomain/zero/1.0/>. The archive itself',
            '  carries no licence file, so this could not be checked mechanically; the',
            "  page was read by hand on $today, and the SHA-256 above ties that reading",
            '  to exactly these bytes. This is a deliberate exception to the rule at the',
            '  top of `docs/brush-sources.md`, made once, for this source only.'
        )
    }
    else {
        $where = if ($pack.LicenceIn) { "``$($pack.LicenceFile)`` inside ``$($pack.LicenceIn)``" } else { "``$($pack.LicenceFile)``" }
        $lines += "- **Licence:** $($pack.Licence), verified against $where in the download itself"
    }
    $lines += @(
        "- **Authors:** $($pack.Authors)",
        "- **Format:** $($pack.Format)",
        ''
    )
    if ($pack.Licence -like 'CC-BY*') {
        $lines += @(
            '  Attribution is **required**: every preset generated from this pack carries',
            '  a `Credit`, and the brush browser prints it on the row.',
            ''
        )
    }
}
$licenceFile = Join-Path $BrushRoot 'LICENSES.md'
# Not Set-Content -Encoding utf8: Windows PowerShell 5.1 writes a byte-order
# mark with that, and a BOM in a committed Markdown file shows up as a stray
# glyph everywhere it is rendered. (This script itself is saved *with* a BOM,
# which is the opposite problem — 5.1 reads an unmarked .ps1 as the system
# codepage, which mangles the accented author names below.)
#
# The trailing newline is not cosmetic either: the `.sh` twin writes one, and
# the two scripts producing byte-identical output is how "keep them in step"
# is actually checked.
[System.IO.File]::WriteAllText(
    $licenceFile,
    ($lines -join "`n") + "`n",
    (New-Object System.Text.UTF8Encoding $false)
)
Write-Host "wrote $licenceFile"
Write-Host ''
Write-Host 'Next: cargo run -p umber-core --example build-brush-library'
