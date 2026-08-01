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
- **Not shipped anyway.** Three reasons, and the first is the one that decides
  it:

  1. **Umber cannot paint them the way GIMP does.** These are photographic
     texture stamps, sparse and faint — the ones sampled run from 2 % to 5 %
     mean coverage. GIMP composites every dab, so a stroke at the pack's own
     spacing builds up to solid. Umber takes a `max` of coverage across the
     whole stroke and applies opacity once at commit (the wet-layer design in
     `CLAUDE.md`), so it cannot build up at all. Stamping `Organic/Organic_000`
     along a line at the file's spacing reaches **1.00** coverage compositing
     and **0.48** taking a max — a stroke half as strong as the author's. That
     is the same standard the generator already holds MyPaint brushes to:
     nothing shipped under an author's name should paint unlike their brush.
     Note the standard for a brush the *user* imports is deliberately the
     opposite — an approximation of a brush you chose beats a refusal — and
     these import perfectly well.
  2. **It is raw material, and says so.** The README calls it "a 'raw' resource
     (not adjusted or proofed) meant to help people creating their own custom
     brushes". Every brush has an empty name field and a spacing of 0, so a
     shipped set would be 12 of 1022 files called "Aqua 000" and "Spot 004".
  3. **158 MB and no tags.** The fetch script pins MyPaint at `v2.0.2`; this
     repository has no releases, so a reproducible fetch would have to pin the
     commit (`16b7899`, 2024-08-27) and still download the whole thing to keep
     a dozen files.

  Reasons 2 and 3 are answerable — curate the dense folders (`Opaque` and `Spot`
  average 91 and 97 mean coverage and would reproduce faithfully), name them by
  folder, pin the commit. Reason 1 is answerable only by giving the dab pass a
  build-up mode, which is a change to the wet-layer design and not a brush job.

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

**A pack of bitmap tips needs a fourth step that does not exist yet.** The
shipped library is a single embedded RON, so there is nowhere in it for a
bitmap: `BrushPreset::tip` resolves against the *user's* library only. Shipping
stamps means the generator also writing the masks to
`crates/umber-core/assets/tips/` and an `include_bytes!` table beside them, and
deciding — measurably, not by eye — which of a pack's tips reproduce faithfully
under a `max`-coverage stroke. See the Gimp Brushcollection entry above for why
that measurement is the part that matters.
