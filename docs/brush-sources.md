# Brush sources

Every pack considered for Umber's library, whether it is used, and — where it
is not — the specific reason. The rule this list is written against:

> If a source's licence cannot be verified **from its own files**, it does not
> ship. A licence stated on a web page next to a download is not the same thing
> as a licence inside the download.

`tools/fetch-brushes.ps1` enforces that mechanically: a pack is only kept if the
declared licence file is present in the archive and states what it is supposed
to state. See `docs/brushes.md` for what happens to a pack once it is fetched.

**One pack is a recorded exception to that rule**, made deliberately by the
project's owner and marked as such everywhere it appears. It is the rubberduck
entry below. Nothing else gets the same treatment without the same decision
being made again.

## Shipping

### MyPaint default brushes 2.0.2 — CC0-1.0

- Source: <https://github.com/mypaint/mypaint-brushes>
- Archive: `archive/refs/tags/v2.0.2.zip`
- Format: `.myb` (JSON), 196 brushes
- Licence evidence: `Licenses.dep5` in the archive, DEP5 format, with
  `Files: brushes/*` → `License: CC0-1.0`, and per-artist stanzas for
  `brushes/deevad` (David Revoy), `brushes/ramon` (Ramón Miranda),
  `brushes/tanda` (Marcelo Cerviño), `brushes/kaerhon_v1` (Guillaume
  Loussarévian) and `brushes/Dieterle` (Brien Dieterle) — all CC0-1.0.
- Caveat handled: the brush *settings* are CC0, but the pack also carries
  `*_prev.png` preview thumbnails whose licensing is not covered by the same
  stanza. The fetch script never copies them, so they cannot be shipped by
  accident.
- Result: all 196 converted.

This one pack happens to include David Revoy's MyPaint brushes and Ramón
Miranda's, both of which are also distributed separately — with a verified CC0
statement here and without one there. Taking them from this archive is strictly
better.

### David Revoy — Krita brush bundle 2025-01 — CC0-1.0

- Page: <https://www.davidrevoy.com/article1060/krita-brushes-2025-01-bundle>
- Archive: <https://www.peppercarrot.com/extras/resources/deevad-bundle_25.01.zip>,
  pinned by SHA-256 (the author publishes the same checksum on the page).
- Format: `.bundle` (Krita resource bundle), 46 presets, 8.5 MB
- Licence evidence: **passes, and this is the entry that changed.** A Krita
  bundle is a ZIP whose `meta.xml` records its own provenance, and Revoy's says
  `<meta:license>CC-0</meta:license>` and `<dc:author>David Revoy
  (Deevad)</dc:author>`. That is a licence statement inside the download. The
  earlier version of this file skipped the bundle for want of a `.kpp` reader
  and never got as far as looking; the reader exists now, and so does a licence
  check that can open a nested archive.
- Result: **44 of the 46 read**; the other two are `deformbrush` and
  `experimentbrush`, which are separate Krita paint engines rather than brush
  settings. Of the 44, **8 ship**. The rest need a bitmap tip, which the shipped
  library still cannot hold — see "Shipping a stamp" below. All 44 import
  through **Import brushes…** today, tips and all.

### Raghavendra Kamath — Krita brush presets v2.1 — CC0-1.0

- Source: <https://gitlab.com/raghukamath/krita-brush-presets>
- Archive: the `v2.1` tag, so the fetch is reproducible without pinning a bare
  commit.
- Format: `.bundle`, 26 presets
- Licence evidence: **passes.** `LICENSE` at the root of the repository is the
  full CC0 1.0 Universal text, and `README.md` states "## Licence — [CC0]".
  Both are in any archive of it.
- Result: 26 read, **4 ship**; the rest need a bitmap tip or use Krita's
  brush-tip randomness and density, which Umber's dab has no equivalent for.
- The repository also ships the same presets loose under `paintoppresets/`. The
  fetch script takes only the bundle: taking both converts every preset twice,
  under two ids, with two rows in the picker.

### GDQuest — Free Krita brushes for game artists — CC-BY-4.0

- Source: <https://github.com/GDQuest/krita-free-brushes>
- Archive: pinned to commit `c68b0cc`; the repository has no tags.
- Format: `.kpp` with `.gbr` and `.gih` tips beside them, 43 presets
- Licence evidence: **passes.** `README.md` in the repository carries
  "## License: CC-Attribution-4.0 — These brushes are licensed under the
  Creative Commons Attribution 4.0 terms to `GDquest, GDquest.com`".
- **CC-BY, so attribution is not optional.** Every preset generated from this
  pack carries a `Credit` naming GDquest, the library browser prints it on the
  row, and `every_shipped_preset_is_usable_and_attributed` fails the build if
  one ever does not. This is what `BrushPreset::credit` was built for.
- Result: 43 read (7 refused as other paint engines — `spraybrush`,
  `hatchingbrush`, `experimentbrush`, `deformbrush`), **9 ship**.
- The presets name their tips rather than embedding them, and the tips are in a
  sibling `brushes/`. `brushimport::read_file` looks there, which is the
  difference between 14 of these importing as stamps and 22 doing so.

### rubberduck — 60 free GIMP/Krita brushes — CC0-1.0, **declared on the page**

- Page: <https://opengameart.org/content/60-free-gimp-krita-brushes>
- Archive:
  <https://opengameart.org/sites/default/files/60-free-gimp-and-krita-brushes.zip>,
  pinned by SHA-256 `212069242a44ac19c44894df25e93c36dc546d7d84008454cc2d0f22acddaee6`.
- Format: 17 `.gbr` and 43 `.gih`, **269 stamps** in all, 12.8 MB
- Licence evidence: **none inside the download.** OpenGameArt states the licence
  on the submission page, which is exactly the case the rule at the top of this
  file covers, and it is why the earlier version of this entry skipped the pack.

  It is here now because **the project's owner asked for it by name**. That is a
  decision about this source, not a change to the rule. So, precisely:

  - The page's "License(s)" field shows a single Creative Commons Zero mark
    linking to <http://creativecommons.org/publicdomain/zero/1.0/>.
  - The "Author" field is `rubberduck`; the submission is dated
    Friday, 30 October 2015.
  - The body text reads, in full: *"i made 60 free gimp / krita brushes, format
    is .gbr and .gih (animated brushes / brushpipe)."*
  - Read by hand on **2026-08-01**. The SHA-256 above ties that reading to
    exactly those bytes, which is the only substitute available for a licence
    file: it cannot prove the terms, but it can prove that the archive somebody
    checked the page against is the archive you have.

  `tools/fetch-brushes.ps1` prints a warning on every run for this pack, and
  `assets/brushes/LICENSES.md` repeats all of the above. It is deliberately not
  described as "verified".
- Result: **all 60 files read, all 269 stamps import**. **None ships**, because
  every one of them is a bitmap tip — see below.
- The earlier entry also said `.gih` "is not read at all". It is now; see
  `docs/brushes.md`.

## Shipping a stamp

Everything above that does not ship is held back by one thing, and it is worth
stating once rather than five times.

**The shipped library is a single embedded RON, and a bitmap does not go in
it.** `BrushPreset::tip` holds a mask's *name* and resolves it against the
**user's** library. Shipping a stamp needs three things that do not exist:

1. The generator writing masks to `crates/umber-core/assets/tips/` and an
   `include_bytes!` table beside `builtin-brushes.ron`.
2. `preset::builtin()` resolving a shipped tip, which today it cannot.
3. A rule for **which** stamps reproduce faithfully, decided by measurement.

The third is the one that decides it, and there are now numbers. Measured over
the packs above, as 8-bit PNG and as mean coverage:

| Pack | Tips | Encoded | Mean coverage |
|---|---|---|---|
| rubberduck (OpenGameArt) | 269 | **10.7 MB** | 0.14 |
| Revoy 25.01 | 34 | 0.5 MB | 0.23 |
| Raghukamath v2.1 | 34 | 0.9 MB | 0.22 |
| GDQuest | 22 | 0.7 MB | 0.47 |

Two things follow.

- **10.7 MB is not a library, it is a download.** The whole of
  `builtin-brushes.ron` is about 200 KB. Embedding one pack's stamps would
  multiply what every user fetches, on every platform, by fifty, for a pack
  whose brushes cannot yet be painted faithfully anyway.
- **A mean coverage of 0.14 is the Gimp Brushcollection problem again.** GIMP
  composites every dab, so a sparse stamp builds up to solid along a stroke;
  Umber takes a `max` of coverage and applies opacity once at commit (the
  wet-layer design in `CLAUDE.md`), so it cannot build up at all. The
  measurement below, taken on that other pack, applies to these in the same
  proportion.

None of this stops anybody using them. All 269 import through **Import
brushes…**, are saved into the user's own library with their masks, reload, and
paint. The distinction the project draws is between *what you chose* and *what
Umber claims*, and it is the same distinction everywhere else in the importer.

## Not shipping, and why

### David Revoy — MyPaint brush kit v6

- Page: <https://www.davidrevoy.com/article142/ressource-mypaint-brushes>
- **Skipped.** The download is a DeviantArt link (`fav.me/d5jhhbb`) that needs a
  browser session, so it cannot be fetched by a script, and the archive's own
  licence statement therefore cannot be checked before use. The article text
  says public domain while the site footer says CC-BY-4.0 — exactly the
  ambiguity the rule above exists for.
- Not a real loss: 36 of Revoy's brushes are in the MyPaint pack above under a
  verified CC0 stanza, and his 2025 Krita bundle is now fetched as well.

### Vasco Alexander Basque — Gimp Brushcollection — CC0-1.0

- Source: <https://github.com/vascoalexander/gimp-brush-collection>
- Format: `.gbr`, 1022 brushes in twelve folders, 158 MB
- Licence evidence: **passes.** `README.md` in the repository — and therefore in
  any archive of it — carries the Creative Commons chooser's own wording,
  "Gimp Brushcollection by Vasco Alexander Basque is marked with CC0 1.0", plus
  "All Brushes in this Package are created by me. You can use this resource for
  whatever you want without any restrictions."
- **Still not shipped.** The three reasons this entry has always given, revisited:

  1. **Umber cannot paint them the way GIMP does.** Unchanged, and still the one
     that decides it. These are photographic texture stamps, sparse and faint —
     the ones sampled run from 2 % to 5 % mean coverage. Stamping
     `Organic/Organic_000` along a line at the file's own spacing reaches
     **1.00** coverage compositing and **0.48** taking a max: a stroke half as
     strong as the author's. A per-dab build-up mode is the answer and is a
     change to the wet-layer design, not a brush job. That work is under way
     elsewhere; when it lands, this reason goes.
  2. **It is raw material, and says so.** The README calls it "a 'raw' resource
     (not adjusted or proofed) meant to help people creating their own custom
     brushes". Every brush has an empty name field and a spacing of 0.
     **Answerable, and half-answered:** the `.gbr` reader already falls back to
     the file name when the embedded name is empty, and treats a spacing of 0 as
     unset rather than as 1 %. What remains is curation — `Opaque` and `Spot`
     average 91 % and 97 % mean coverage and would reproduce faithfully, so
     those are the folders to take, named by folder.
  3. **158 MB and no tags.** **Answerable:** pin the commit `16b7899`
     (2024-08-27), which the fetch script's `Root` field already supports for
     GitHub's `/archive/<sha>.zip`. The download is still whole-repository,
     which is the cost of a repository with no releases.

  So reasons 2 and 3 are now a fetch-script row and a `Keep` list away, and
  reason 1 is waiting on the dab pass. **Nothing here should be added until it
  lands** — the point of the entry is that a faithful-looking brush that paints
  at half strength is worse than an absent one.

  Note the standard for a brush the *user* imports is deliberately the
  opposite — an approximation of a brush you chose beats a refusal — and these
  import perfectly well today.

### Adobe Photoshop `.abr` packs

- **No pack fetched.** The reader exists (`umber_core::brushimport::abr`, all
  four versions, sampled brushes), but every `.abr` collection looked at is
  either commercially licensed or states its terms on a page rather than in the
  archive, and the exception above is not a precedent. `.abr` is supported for
  files the user already has.

### CC0 paper and grain textures (ambientCG, Poly Haven, Texture Ninja)

- **Skipped for now.** Umber has no grain channel in the dab pass yet, so there
  is nothing to point them at. A Krita bundle's `patterns/` are declined for the
  same reason, and the import says so.

## Adding a pack

1. Add a row to the `$Packs` table in `tools/fetch-brushes.ps1` and the `PACKS`
   line in `tools/fetch-brushes.sh`, including the licence file and the strings
   that must appear in it. The two must stay in step; they write the same tree
   and the same `LICENSES.md`.
   - The licence file may be inside a **nested archive** (`LicenceIn`), which is
     how a Krita `.bundle`'s `meta.xml` is reached.
   - Pin a `Sha256` for a static download. A forge's generated archive is not
     byte-stable, so pin a commit or a tag in the URL instead.
2. Add a row to `PACKS` in `crates/umber-core/examples/build-brush-library.rs`
   with the per-directory authorship, so every preset carries a `Credit`.
3. Run both steps from `docs/brushes.md` and commit the regenerated
   `builtin-brushes.ron` alongside the updated `assets/brushes/LICENSES.md`.

The test `every_shipped_preset_is_usable_and_attributed` fails if any shipped
preset has no credit, which is the backstop for step 2.
