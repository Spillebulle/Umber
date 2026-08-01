# Brushes

How Umber's brush library is put together, what an imported brush keeps, and
what it loses.

## The library, in three parts

| Part | Where | Written by |
|---|---|---|
| Umber's own five presets | `umber_defaults` in `crates/umber-core/src/preset.rs` | by hand |
| The imported set | `crates/umber-core/assets/builtin-brushes.ron`, embedded with `include_str!` | the generator, below |
| The user's own | `%APPDATA%\Umber\data\brushes\` (Windows), `~/.local/share/umber/brushes/` (Linux), `~/Library/Application Support/Umber/brushes/` (macOS) | the app, at runtime |

The first two together are `umber_core::preset::builtin()`, a `&'static
[BrushPreset]` parsed once. The third is `umber_core::preset::UserLibrary`.

They are kept apart on purpose. The shipped library is replaced wholesale by an
update; anything the user saved into it would be lost. The user library is never
written by the build.

## The user library is a directory

```text
brushes/
    brushes.ron     the presets — exactly the format it always was
    tips/
        <name>.png  one 8-bit greyscale coverage mask per bitmap tip
```

It used to be the `brushes.ron` alone. A bitmap tip does not go in a text file,
so a stamp brush could be loaded and painted with but never *saved*, which is
what blocked every stamp-based pack.

A directory rather than a zip. Three reasons, heaviest first:

- **A write touches only what changed.** The library is rewritten on every
  edit — save, rename, delete, import — so a rename costs a few kilobytes of
  RON. In a zip it would cost the whole archive, tips and all.
- **The atomic write keeps working.** `UserLibrary::write` writes the RON beside
  itself and renames over the old one, so an interrupted write cannot leave a
  truncated collection. The same trick on a zip would mean rewriting every mask
  to change a name, and risking the masks along with the index.
- **The tips are ordinary pictures.** A stamp's whole content is an image;
  opening, replacing or copying one with anything that opens an image is worth
  more than tidiness.

`BrushPreset::tip` holds the mask's *name*, not the mask. That is what lets two
brushes cut from one stamp share a file and a single GPU upload — and what makes
a name resolving to nothing a brush that paints round rather than a library that
will not load. An Umber `.ron` exported and re-imported elsewhere therefore
arrives round: the text carries the reference, not the picture, and
`UserLibrary::import_file` drops a reference it cannot resolve rather than
leaving one dangling.

### Migrating

A `brushes.ron` sitting where the directory now goes — every library written
before this — is read on first load and written into the new layout. **The
original is left where it is.** A migration that deletes the only copy of
somebody's collection has to be right first time, and this one does not have to
be; an older Umber on the same machine also keeps working, at the price of the
two copies diverging from then on. The library says so once, in the Brushes
panel, and never again.

`a_flat_library_is_migrated_into_the_directory` and
`a_directory_that_already_has_brushes_ignores_the_old_file` are the guards.

## Refreshing the shipped library

```sh
pwsh tools/fetch-brushes.ps1     # or: sh tools/fetch-brushes.sh
cargo run -p umber-core --example build-brush-library
```

The first step downloads the packs into `assets/brushes/`, which is git-ignored,
and records what it took in `assets/brushes/LICENSES.md`, which is not. The
second converts them and rewrites `builtin-brushes.ron`. Both steps are
deliberate acts rather than a `build.rs`: the packs are not in the repository,
so a build script would make a clean checkout unbuildable, and a generated file
that lands in a commit is a file whose diff can be read.

Preview thumbnails are never downloaded. In the MyPaint pack the brush
*settings* are CC0 but some of the previews are CC-BY, and not having the files
is the surest way not to ship them.

## What is in it today

**All 196** MyPaint brushes, plus Umber's own five: 201 presets, every one CC0.

It was 128 for a while. The 68 missing were refused by the generator rather than
lost by accident, and both reasons were engine gaps rather than import gaps:

| Was refused | Count | Now |
|---|---|---|
| `smudge >= 0.5` | 67 | The stroke carries a colour per dab and samples the canvas asynchronously. See the colour-pickup section of `CLAUDE.md`. |
| time-based dabs only | 2 | The dab loop has a time term, so an airbrush keeps depositing while the pen is held still. |

(One was refused on both counts, hence 68 rather than 69.)

`umber_core::brushimport::mypaint::unsupported_features` is still the check, and
it still lists `colorize` and `lock_alpha`, which nothing in this pack uses. It
is deliberately separate from the importer: a user who asks for a specific file
should get whatever the importer can make of it, with a note saying what was
dropped. The generator is the fussy one, because it decides what Umber *claims*
to support.

## How the library is grouped

By **style**, not by pack — `umber_core::style`. A pack arrives sorted by
whoever drew it, which is the right way to credit it and the wrong way to browse
it: nobody reaches for a brush by remembering the artist, and author-grouping
put the pencils in six different collections.

Authorship is not lost. It travels on the brush in `BrushPreset::credit` and the
browser prints it under every name, which is also what satisfies CC-BY should a
pack ever need it.

Twelve collections, in the order the picker lists them:

| Collection | Count |
|---|---|
| Pencils & sketching | 12 |
| Inks & pens | 30 |
| Markers | 6 |
| Charcoal, chalk & pastel | 6 |
| Paint & brushes | 45 |
| Watercolour & wet media | 22 |
| Airbrush & spray | 11 |
| Blenders & smudge | 23 |
| Erasers | 8 |
| Texture & grain | 11 |
| Foliage & fur | 8 |
| Effects & experimental | 19 |

The generator prints this table on every run, because a classifier is a
judgement and judgements have to be looked at rather than asserted. The first
attempt sorted on `smudge >= 0.5` before consulting the name and put 68 brushes
in "Blenders" — MyPaint's oil paints all mix with what is under them, so the
setting says far less than it appears to. Erasing is the only setting that now
overrides a name; see the module docs for the rest of the ordering.

## What conversion keeps

`.myb` is JSON. MyPaint evaluates each setting as

```text
value = base_value + Σ mapping_i(input_i)
```

— the input mappings are **added** to the base value, not multiplied by it.

| MyPaint | Umber |
|---|---|
| `radius_logarithmic` | `size`, `min_size_ratio`, `size_curve` |
| `hardness` | `hardness`, `min_hardness_ratio`, `hardness_curve` |
| `opaque` × `opaque_multiply` | `opacity`, `opacity_curve`, `pressure_opacity` |
| `dabs_per_actual_radius` + `dabs_per_basic_radius` | `spacing` |
| `eraser` | `mode` |
| `slow_tracking` | `stabilization` |
| `elliptical_dab_ratio` | `dab_ratio` |
| `elliptical_dab_angle` | `dab_angle`, `dab_angle_follows_stroke`, `dab_angle_jitter` |
| `offset_by_random` | `scatter`, `min_scatter_ratio`, `scatter_curve` |
| `radius_by_random` | `radius_jitter` |
| `smudge`, `smudge_length`, `smudge_radius_log` | `smudge`, `smudge_length`, `smudge_radius` |
| `dabs_per_second` | `dabs_per_second` |

`radius_logarithmic` is the natural log of the dab radius **in pixels**, and its
pressure mapping is an offset in log space, so the radius at pressure *p* is
`exp(base + map(p))`. The classic mis-import is to read the base value as a
radius: it turns a 2.6 px pen into a 0.96 px one. `classic/pen` has a base of
0.96 and a pressure mapping to +0.5, so Umber stores a size of 8.61 px, a
minimum ratio of `exp(-0.5) = 0.607`, and a size curve that reproduces
MyPaint's radius exactly at all five sample points. There is a test for that:
`the_imported_radius_matches_mypaint_at_every_sample`.

### Three settings are read as a curve, not as a number

`radius_logarithmic`, `hardness` and `offset_by_random` are evaluated at the
five pressures a `ResponseCurve` samples at, rather than read off `base_value`.
That matters because MyPaint states most of what a brush *does* as a mapping on
top of a base of zero: 69 of the 196 brushes vary hardness with pressure, and 38
vary scatter — 16 of those with no constant scatter at all, so reading the base
alone imported a granular brush as a perfectly smooth line.

Umber's form is `peak × (min_ratio + (1 − min_ratio) × curve(p))`. It reproduces
MyPaint's value exactly at those five points for a monotonic mapping, and
degrades gracefully — same range, same ordering, different shape between the
samples — for one that is not.

MyPaint's editor writes a two-point mapping for *every* input a brush has ever
been shown, and most of those are flat. "Has control points" is therefore not
the same question as "is driven by this input": 24 of the 55 brushes that appear
to map `elliptical_dab_ratio` map it to a constant zero. The importer measures
the span of a mapping's output, which is what makes the counts above mean
anything.

## What conversion loses

Documented in full in the module docs of
`crates/umber-core/src/brushimport/mypaint.rs`. The short version, worst first:

- **`elliptical_dab_ratio` driven by an input.** 46 brushes map it and 15 of
  those state a round base, so they arrive round. This is *deliberately* not
  approximated by lifting a constant out of the mapping. The inputs it is
  actually driven by are `random` (16 brushes), `speed1` (14), `stroke` (9),
  `pressure` (8) and `tilt_declination` (7) — and for three of those five the
  input sits at its neutral on a desktop with a mouse, which is precisely the
  case where the base value is what MyPaint itself would render. Substituting
  the mapping's peak would make those brushes wrong in a new way rather than
  right. A ratio that varies genuinely needs a sixth shape parameter, and the
  handful of brushes it would rescue are mostly ones whose range is 0.9 to 1.1.
- **`radius_by_random` driven by an input** — 9 brushes, all with a round base
  and all but one driven by `custom` or `attack_angle`, neither of which exists
  here.
- **`offset_by_speed`.** Scatter that grows with pen speed. 14 brushes. The
  constant part of their scatter is imported; the speed-reactive part is not, so
  a fast flick spreads less than it should.
- **`paint_mode`.** MyPaint 2's spectral pigment mixing — a different colour
  model rather than a brush setting. 19 brushes ask for it.
- **Bitmap tips.** MyPaint has none either — a `.myb` is always a round dab, so
  nothing is lost here. The engine and the library now have them
  (see below); it is the Krita and GIMP packs they exist for.
- **Non-pressure inputs.** `speed1`, `speed2`, `stroke`, `tilt_*`, `custom`,
  `brush_radius`. `Brush` is a `Copy` struct of fixed-size curves, so every
  input it gains costs a curve on every brush. Two are exceptions, both on
  `elliptical_dab_angle`: a `direction` mapping is read as "this dab turns to
  follow the stroke", the difference between a rake and a broad nib, and a
  `random` one is read as `dab_angle_jitter`. Neither is read as a curve.
- **Tilt.** 9 brushes map it, and desktop reports it as `(0, 0)` regardless —
  see the pressure section of `CLAUDE.md`. Supporting the setting without a
  device to drive it would change nothing on any machine Umber currently runs
  on.
- **Opacity build-up.** MyPaint composites each dab, so a low-opacity brush
  darkens as a stroke crosses itself. Umber takes a `max` of coverage and
  applies opacity once at commit — the wet-layer design in `CLAUDE.md` — which
  is why `opaque_linearize` is ignored rather than approximated. A MyPaint wash
  and an Umber wash of the same numbers will not look the same.

  The engine now *has* a build-up mode (see below), and MyPaint brushes
  deliberately do not use it. Umber applies opacity once at commit, so an
  ordinary `.myb` dab arrives with a per-dab coverage of exactly 1.0 — and
  building up from 1.0 is the same as taking a max of it. Turning it on would
  change nothing for most of the pack and would deepen the rest into something
  MyPaint does not draw either, since MyPaint's build-up is on *opacity* and
  Umber's opacity is not in the dab.

## Bitmap tips

The dab pass can stamp an 8-bit coverage mask instead of its procedural round
falloff. This is what the stamp-based packs need.

`umber_core::tip::TipMask` is the mask — plain bytes, `0` no paint and `255`
full, so the engine keeps no GPU types. `CanvasRenderer::set_tip` uploads one to
an `R8Unorm` texture and flips a flag in the dab uniforms.

Four things about the design are load-bearing:

- **The tip is bound per pass, not per dab.** A stroke has one brush, so one
  tip covers the whole dab pass and a thousand tipped dabs are still a single
  draw call. Set it between strokes; changing it mid-stroke would restamp what
  is already in the scratch under a new shape.
- **The tip modulates coverage; it does not composite.** The blend state is
  untouched, so a tipped stroke saturates at 1.0 under overlap exactly as a
  round one does — *unless the brush asked to build up*, which is a separate
  choice of blend state and not something the tip does.
  `a_tipped_stamp_still_saturates_under_overlap` is the guard, and it is a
  deliberate copy of `overlapping_dabs_do_not_compound` rather than an extension
  of it.
- **The dab knows the tip's proportions.** `tip_scale` is a per-pass uniform —
  the mask's dimensions with the longer normalised to 1 — and the vertex shader
  scales the quad's axes by it, so a 512×256 stamp occupies a 2:1 box and keeps
  its shape. It is `(1, 1)` with no tip, which is the exact identity. See
  "Non-square tips" below.
- **The round path is untouched.** The shader samples the tip unconditionally —
  `textureSample` may not sit in non-uniform control flow — and then `select`s
  between it and the falloff. With no tip the binding is a 1×1 placeholder whose
  contents are discarded. Every pre-existing GPU test still passes unchanged,
  which is the evidence.

A fourth thing, about the *stroke* rather than the pass: `set_tip` is called
from `UmberApp::start_stroke` and nowhere else, and it early-outs when the mask
is the same `Arc` as last time. Identity rather than equality — masks are shared
out of the library, so two brushes cut from one stamp really are one allocation,
and comparing a megabyte of coverage to answer "same brush?" would put back the
cost the check exists to avoid. Without it a texture allocation and a copy land
on the first frame of every stroke, which is the one moment this project exists
to keep short. `a_second_brush_with_a_different_tip_replaces_the_first` guards
the failure direction, which is a *stale* tip.

## Importing a GIMP brush

`umber_core::brushimport::gbr::from_gbr` decodes a `.gbr` into a `TipMask`, plus
the brush name and the spacing the format carries. Everything in it is
big-endian; a little-endian read reports a billion-pixel brush and is caught by
the length check rather than producing garbage.

`read_file` accepts `.gbr` alongside `.myb` and `.ron`, so **Import brushes…**
in the Brushes panel reads one, saves the mask into `tips/`, and selects the
brush — importing a brush is asking to paint with it, and a stamp is
unrecognisable in a list and obvious the moment it makes a mark.

`gbr::to_brush` decides the rest of the `Brush`, which the format does not carry:

| From the file | Becomes | Why |
|---|---|---|
| mask width and height | `size`, the mask's longer side | a stamp lands at its original scale until you say otherwise |
| spacing, per cent | `spacing` | GIMP's default of 10 % is Umber's default too |
| spacing of **0** | the default, not 1 % | GIMP's own control cannot go below 1, so a zero is a writer that never filled the field in; taken literally it turns a 500 px stamp into five-pixel steps |
| — | `pressure_size` and `pressure_opacity` **off** | a `.gbr` carries no dynamics, and GIMP stamps one at a constant size |

The mask is handed over exactly as the file holds it — see "Non-square tips".

`build_up` is the fourth thing decided here, and the one that decides whether
the stamp paints like its author's. It is **measured** by
`umber_core::tip::stroke_coverage` rather than guessed: a stamp whose `max`
stroke is as strong as a compositing one stays on the `max` path, and a sparse
photographic texture gets `build_up: true`. See `docs/brush-sources.md`.

A 4-byte `.gbr` is a coloured stamp and imports as its silhouette — the stroke
scratch is one coverage channel by design. `brushimport::dropped_features` says
so, the same way a `.myb` that leans on `colorize` does.

In the brush editor's **Tip** tab a stamp brush shows the mask, its size in
pixels, and a way back to the round dab. **Hardness is drawn dead there**, with
the reason underneath: a tip *replaces* the procedural falloff rather than being
multiplied into it — `select(round, masked, use_tip)` in `dab.wgsl` — so
hardness has nothing left to shape. A round brush shows none of this. Almost
every brush is round, and a permanent row saying so would be a control that
never does anything.

## Build-up

Coverage takes a `max` so that overlapping dabs within one stroke cannot
compound. That is right for a solid disc, where overlap is an artefact of how a
line is drawn. It is wrong for a sparse texture stamp, where overlap **is** the
mark: GIMP and Krita composite every dab, so a stamp whose brightest texel is
0.49 builds to solid along a stroke, and under a `max` it can never exceed 0.49
however long the stroke is.

`Brush::build_up` switches the coverage attachment's blend state to
`a = cov + a(1 − cov)`, which is one dab compositing over the last. Five things
about it:

- **It is a blend-state change and nothing else.** The dab shader is byte for
  byte what it was, so the two paths cannot drift into stamping different
  shapes. That is the lesson the paint-versus-erase note in `CLAUDE.md` records.
- **Nothing downstream sees it.** The scratch still holds coverage in 0..1, so
  `composite.wgsl` and `commit.wgsl` are untouched and `Brush::opacity` is still
  applied exactly once, at commit.
  `a_building_stroke_still_applies_its_opacity_exactly_once` pins that.
- **The `max` path is untouched.** It is the default, every pre-existing GPU
  test passes unchanged, and `build_up_leaves_the_max_path_alone` says so
  directly.
- **It combines with smudging** rather than being refused with it. There are
  four dab pipelines now, from two independent binary choices, built by one loop
  over one descriptor. Under build-up the colour attachment's premultiplied
  `over` accumulates its alpha by the very formula the coverage attachment uses,
  so the two agree exactly and the un-premultiply at commit divides by the
  coverage that is really there.
  `a_building_smudge_keeps_its_colour_and_its_accumulation` is the guard.
- **It asymptotes one level short of solid.** The scratch is `R8Unorm`, so once
  coverage reaches 254/255 a further half-coverage dab contributes `0.5/255` and
  rounds to nothing. A dab of *full* coverage does reach 255, because
  `a + 1(1 − a)` is exactly 1. Sixteen-bit coverage would close a 0.4 % gap at
  four times the bandwidth of the hottest texture in the frame.

Build-up only means anything where a dab is not solid: a bitmap tip, paper
grain, or a pressure-opacity ramp. For an ordinary brush per-dab coverage is
exactly 1.0 and the two rules agree — which is why nothing in the MyPaint pack
uses it.

## Non-square tips

A non-square mask used to be **padded into a square**, because the dab stretched
whatever it was given over its bounding box. The recorded reason for padding
rather than spending `dab_ratio` on it still stands: the ratio's long axis is
the dab's *x* axis, so a portrait mask would have to be rotated a quarter turn
and rotated back by `dab_angle`, and the ratio is the user's setting rather than
the file's.

It was never a choice between only those two. The dab pass is now told the tip's
proportions — `TipMask::aspect`, the mask's dimensions with the longer
normalised to 1 — and the vertex shader scales the quad's axes by them. That is
**exactly the geometry padding produced**: a mask padded to side `max(w, h)` and
stretched over a square occupies `w/side` by `h/side` of it. Three costs go with
the padding — the margin of empty fragments, the padded texture, and having to
think about the ratio at all — and the quarter-turn problem never arises,
because this scales the dab's own axes rather than borrowing the ratio.

`Brush::size` still describes the long axis, of the stamp now, and `dab_ratio`
still squashes on top of it. `a_non_square_tip_keeps_its_proportions` guards it.

One consequence worth stating: a **tipped dab has an angle whatever its
roundness**, because a bitmap is not rotationally symmetric. `dab_angle`,
`dab_angle_follows_stroke` and `dab_angle_jitter` are all live for a stamp
brush, and the editor enables them from `Editor::tip` rather than from
`Brush::dab_has_angle`, which cannot know.

A second consequence, in the damaged rect: a tip paints into its quad's
**corners**, and a quad turned 45° reaches out to `radius × √2`.
`StrokeBuilder::bounds` unioned the circumscribing circle, which is enough for a
round dab at any angle and was not enough for a rotated stamp — coverage left
outside the committed rectangle redraws as a hanging preview and is then baked
in by the next stroke, in that stroke's colour. It now unions the axis-aligned
box of the rotated quad: exact for a tip, conservative for a round dab, and
*tighter* than the circle for an unrotated ellipse.
`a_rotated_stamp_is_committed_all_the_way_into_its_corners` guards it.

## Paper grain

An optional tiling texture multiplied into dab coverage:
`coverage × mix(1.0, tile, strength)`. This is what makes a pencil catch on the
tooth of the paper, and it is the design's **Texture** section.

- **Zero strength is the exact identity.** `mix(1.0, tile, 0.0)` is 1.0 whatever
  the tile holds, so a brush that asks for no paper pays one multiply by one and
  no branch. `grain_off_is_the_exact_identity` binds a *black* tile at strength
  zero to prove the multiply really is by one.
- **The grain is anchored to the document, not to the dab.** That is the whole
  effect: a second stroke lands in the same pits as the first, and a brush
  dragged across the sheet catches and skips. The fragment shader interpolates
  the document position for it, and
  `grain_is_anchored_to_the_paper_and_not_to_the_dab` is the guard.
- **`Brush::grain_scale` is in document pixels.** Paper does not get coarser
  when you pick up a bigger pencil.
- **Its own sampler**, which repeats where the tip's clamps. A paper tile covers
  the whole document and must wrap; a tip stretched over its dab must not.
- **It does not touch the blend state.** A grained stroke saturates under
  overlap exactly as a plain one does, and builds up if — and only if — the
  brush asked to.

Three papers ship, generated rather than photographed.
`assets/patterns/LICENSES.md` records why, and
`crates/umber-core/examples/build-bitmaps.rs` is the source. `GrainPattern` is a
closed enum rather than a name because `Brush` is `Copy` — the same constraint
that makes `ResponseCurve` a fixed array of samples.

## Tips in the shipped library

`BrushPreset::tip` used to resolve against the *user's* library only, so nothing
shipped could carry a stamp. It now falls through to `tip::builtin`, which
decodes an `include_bytes!` table generated from the files in
`crates/umber-core/assets/tips/`. Both sources hand back an `Arc<TipMask>` that
is stable for the life of the process, which is what `CanvasRenderer::set_tip`'s
identity check needs.

One shipped brush uses it: **Stipple chalk**, a sparse speckle Umber drew.
Sparse on purpose — a dense silhouette would paint identically under either
coverage rule and would demonstrate nothing. Its brightest texel is 0.44, so it
ships with `build_up` set, and
`a_shipped_stamp_paints_at_the_strength_it_was_drawn_at` checks that flag
against the measurement rather than against anybody's memory.

Adding a stamp is dropping an 8-bit greyscale PNG into that directory and
re-running `cargo run -p umber-core --example build-bitmaps`, which rewrites the
table from the listing. The table *is* the listing, so a file that is not there
is not in the binary and one that is cannot be forgotten.

## Not done yet

- **A third-party `.gbr` pack to ship.** The machinery is complete — a stamp
  brush can be imported, saved, reloaded, painted with, embedded and shipped —
  and the build-up problem that blocked the one licence-clearing CC0 pack is
  solved. What remains for that pack is curation rather than engine work; see
  `docs/brush-sources.md`. The `.gbr` decoder is still tested against files
  built byte by byte in the test module rather than against a real brush.
- **A paper texture of your own.** Three ship; `GrainPattern` is a closed enum,
  and reading a fourth off disk needs a variant that names a file.
- **Ellipticity driven by an input**, and scatter driven by pen speed — see
  "What conversion loses" above for why each was left alone rather than
  approximated.
- **`lock_alpha`, `colorize` and `change_color_*`.** No brush in the shipped
  pack sets any of them to a live value, so nothing in the library is waiting on
  them. They are worth building as painting features in their own right —
  `lock_alpha` especially — not as import fidelity.
- **Per-brush blend modes.**
- **A `.kpp` importer.** Krita presets are PNG files with the settings in a
  text chunk, and most of them lean on a bitmap tip. The tip half is now here;
  the settings half is not.
- **`.gih` animated brushes**, a `.gbr` sequence. One tip is bound per stroke,
  so there is nowhere to put the other frames.

## What a user can set

The brush editor has five sections, following the design's naming where it has
a name for the thing:

| Section | Holds |
|---|---|
| Tip | size, hardness, opacity, spacing, airbrush rate, roundness, angle, angle-follows-stroke, stabilisation |
| Dynamics | pressure source, and pressure → size / opacity / hardness with their curves and floors |
| Scatter | scatter, size jitter, angle jitter, pressure → scatter |
| Texture | build-up, paper strength, which paper, tile size |
| Blending | colour pickup, smear length, pickup radius |

That is every field of `Brush` except `mode`, which is the tool choice (Brush
or Eraser) rather than a brush setting, and is on the tool rail.

Build-up and the paper share a section deliberately. Both are about a mark made
of many faint stamps rather than one solid one: grain is what makes it faint,
and build-up is what lets a second pass make it darker. A textured brush without
build-up paints one pass and then stops responding, which is the surprise the
pairing avoids.

One of the design's six sections is still not drawn at all rather than drawn
empty: **Wet edges** has no engine behind it. **Stabiliser** is one slider and
rides on Tip. Colour pickup needed a home and none of the design's names is one,
so **Blending** is a name of our own — filing it under "Wet edges" would have
borrowed a term that means something else in every application that has it.

Roundness is shown rather than `dab_ratio`, because that is the design's word
and every other paint application's; the two are reciprocals. Angle and angle
jitter are disabled on a round dab, with a line saying why, since a circle has
no angle.

See `docs/brush-sources.md` for the packs that were considered and why the ones
that are missing are missing.
