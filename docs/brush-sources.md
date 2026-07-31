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
- **Skipped for now.** Two reasons, either sufficient: the bundles are `.kpp`
  presets built around bitmap tips, which Umber's round-only dab pass cannot
  render; and they run to roughly 100 MB, which is not something to put through
  a repository's history for a format we cannot read.

### Raghukamath — Krita brush presets v2

- Source: <https://gitlab.com/raghukamath/krita-brush-presets>
- **Skipped for now.** `.bundle` / `.kpp`, same bitmap-tip problem.

### OpenGameArt — 60 free GIMP/Krita brushes (rubberduck)

- Page: <https://opengameart.org/content/60-free-gimp-krita-brushes>
- **Skipped for now.** `.gbr` / `.gih`, which are bitmap tips by definition.
  This is the pack to fetch first once the tip work lands: it is small, the
  format is simple, and OpenGameArt states the licence per submission.

### GDQuest Krita brushes

- CC-BY-4.0, so allowed only with a credits entry. `BrushPreset::credit` exists
  precisely to carry that. **Skipped for now** on the same bitmap-tip grounds,
  not on licensing ones.

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
