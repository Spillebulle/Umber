# Brush sources

Every pack considered for Umber's library, whether it is used, and — where it
is not — the specific reason. The rule this list is written against:

> If a source's licence cannot be verified **from its own files**, it does not
> ship. A licence stated on a web page next to a download is not the same thing
> as a licence inside the download.

`tools/fetch-brushes.ps1` enforces that mechanically: a pack is only kept if the
declared licence file is present in the archive and states what it is supposed
to state. See `docs/brushes.md` for what happens to a pack once it is fetched.

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
- Result: 128 of the 196 converted; the rest need engine features Umber does
  not have (see `docs/brushes.md`).

This one pack happens to include David Revoy's MyPaint brushes and Ramón
Miranda's, both of which are also distributed separately — with a verified CC0
statement here and without one there. Taking them from this archive is strictly
better.

## Not shipping, and why

### David Revoy — MyPaint brush kit v6

- Page: <https://www.davidrevoy.com/article142/ressource-mypaint-brushes>
- **Skipped.** The download is a DeviantArt link (`fav.me/d5jhhbb`) that needs a
  browser session, so it cannot be fetched by a script, and the archive's own
  licence statement therefore cannot be checked before use. The article text
  says public domain while the site footer says CC-BY-4.0 — exactly the
  ambiguity the rule above exists for.
- Not a real loss: 36 of Revoy's brushes are in the MyPaint pack above under a
  verified CC0 stanza.

### David Revoy — Krita brush bundles (2023-01 … 2025-01)

- Page: <https://www.davidrevoy.com/article1060/krita-brushes-2025-01-bundle>
- **Skipped for now.** The bundles are `.kpp` presets, and Umber has no `.kpp`
  reader — the bitmap tips inside them are no longer the obstacle, but the
  settings format still is. They also run to roughly 100 MB.

### Raghukamath — Krita brush presets v2

- Source: <https://gitlab.com/raghukamath/krita-brush-presets>
- **Skipped for now.** `.bundle` / `.kpp`, same missing reader.

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
     brushes". Every brush has an empty name field and a spacing of 0, so a
     shipped set would be 12 of 1022 files called "Aqua 000" and "Spot 004".
  3. **158 MB and no tags.** The fetch script pins MyPaint at `v2.0.2`; this
     repository has no releases, so a reproducible fetch would have to pin the
     commit (`16b7899`, 2024-08-27) and still download the whole thing to keep
     a dozen files.

  Reasons 2 and 3 are ordinary work — curate a folder, name the brushes by
  hand, pin the commit in `tools/fetch-brushes.ps1` — and they are what stands
  between this pack and the library now. The engine is no longer the obstacle:
  the fourth step below is built, and `assets/tips/` ships Umber's own stamp
  through it as proof.

### OpenGameArt — 60 free GIMP/Krita brushes (rubberduck)

- Page: <https://opengameart.org/content/60-free-gimp-krita-brushes>
- **Skipped.** `.gbr` is now readable — `umber_core::brushimport::gbr` decodes
  it and the dab pass can stamp it — so the format is no longer the obstacle.
  The licence is: OpenGameArt states it on the submission page, not inside the
  archive, so a fetch script cannot verify it from the download, which is
  exactly the case the rule at the top of this file covers. It is also behind a
  form rather than a stable archive URL. Somebody who reads the page and
  satisfies themselves can drop the files in by hand; the project will not
  claim a licence it cannot check.
- `.gih` — an animated `.gbr` sequence — is not read at all. Umber stamps one
  tip per stroke, so there would be nowhere to put the other frames.

### GDQuest Krita brushes

- CC-BY-4.0, so allowed only with a credits entry. `BrushPreset::credit` exists
  precisely to carry that. **Skipped for now** because they are `.kpp`, not on
  licensing grounds.

### CC0 paper and grain textures (ambientCG, Poly Haven, Texture Ninja)

- **Skipped for now.** Umber has no grain channel in the dab pass yet, so there
  is nothing to point them at.

## Adding a pack

1. Add a row to the `$Packs` table in `tools/fetch-brushes.ps1` and the `PACKS`
   line in `tools/fetch-brushes.sh`, including the licence file and the strings
   that must appear in it.
2. Add a row to `PACKS` in `crates/umber-core/examples/build-brush-library.rs`
   with the per-directory authorship, so every preset carries a `Credit`.
3. Run both steps from `docs/brushes.md` and commit the regenerated
   `builtin-brushes.ron` alongside the updated `assets/brushes/LICENSES.md`.

The test `every_shipped_preset_is_usable_and_attributed` fails if any shipped
preset has no credit, which is the backstop for step 2.

**A pack of bitmap tips has a fourth step.** `BrushPreset::tip` used to resolve
against the *user's* library only, so there was nowhere in the shipped set to
put a bitmap. There now is:

4. Put the masks in `crates/umber-core/assets/tips/` as 8-bit greyscale PNGs and
   name them in `TIPS` in `crates/umber-core/src/tip.rs`, which is an
   `include_bytes!` table beside `builtin-brushes.ron`. A preset's `tip` field
   resolves against the shipped table first and the user's library second, so a
   shipped stamp and a user's own are the same mechanism.

   Whether a tip reproduces faithfully is **measured**, by
   `umber_core::tip::stroke_coverage`, and it is now a question with two
   answers rather than one: a dense stamp ships on the `max` path and a sparse
   one ships with `build_up` set. Only a mask too faint for eight-bit coverage
   to accumulate — `StrokeCoverage::is_usable` — is refused, and nothing
   sampled from the packs above comes close to that.
