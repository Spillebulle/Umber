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
cargo run -p umber-core --example survey-mypaint   # the tables below
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
| `radius_logarithmic` | `size`, `min_size_ratio`, `size_curve`, `Size` modulations |
| `hardness` | `hardness`, `min_hardness_ratio`, `hardness_curve`, `Hardness` modulations |
| `opaque` × `opaque_multiply` | `opacity`, `opacity_curve`, `pressure_opacity`, `Opacity` modulations |
| `dabs_per_actual_radius` + `dabs_per_basic_radius` | `spacing` |
| `eraser` | `mode` |
| `slow_tracking` | `stabilization` |
| `elliptical_dab_ratio` | `dab_ratio`, `Ratio` modulations |
| `elliptical_dab_angle` | `dab_angle`, `dab_angle_follows_stroke`, `dab_angle_jitter`, `Angle` modulations |
| `offset_by_random` | `scatter`, `min_scatter_ratio`, `scatter_curve`, `Scatter` modulations |
| `offset_by_speed` | `speed_offset` |
| `radius_by_random` | `radius_jitter` |
| `smudge`, `smudge_length`, `smudge_radius_log` | `smudge`, `smudge_length`, `smudge_radius`, `Smudge` modulations |
| `dabs_per_second` | `dabs_per_second` |
| `stroke_duration_logarithmic`, `stroke_holdtime` | `stroke_span`, `stroke_hold` |
| `change_color_h` | `Hue` modulations |
| `change_color_v` + `change_color_l` | `Value` modulations |
| `change_color_hsv_s` + `change_color_hsl_s` | `Saturation` modulations |

`radius_logarithmic` is the natural log of the dab radius **in pixels**, and its
pressure mapping is an offset in log space, so the radius at pressure *p* is
`exp(base + map(p))`. The classic mis-import is to read the base value as a
radius: it turns a 2.6 px pen into a 0.96 px one. `classic/pen` has a base of
0.96, a pressure mapping to +0.5 and a speed mapping to −0.15, so Umber stores a
size of 7.99 px — the radius at full pressure and typical speed, doubled — a
minimum ratio of `exp(-0.5) = 0.607`, a size curve, and a `Size ← Speed`
modulation. Rebuilding the radius from those four reproduces MyPaint's own value
at every sampled combination of the two inputs, which is what
`the_imported_radius_matches_mypaint_at_every_sample` asserts.

### Every setting is evaluated, not read

MyPaint's `value = base_value + Σ mapping_i(input_i)` is evaluated in full, with
each input held at MyPaint's own idea of a typical value. That is one function,
`MybFile::eval`, and everything goes through it. Three faults disappeared when
it did:

- **`opaque`'s own mappings were dropped.** Three brushes — `deevad/oil_mop`,
  `basic_digital_brush`, `basic_digital_knife` — shipped at `opacity: 0.0`,
  completely invisible, because they state a base of about `2.5e-05` and put the
  whole of their opacity on a pressure mapping. `classic/long_grass` is the
  milder shape of the same fault and shipped at 0.748 instead of 1.0.
- **`brush_radius` mappings were dropped**, and it is a *constant*:
  libmypaint reads it as `BASEVAL(RADIUS_LOGARITHMIC)`. 13 brushes map it onto
  the radius, 10 onto hardness and 5 onto spacing, and every one of them
  imported at the wrong value for want of an evaluation.
- **Mappings on inputs Umber has no equivalent for were dropped** rather than
  evaluated at that input's resting value. A brush whose whole tilt mapping sits
  at −0.3 should arrive 0.3 narrower, because that is what MyPaint draws on a
  machine with no tilt.

The exact arithmetic matters in two more places, and both were checked against
libmypaint's source rather than reasoned about:

- **Only the product is clamped.** `opaque = MAX(0, opaque); CLAMP(opaque ×
  opaque_multiply, 0, 1)`. Neither half is first clamped to the range its editor
  shows, so a brush whose `opaque` reaches 1.5 really does reach full coverage
  at two thirds of its multiplier.
- **A mapping extrapolates its end segments.** `mypaint_mapping_calculate` picks
  the segment `x` falls in, falling back to the first or last, and carries its
  slope onwards; it does not hold the end value. `classic/pen` states its speed
  mapping over `0..1` while the speed input runs to 4, so holding thins the nib
  by 14% on a flick where MyPaint thins it by 45%. Extrapolation is bounded
  because the input's *domain* is clamped.

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
the span of a mapping's output, which is what makes the counts below mean
anything.

## Everything else the brushes ask for

`cargo run -p umber-core --example survey-mypaint` prints this and the tables
below it, and says so when a brush loses something. It is kept rather than
thrown away for the same reason the library generator prints its classification:
a count nobody can re-derive quietly goes stale, and two of this file's own
figures were wrong before it existed.

Every `(setting, input)` pair the pack uses at least three times, ranked by how
many brushes have a **non-flat** mapping for it. "Read" means the effect reaches
the canvas.

| Setting | Input | Brushes | Output span (min / median / max) | Fate |
|---|---|---:|---|---|
| `opaque_multiply` | pressure | 194 | 0.11 / 1.00 / 1.67 | read |
| `radius_logarithmic` | pressure | 113 | 0.15 / 1.25 / 5.17 | read |
| `hardness` | pressure | 69 | 0.03 / 0.30 / 1.70 | read |
| `opaque` | pressure | 54 | 0.04 / 1.00 / 2.00 | read |
| `smudge` | pressure | 42 | 0.17 / 1.00 / 2.00 | read |
| `elliptical_dab_angle` | direction | 39 | 42.8 / 180 / 360 | read, as "follows the stroke" |
| `offset_by_random` | pressure | 38 | 0.30 / 1.40 / 8.57 | read |
| `elliptical_dab_angle` | random | 31 | 1.0 / 360 / 360 | read, as angle jitter |
| `radius_logarithmic` | speed1 | 31 | 0.04 / 0.40 / 2.06 | read |
| `custom_input` | random | 19 | 10.6 / 20 / 20 | dropped |
| `opaque` | speed1 | 19 | 0.05 / 0.14 / 2.00 | read |
| `elliptical_dab_ratio` | random | 16 | 0.40 / 2.00 / 18.0 | read |
| `change_color_l` | stroke | 14 | 0.01 / 0.02 / 0.60 | read, as HSV value |
| `elliptical_dab_ratio` | speed1 | 14 | 0.54 / 2.13 / 14.0 | read |
| `radius_logarithmic` | speed2 | 14 | 0.04 / 0.42 / 3.02 | read |
| `custom_input` | pressure | 13 | 1.0 / 1.0 / 1.0 | dropped |
| `radius_logarithmic` | brush_radius | 13 | 3.51 / 6.86 / 8.00 | folded into the base |
| `hardness` | speed1 | 12 | 0.02 / 0.08 / 0.40 | read |
| `radius_logarithmic` | random | 12 | 0.22 / 1.25 / 3.00 | read |
| `smudge_bucket` | custom | 12 | 127 / 255 / 257 | no equivalent |
| `smudge_length` | stroke | 12 | 0.52 / 2.00 / 2.00 | base read, mapping dropped |
| `radius_logarithmic` | stroke | 11 | 0.62 / 1.82 / 4.58 | read |
| `smudge_length` | pressure | 11 | 0.82 / 0.82 / 1.65 | base read, mapping dropped |
| `anti_aliasing` | brush_radius | 10 | 0.60 / 3.84 / 3.84 | correctly ignored |
| `hardness` | brush_radius | 10 | 0.36 / 0.89 / 0.89 | folded into the base |
| `offset_angle` | custom | 10 | 47.6 / 80 / 80.6 | no equivalent |
| `smudge` | stroke | 10 | 0.05 / 1.00 / 2.00 | read |
| `elliptical_dab_ratio` | stroke | 9 | 0.80 / 0.80 / 9.00 | read |
| `radius_by_random` | custom | 9 | 0.14 / 0.14 / 0.14 | held at neutral |
| `radius_logarithmic` | custom | 9 | 0.90 / 0.90 / 7.32 | held at neutral |
| `change_color_v` | random | 8 | 0.08 / 0.20 / 0.60 | read |
| `elliptical_dab_ratio` | pressure | 8 | 2.39 / 2.70 / 15.0 | read |
| `opaque` | stroke | 8 | 1.0 / 1.0 / 1.0 | read |
| `elliptical_dab_angle` | stroke | 7 | 28.6 / 360 / 360 | read |
| `elliptical_dab_ratio` | tilt_declination | 7 | 4.60 / 9.00 / 11.2 | held at neutral |
| `offset_by_random` | tilt_declination | 6 | 0.63 / 0.83 / 1.80 | held at neutral |
| `radius_logarithmic` | tilt_declination | 6 | 0.19 / 1.60 / 1.60 | held at neutral |
| `change_color_h` | custom, random | 5, 5 | 0.02 / 0.04 / 0.10 | held at neutral; read |
| `change_color_v` | stroke | 5 | 0.20 / 0.46 / 2.88 | read |
| `dabs_per_actual_radius` | brush_radius | 5 | 5.78 / 29.6 / 31.4 | folded into the base |
| `eraser` | pressure | 5 | 0.05 / 0.10 / 0.50 | base read, mapping dropped |
| `offset_by_random` | speed1, speed2 | 5, 5 | 0.10 / 1.71 / 2.15 | read |
| `offset_multiplier` | stroke | 5 | 0.31 / 1.09 / 1.41 | no equivalent |
| `smudge` | speed1 | 5 | 0.02 / 0.15 / 0.20 | read |

Totalled across every setting: `pressure` 576 live mappings, `random` 111,
`speed1` 104, `stroke` 100, `custom` 74, `direction` 46, `brush_radius` 44,
`tilt_declination` 28, `speed2` 27, then a long tail. Six of those are read
directly, `brush_radius` is a constant that is folded in, and the rest are held
at their neutral — which is not the same as being ignored, because
`mapping(neutral)` is still added.

Settings with a non-default **base value** and no mapping worth speaking of, in
brush counts: `dabs_per_actual_radius` 146, `opaque_linearize` 123,
`anti_aliasing` 100, `opaque` 93, `dabs_per_basic_radius` 75, `smudge` 74,
`offset_by_random` 66, `stroke_duration_logarithmic` 56,
`elliptical_dab_ratio` 55, `color_h/s/v` 52 each, `smudge_length` 52,
`slow_tracking` 51, `stroke_holdtime` 47, `dabs_per_second` 45,
`radius_by_random` 23, `slow_tracking_per_dab` 22, `smudge_radius_log` 20,
`paint_mode` 19, `tracking_noise` 17, `stroke_threshold` 14, `eraser` 10,
`offset_by_speed` 10.

### What that produced

64 of the 196 brushes need no modulation at all and stay on the fast path; 52
take one, 46 two, and the busiest — `classic/long_grass` — takes nine of the
table's twelve slots. Nothing in the pack is truncated. By target:
`Size` 68 entries, `Smudge` 56, `Ratio` 50, `Opacity` 36, `Value` 35,
`Hardness` 14, `Scatter` 12, `Hue` 11, `Angle` 8, `Saturation` 5.

107 brushes now need the per-dab colour path, against 74 before. That is a real
cost — a second render target written during the dab pass — and it is what those
brushes ask for: 42 of them state colour pickup entirely as a pressure mapping,
which is an oil brush that mixes when you lean on it, and reading the base alone
made every one of them deposit flat paint for the whole stroke. The scratch
target is allocated per document rather than per stroke, so nothing is
allocated when one of these is picked up.

## What conversion loses

Documented in full in the module docs of
`crates/umber-core/src/brushimport/mypaint.rs`. The short version, worst first:

- **`custom_input` and everything it drives.** MyPaint's `custom` input is a
  low-passed copy of a setting that is itself mapped, so supporting it means a
  second evaluation pass with its own filter. 74 mappings read it — but two
  thirds of those drive `offset_angle`, `smudge_bucket` and the rest of the
  Anti-Art offset machinery, which has no equivalent here at all.
- **The Anti-Art extensions.** `offset_x/y`, `offset_angle*`,
  `offset_multiplier`, `gridmap_*`, `smudge_bucket`, `smudge_transparency`,
  `smudge_length_log` — 19 brushes, and they need a dab that can be thrown to a
  computed place with a colour bucket of its own.
- **Tilt.** 28 mappings, and desktop reports tilt as `(0, 0)` regardless — see
  the pressure section of `CLAUDE.md`. Held at neutral, which is what MyPaint
  renders on the same machine, so nothing is *wrong*; it is simply flat.
- **`paint_mode`.** MyPaint 2's spectral pigment mixing — a different colour
  model rather than a brush setting. 19 brushes ask for it.
- **`smudge_length` driven by an input** — 23 mappings. `smudge_length` decides
  how fast the carried colour decays towards each new canvas sample, and that
  decay happens when a *probe comes home*, not when a dab is emitted. Making it
  per-dab would mean deciding which dab a readback belonged to.
- **`eraser` as a fraction.** MyPaint scales a dab's target alpha by
  `1 - eraser`, so a brush can erase a bit; `Brush::mode` is a switch. Five
  brushes map it onto pressure and import as whichever side of 0.5 their base
  lands.
- **`colorize`, `lock_alpha`, `posterize`, `restore_color`.** All four change
  how a dab *composites* rather than what it is, so they belong to the commit
  shader rather than the importer. No brush in the pack sets any of them to a
  live value.
- **The alpha correction on `radius_by_random`.** MyPaint dims a dab that
  randomness made larger, by the square of the ratio, so a jittered stroke keeps
  its average density. Umber's `max` coverage has no per-dab density to keep.
- **Bitmap tips.** MyPaint has none either — a `.myb` is always a round dab, so
  nothing is lost here. The engine and the library now have them
  (see below); it is the Krita and GIMP packs they exist for.
- **Opacity build-up.** MyPaint composites each dab, so a low-opacity brush
  darkens as a stroke crosses itself. Umber takes a `max` of coverage across the
  whole stroke and applies opacity once at commit — that is the wet-layer design
  in `CLAUDE.md`, and it is why `opaque_linearize` is ignored rather than
  approximated. A MyPaint wash and an Umber wash of the same numbers will not
  look the same.

Three things look like faults and are not:

- **`color_h` / `color_s` / `color_v`** are non-default in 52 brushes, and the
  values are simply whatever colour was on the canvas when the file was saved —
  the same triple repeats verbatim across a dozen unrelated brushes. MyPaint
  only applies them when `restore_color` is set, which two brushes do. Ignoring
  them is correct.
- **`opaque_linearize`** is non-default in 123 brushes. It is MyPaint reducing
  per-dab alpha so that dabs compounding at the brush's spacing reach the
  requested opacity. Umber's `max` coverage reaches exactly `opacity` already.
- **`anti_aliasing`** is non-default in 100 brushes. It is a minimum edge
  fadeout in pixels, and Umber's dab shader applies one unconditionally, sized
  from the dab's short axis. Nothing is lost.

### Approximations, stated

- **`change_color_l` and `change_color_hsl_s` are applied in HSV, not HSL.**
  They are not the same axis — a fully saturated hue is L 0.5 and V 1 — so the
  *amount* of a lightness shift is approximate while its direction and its
  timing are exact. 14 brushes drift lightness along the stroke and this is what
  makes that visible at all.
- **Several mappings on one setting compose by addition, as MyPaint does — but
  opacity composes by multiplication**, because MyPaint reaches opacity by
  multiplying two settings rather than adding to one. Exact for one mapping,
  approximate for two.
- **The `stroke` input's ramp is measured against `Brush::size / 2`**, where
  MyPaint measures against `exp(base radius_logarithmic)`. For a brush that
  doubles under pressure the cycle is about twice as long here.
- **`speed1_gamma`, `speed1_slowness` and their `speed2` twins are constants**,
  at MyPaint's defaults. Every brush in the pack leaves all four there.
- **`offset_by_speed_slowness` is a constant**, at the 10 ms MyPaint's default
  gives. Only ten brushes use the offset at all.
- **A dab's random draw is uniform where MyPaint's `offset_by_random` and
  `radius_by_random` are gaussian.** Those two keep their own gaussian; the
  `random` *input* is uniform in both, and one draw per dab is shared by every
  entry that reads it, exactly as libmypaint's `random_input` is.

## Bitmap tips

The dab pass can stamp an 8-bit coverage mask instead of its procedural round
falloff. This is what the stamp-based packs need.

`umber_core::tip::TipMask` is the mask — plain bytes, `0` no paint and `255`
full, so the engine keeps no GPU types. `CanvasRenderer::set_tip` uploads one to
an `R8Unorm` texture and flips a flag in the dab uniforms.

Three things about the design are load-bearing:

- **The tip is bound per pass, not per dab.** A stroke has one brush, so one
  tip covers the whole dab pass and a thousand tipped dabs are still a single
  draw call. Set it between strokes; changing it mid-stroke would restamp what
  is already in the scratch under a new shape.
- **The tip modulates coverage; it does not composite.** The blend state is
  untouched and still `max`, so a tipped stroke saturates at 1.0 under overlap
  exactly as a round one does and stroke opacity is still applied once, at
  commit. `a_tipped_stamp_still_saturates_under_overlap` is the guard, and it is
  a deliberate copy of `overlapping_dabs_do_not_compound` rather than an
  extension of it.
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
| mask width and height | `size`, after padding to a square | a stamp lands at its original scale until you say otherwise |
| spacing, per cent | `spacing` | GIMP's default of 10 % is Umber's default too |
| spacing of **0** | the default, not 1 % | GIMP's own control cannot go below 1, so a zero is a writer that never filled the field in; taken literally it turns a 500 px stamp into five-pixel steps |
| — | `pressure_size` and `pressure_opacity` **off** | a `.gbr` carries no dynamics, and GIMP stamps one at a constant size |

The mask is padded to a square rather than given a `dab_ratio`. The dab spreads
a tip over its bounding box, so an unpadded portrait stamp comes out squashed;
the ratio's long axis is the dab's *x* axis, so preserving proportions with it
would mean rotating a portrait mask a quarter turn and rotating it back with
`dab_angle`. Padding is exact, needs no rotation, and leaves `dab_ratio` free
for the user to squash a stamp deliberately. It costs shading an empty margin,
which for the near-square masks packs actually contain is a few per cent.

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

## Not done yet

- **A licensed `.gbr` pack to ship.** The machinery is complete — a stamp brush
  can be imported, saved, reloaded and painted with — but nothing ships with
  one. The one CC0 pack whose licence *is* stated inside the download is a raw
  photographic-texture resource whose brushes rely on GIMP's per-dab build-up,
  which Umber's `max` coverage cannot reproduce; see `docs/brush-sources.md` for
  the measurement. The `.gbr` decoder is tested against files built byte by byte
  in the test module rather than against a real brush.
- **Tips in the *shipped* library.** `BrushPreset::tip` resolves against the
  user's library only. Shipping a stamp would additionally need the masks
  embedded in the binary — a generated `assets/tips/` and an `include_bytes!`
  table beside `builtin-brushes.ron` — and a rule in the generator for which of
  a pack's tips are dense enough to reproduce faithfully. Neither is built,
  because there is nothing yet to point them at.
- **A stamp brush's row in the library looks round.** `widgets::brush_row`
  paints its sample from opacity and hardness, which is what a procedural dab
  is made of. The brush editor shows the mask; the list does not.
- **A row's sample ignores the modulation table.** `widgets::brush_sample` is a
  miniature dab loop of its own rather than a `StrokeBuilder`, so a brush whose
  ellipticity is thrown per dab draws its row as though it were not. Fixing it
  properly means the sample driving a real stroke builder — which would also
  give it speed and stroke position, neither of which a static row has.
- **Grain / paper texture** multiplied into dab coverage.
- **Elliptical tips.** The tip is stretched over the dab's bounding square, so a
  non-square mask loses its aspect ratio. The dab carries a single radius and
  has nowhere to record one.
- **`lock_alpha`, `colorize` and `posterize`.** All three change how a dab
  *composites*, so they belong to `commit.wgsl` — and every change there has to
  be made identically in `composite.wgsl` or the stroke jumps at pointer-up.
  `lock_alpha` additionally needs the layer's own alpha read as a mask, which
  the stroke scratch has no channel for. No brush in the shipped pack sets any
  of them to a live value, so nothing in the library is waiting on them; they
  are worth building as painting features in their own right, `lock_alpha`
  especially, rather than as import fidelity.
- **Per-brush blend modes.** `Brush::mode` is `Paint | Erase`. Widening it to
  the layer stack's `BlendMode` would mean the commit pass choosing among eight
  blend states rather than two, the same eight added to the preview half of
  `composite.wgsl`, and a control in the brush editor — perhaps two days, most
  of it in keeping the two shaders in step. Nothing in the shipped library needs
  it, so it is worth doing when a *user* asks rather than to close an import
  gap.
- **`custom_input`**, and with it the last third of MyPaint's own mappings. See
  "What conversion loses".
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
| Inputs | the modulation table — target, input, both ends of its range and its curve — plus the stroke ramp and hold |
| Scatter | scatter, size jitter, angle jitter, speed lead, pressure → scatter |
| Blending | colour pickup, smear length, pickup radius |

That is every field of `Brush` except `mode`, which is the tool choice (Brush
or Eraser) rather than a brush setting, and is on the tool rail.

Two of the design's six sections are not drawn at all rather than drawn empty:
**Texture** has no engine behind it (see above) and **Wet edges** has none
either. **Stabiliser** is one slider and rides on Tip. Two of the five are names
of our own, and both were needed because the design has no word for the thing:

- **Blending**, for colour pickup. Filing it under "Wet edges" would have
  borrowed a term that means something else in every application that has it.
- **Inputs**, for the modulation table. It could not go on Dynamics: that
  section is three curves that all answer "what does pressing harder do", and
  this is a *list* of arbitrary length whose rows each pick their own target and
  their own driver. No amount of column arithmetic makes those the same shape,
  and the two names have to draw exactly that distinction — Dynamics is
  pressure, Inputs is everything else.

The stroke ramp is drawn **dead**, with the reason under it, whenever nothing on
the brush reads stroke position. Speed lead sits on Scatter with the other
things that move a dab off the line rather than on Inputs with the modulations,
because it is a property of the brush and not a row in the table — and it is
spelled "Speed lead" rather than as scatter because a lead trails and a spray
does not.

Roundness is shown rather than `dab_ratio`, because that is the design's word
and every other paint application's; the two are reciprocals. Angle and angle
jitter are disabled on a round dab, with a line saying why, since a circle has
no angle.

See `docs/brush-sources.md` for the packs that were considered and why the ones
that are missing are missing.
