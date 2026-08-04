# Brush sources

Every pack considered for Umber's library, whether it is used, and — where it
is not — the specific reason. The rule this list is written against:

> If a source's licence cannot be verified **from its own files**, it does not
> ship. A licence stated on a web page next to a download is not the same thing
> as a licence inside the download.

`tools/fetch-brushes.ps1` enforces that mechanically: a pack is only kept if the
declared licence file is present in the archive and states what it is supposed
to state. See `docs/brushes.md` for what happens to a pack once it is fetched.

**Shipping a mask is a second, stricter question.** Converting a pack into
settings is a description of somebody's work, made on one machine; embedding its
**bitmap tips** puts the artwork itself in the binary and in this repository, in
every release on every platform. The rule above therefore has to be met in full
before a pack's masks travel, and `Pack::ship_tips` in
`crates/umber-core/examples/build-brush-library.rs` records that decision per
pack. Exactly one pack answers differently to the two questions, and it is
rubberduck's.

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
  settings. Of the 44, **18 ship** — 7 procedural and **11 stamps**, carried by
  7 masks. The rest drop something Umber cannot render. All 44 import through
  **Import brushes…** today, tips and all.
- Masks shipped: yes. CC0 stated in the bundle's own `meta.xml`, which is a
  licence statement inside the download.

### Raghavendra Kamath — Krita brush presets v2.1 — CC0-1.0

- Source: <https://gitlab.com/raghukamath/krita-brush-presets>
- Archive: the `v2.1` tag, so the fetch is reproducible without pinning a bare
  commit.
- Format: `.bundle`, 26 presets
- Licence evidence: **passes.** `LICENSE` at the root of the repository is the
  full CC0 1.0 Universal text, and `README.md` states "## Licence — [CC0]".
  Both are in any archive of it.
- Result: 26 read, **8 ship** — 4 procedural and **4 stamps**, carried by 4
  masks. The rest use Krita's brush-tip randomness and density, which Umber's
  dab has no equivalent for, or drop something else.
- Masks shipped: yes. CC0 stated in the repository's `LICENSE` and `README.md`,
  both of which are in any archive of it.
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
  `hatchingbrush`, `experimentbrush`, `deformbrush`), **11 ship** — 7 procedural
  and **4 stamps**, carried by 4 masks.
- Masks shipped: yes, **with attribution**, which for CC-BY is a condition
  rather than a courtesy. Every preset generated from this pack carries a
  `Credit` naming GDquest whether it stamps a mask or not, and
  `every_shipped_preset_is_usable_and_attributed` fails the build if one does
  not.
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
- Result: **all 60 files read, all 269 stamps import**. **None ships**, and the
  reason has changed: it used to be that the library could not hold a mask, and
  it is now the licence. Of the 269, the 252 that come out of a `.gih` lose the
  pipe's sequencing and would be refused for that whatever happened here; the
  remaining **17**, one per `.gbr`, convert cleanly and would ship but for this
  entry.
- Masks shipped: **no.** This is the pack the rule at the top of this file
  covers, and shipping a mask is where that rule bites hardest. The exception
  recorded above was made so the pack could be *fetched and converted*; putting
  17 pieces of somebody else's artwork inside every Umber release, on every
  platform, is a larger claim than converting them on one machine, and it is not
  one this project makes on evidence it could not check. The cost is exactly 17
  brushes and 1.2 MB of PNG. `ship_tips: false` in
  `crates/umber-core/examples/build-brush-library.rs` is the whole of it, and
  flipping it is a decision for whoever owns the project rather than a default.
- The earlier entry also said `.gih` "is not read at all". It is now; see
  `docs/brushes.md`.

## Shipping a stamp

Stamps ship now: **23 brushes carried by 18 masks, 649 kB of 8-bit greyscale
PNG in `crates/umber-core/assets/tips/`**. The release binary was measured when
the first fifteen of those masks landed — 664 kB, 17,162,752 bytes before and
17,842,176 after — and the three added since have not been weighed again,
because the delta is the PNG and nothing else. The generator writes the masks,
deduplicates them by content and rewrites the `include_bytes!` table;
`preset::builtin()` resolves a shipped tip through `tip::builtin` before the
user's library. `docs/brushes.md` has the mechanism.

What decided the shape of it, in the order the questions were asked.

### How many brushes is this actually about

**338** across the five packs carry a mask. Only **40** of them drop nothing
else, so the other 298 were never candidates whatever the library could hold —
257 are refused for a `.gih` pipe's sequencing alone, and the rest for coloured
stamps, mirrored dabs, brush-tip randomness or a rotation Umber cannot drive.
Of the 40, 23 ship and 17 are rubberduck's.

It was 37 and 19 until Krita's **paint-deposit rate** came off that list.
That one was never a mask Umber could not paint: `Brush::smudge` is the mix
between the palette colour and what a dab lifted, so `1 - smudge` already is a
deposit rate, and what the reader was missing was the enable flag beside the
value. `crates/umber-core/src/brushimport/kpp.rs` has the argument.

### How big the whole thing would have been

Every unique mask the 338 need, as 8-bit PNG, at four maximum long sides:

| Pack | Brushes with a mask | Unique masks | Full size | ≤ 1024 px | ≤ 512 px | ≤ 256 px |
|---|---:|---:|---:|---:|---:|---:|
| Revoy 25.01 | 34 | 24 | 0.5 MB | 0.5 MB | 0.5 MB | 0.3 MB |
| Raghukamath v2.1 | 13 | 12 | 0.5 MB | 0.4 MB | 0.3 MB | 0.1 MB |
| GDQuest | 22 | 14 | 0.5 MB | 0.5 MB | 0.5 MB | 0.2 MB |
| rubberduck (OpenGameArt) | 269 | 265 | **10.9 MB** | 10.9 MB | 10.4 MB | 5.5 MB |
| **all** | **338** | **315** | **12.4 MB** | 12.4 MB | 11.7 MB | 6.1 MB |

Three things follow, and the first two are why the answer is not "downsample".

- **Deduplication is worth having and does not decide anything.** 338 brushes
  need 315 masks: a pack does cut several presets from one stamp, but not
  often. It is worth doing because it is free — `BrushPreset::tip` already holds
  a *name* — and because it saves a GPU upload as well as bytes.
- **A resolution cap buys almost nothing.** The median mask is already 350 px
  long, so capping at 512 saves 6%. Capping at 256 does save half, and costs
  too much for it: eleven masks change their build-up verdict, and one drops
  below the strength at which eight-bit coverage can accumulate at all. **The
  masks that ship are shipped at their original resolution**, which needs no cap
  and no re-measurement.
- **12.4 MB was never the question**, because 315 masks was never the set. The
  masks the shipping brushes need are 33, and without rubberduck's 15.

### The three that ship, and the one that does not

624 kB on a 5.9 MB MSI is about 11%, for 19 brushes — a trade worth making and
stated rather than made quietly. Revoy's, Raghukamath's and GDQuest's licences
are verified inside their downloads, which is what shipping artwork requires.

rubberduck's is not, and that is the finding worth reading twice: **its 17
brushes and 1.2 MB are held back by the licence rule and by nothing else.** The
pack is CC0 as far as anyone can tell — the terms would permit this — but the
statement is on the OpenGameArt submission page and not inside the archive, and
the recorded exception above was made so the pack could be fetched and
converted. Redistributing the masks is a further step. `ship_tips: false` in
`crates/umber-core/examples/build-brush-library.rs` is where that decision
lives, in one line, next to the reason.

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
  whatever you want without any restrictions." That is a licence statement
  inside the download, which is what the rule at the top of this file asks for.
- **Not shipped yet.** It used to be three reasons, and the first of them was
  the one that decided it. That one is now **answered**:

  1. ~~**Umber cannot paint them the way GIMP does.**~~ **Fixed.** These are
     photographic texture stamps, sparse and faint — the ones sampled run from
     2 % to 12 % mean coverage. GIMP composites every dab, so a stroke at the
     pack's own spacing builds up to solid. Umber took a `max` of coverage
     across the whole stroke, which caps a stroke at the mask's own brightest
     texel however long it is, so `Organic/Organic_000` — peak texel 125 of 255
     — could never paint stronger than **0.49** where its author's stroke
     reaches solid.

     `Brush::build_up` now selects a second coverage blend,
     `a = cov + a(1 - cov)`, which is per-dab compositing exactly. The
     measurement is reproducible rather than remembered:

     ```sh
     cargo run -p umber-core --example measure-stamp -- <files>.gbr
     ```

     | Stamp | Size | Mean | Peak texel | Stroke under `max` | Stroke building up |
     |---|---|---|---|---|---|
     | `Organic/Organic_000` | 512×512 | 0.053 | 0.490 | **0.490** | **0.907** |
     | `Organic/Organic_001` | 512×512 | 0.042 | 0.514 | 0.514 | 0.900 |
     | `Dots/Dots_000` | 232×232 | 0.023 | 0.808 | 0.808 | 0.920 |
     | `Aqua/Aqua_001` | 394×487 | 0.122 | 0.745 | 0.745 | 0.996 |
     | `Shapes/Shapes_001` | 348×348 | 0.085 | 0.839 | 0.839 | 0.981 |
     | `Opaque/Opaque_000` | 110×113 | 0.357 | 1.000 | 1.000 | 1.000 |
     | `Spot/Spot_000` | 36×36 | 0.380 | 1.000 | 1.000 | 1.000 |
     | `Fuzzy/Fuzzy_000` | 256×246 | 1.000 | 1.000 | 1.000 | 1.000 |

     "Stroke" is the peak coverage a straight line of stamps at the file's own
     spacing reaches, measured by `umber_core::tip::stroke_coverage`. Nine of
     the twenty-four sampled — two from every folder — need build-up; the
     others are dense enough that the two rules agree and they stay on the
     cheaper `max` path, where a stroke crossing itself also stays even. **No
     stamp in the sample is too faint to accumulate.** `gbr::to_brush` runs the
     measurement per file and sets `build_up` from it, so this is a decision the
     importer makes rather than one a person has to remember.

     Note that the peak under `max` is always exactly the mask's brightest
     texel. That is not a coincidence and it is the whole argument: a `max`
     blend cannot produce a value no dab contained.
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

  Reasons 2 and 3 are ordinary work — curate a folder, name the brushes by
  hand, pin the commit in `tools/fetch-brushes.ps1` — and they are what stands
  between this pack and the library now. Neither the engine nor the library is
  the obstacle any more: `assets/tips/` carries fifteen third-party masks and
  nineteen brushes stamp them, and this pack's licence is *verifiable from the
  download*, which is the test rubberduck's fails. It is the strongest
  remaining candidate.

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

- **Skipped**, and now on the licence rule rather than for want of a feature.
  The dab pass has a grain channel; what these do not have is a licence
  statement *inside the download*. All three state CC0 on a web page beside the
  file, which is exactly the case the rule at the top of this document covers.
- Umber ships three papers of its own instead, drawn by
  `crates/umber-core/examples/build-bitmaps.rs` and recorded in
  `assets/patterns/LICENSES.md`. Two things fall out of generating them that a
  photograph would not have given: they **tile by construction** — the noise
  lattice wraps, and a seam would draw a grid across every textured mark, since
  the grain is anchored to the document — and three of them are 200 kB rather
  than megabytes.
- A photographic set is still worth having, and the way in is somebody
  satisfying themselves about a particular download and dropping the files into
  `assets/patterns/`. The project will not claim a licence it cannot check.

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

**A pack of bitmap tips has a fourth decision**, and it is the only one that is
not mechanical:

4. Set `ship_tips` on the pack's row. `true` means its masks are written to
   `crates/umber-core/assets/tips/` as 8-bit greyscale PNGs, deduplicated by
   content, and named in the generated `tip_table.rs` — a preset's `tip` field
   resolves against that table first and the user's library second, so a shipped
   stamp and a user's own are the same mechanism. Nothing else has to be done by
   hand; the generator owns that half of the directory, stale masks included.

   Say `true` only if the pack's licence was verified **inside the download**.
   Shipping a mask is redistributing artwork rather than describing it, and a
   pack may pass the fetch script's check on evidence that is good enough to
   convert and not good enough to redistribute — which is exactly rubberduck's
   position above.

   Whether a tip reproduces faithfully is **measured**, by
   `umber_core::tip::stroke_coverage`, and it is a question with two answers
   rather than one: a dense stamp ships on the `max` path and a sparse one ships
   with `build_up` set. Every reader that produces a mask runs the measurement —
   `.gbr`, `.abr` and, since the stamps started shipping, `.kpp`. Only a mask
   too faint for eight-bit coverage to accumulate — `StrokeCoverage::is_usable`
   — is refused, and nothing in the packs above comes close to that.
   `a_shipped_stamp_paints_at_the_strength_it_was_drawn_at` re-derives the flag
   for every shipped stamp on every `cargo test`.
