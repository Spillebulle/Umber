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

**222 presets**: Umber's own five, all 196 MyPaint brushes, and 21 out of four
more packs — David Revoy's 2025-01 Krita bundle, Raghavendra Kamath's v2.1,
GDQuest's, and rubberduck's 60 GIMP stamps. All CC0 except GDQuest's, which is
CC-BY and therefore carries its credit.

Those 21 are the Krita brushes whose dab is *generated* rather than stamped, so
they convert exactly. The rest of what those packs hold — 269 stamps and about
90 more presets — needs a bitmap tip, and the shipped library has nowhere to put
one. `docs/brush-sources.md` has the counts, the measurements and the reason.
Every one of them imports.

It was 128 for a while. The 68 missing were refused by the generator rather than
lost by accident, and both reasons were engine gaps rather than import gaps:

| Was refused | Count | Now |
|---|---|---|
| `smudge >= 0.5` | 67 | The stroke carries a colour per dab and samples the canvas asynchronously. See the colour-pickup section of `CLAUDE.md`. |
| time-based dabs only | 2 | The dab loop has a time term, so an airbrush keeps depositing while the pen is held still. |

(One was refused on both counts, hence 68 rather than 69.)

The generator's check is now `Imported::dropped`, which every reader fills in
per brush, and a non-empty list is a refusal. `mypaint::unsupported_features`
feeds it for a `.myb` and still lists `colorize` and `lock_alpha`, which nothing
in that pack uses.

Per **brush**, not per file, and that distinction is the whole reason the field
exists: `brushimport::dropped_features` answers the same question for a whole
file, which is what the import notice wants — twenty files should not produce
twenty notices — but a `.bundle` of forty-six brushes reports the union of
everything any of them dropped, and the generator has to be able to keep the
thirty that dropped nothing.

The two checks are deliberately separate. A user who asks for a specific file
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

Counted over the 217 the generator converts; Umber's own five are on top of
these and sort the same way.

| Collection | Count |
|---|---|
| Pencils & sketching | 14 |
| Inks & pens | 32 |
| Markers | 5 |
| Charcoal, chalk & pastel | 6 |
| Paint & brushes | 50 |
| Watercolour & wet media | 22 |
| Airbrush & spray | 13 |
| Blenders & smudge | 23 |
| Erasers | 9 |
| Texture & grain | 13 |
| Foliage & fur | 8 |
| Effects & experimental | 22 |

Adding the stamp packs meant teaching `RULES` the words they use. They name a
brush after the mark — "Cracks", "Vegetation", "Exploding Sparks", "Waterfall" —
rather than after a medium, so without those rules rubberduck's pack arrives as
269 brushes in "Paint & brushes". None of the 196 MyPaint brushes moved
collection as a result, which is the check that the new rules are additions
rather than a reshuffle.

The generator prints this table on every run, because a classifier is a
judgement and judgements have to be looked at rather than asserted. The first
attempt sorted on `smudge >= 0.5` before consulting the name and put 68 brushes
in "Blenders" — MyPaint's oil paints all mix with what is under them, so the
setting says far less than it appears to. Erasing is the only setting that now
overrides a name; see the module docs for the rest of the ordering.

## What a MyPaint `.myb` conversion keeps

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

## What a MyPaint `.myb` conversion loses

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
  darkens as a stroke crosses itself. Umber takes a `max` of coverage across the
  whole stroke and applies opacity once at commit — that is the wet-layer design
  in `CLAUDE.md`, and it is why `opaque_linearize` is ignored rather than
  approximated. A MyPaint wash and an Umber wash of the same numbers will not
  look the same.

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

## What Umber reads

| Extension | From | Yields | Reader |
|---|---|---|---|
| `.myb` | MyPaint | one brush | `brushimport::mypaint` |
| `.gbr`, `.gpb` | GIMP, Krita | one stamp | `brushimport::gbr` |
| `.gih` | GIMP animated brush | **one per cell** | `brushimport::gih` |
| `.vbr` | GIMP parametric brush | one brush, exactly | `brushimport::vbr` |
| `.kpp` | Krita paintop preset | one brush | `brushimport::kpp` |
| `.bundle` | Krita resource bundle | **a whole pack** | `brushimport::bundle` |
| `.abr` | Photoshop 1, 2, 6.1, 6.2 | one per sampled stamp | `brushimport::abr` |
| `.ron` | an Umber library | as many as it holds | `preset::parse_library` |

Two of those are containers, so `read_file` returns a `Vec` and the import
notice has to report "twenty brushes arrived" as readily as "one did".

Every reader is pinned by fixtures **built byte by byte in its own test
module**, not by a vendored file, so `cargo test` means something on a checkout
where no pack was ever fetched. Each was also run against the real archives once
— see `docs/brush-sources.md` for the counts, and the commit for what that
turned up, which was mostly things a hand-built fixture cannot show you.

### Importing a GIMP brush

`umber_core::brushimport::gbr::from_gbr` decodes a `.gbr` into a `TipMask`, plus
the brush name and the spacing the format carries. Everything in it is
big-endian; a little-endian read reports a billion-pixel brush and is caught by
the length check rather than producing garbage.

**Import brushes…** in the Brushes panel reads one, saves the mask into `tips/`,
and selects the brush — importing a brush is asking to paint with it, and a
stamp is unrecognisable in a list and obvious the moment it makes a mark.

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
so, the same way a `.myb` that leans on `colorize` does. A `.gpb` — GIMP's
obsolete pixmap brush — is a `.gbr` with a whole colour *pattern* stapled to the
end of it, and reads through the same path with the same note.

Knowing where a `.gbr` **ends** is load-bearing rather than tidy: a `.gih` is
made of whole `.gbr` files concatenated, so a length misjudged by one byte makes
rubbish of every cell after the first.

### Importing a GIMP brush pipe (`.gih`)

A pipe is a *sequence* of stamps plus a rule for choosing between them, and it
is what a large part of the free stamp collections is actually made of — 43 of
the 60 files in rubberduck's pack are `.gih`, not `.gbr`.

**Umber binds one tip per stroke**, so a pipe cannot arrive as one brush that
rotates through five stamps. Of the three ways out — one preset per cell, a cell
chosen per dab, or one representative cell and a note — the importer takes the
first, because it is the only one that loses no *pixels*: every stamp the artist
drew arrives, named `Bark 1` … `Bark 5`, and can be painted with. What it loses
is the **sequencing**, and the import says so.

Choosing per dab is the better answer and needs the dab pass to hold an array of
tips and an index per instance — see "Not done yet".

### Importing a GIMP parametric brush (`.vbr`)

The one GIMP format Umber reproduces **exactly**. A `.vbr` is not a picture: it
is a shape, a radius, a hardness, an aspect ratio and an angle, and every one of
those has a field on `Brush` already. Nothing is resampled and nothing is
approximated, so the tests can assert exact values.

Only two things are dropped, and the file names them in words, so the import
knows precisely when it is approximating: GIMP's **square** and **diamond**
shapes, and its 3-to-20-point **stars**. Umber's dab is an ellipse — `dab.wgsl`
tests `length(local) <= 1` — so those arrive round.

### Importing a Krita preset (`.kpp`) and bundle (`.bundle`)

A `.kpp` is a **PNG** — the thumbnail Krita shows in its chooser — with the
settings in a text chunk keyed `preset`. Three things about it are easy to get
wrong, and all three were got wrong first:

- **All three of PNG's text chunks turn up.** Of Revoy's 46 presets, 33 use
  `zTXt`, 11 use `iTXt` and the rest `tEXt`, with no pattern to it. A reader
  that knows only one silently rejects a quarter of a real pack.
- **`Pressure<Name>` is not "pressure drives this".** It is Krita's historical
  spelling of "the `<Name>` dynamic is switched on"; *which* input drives it is
  in `<Name>Sensor`. Read the obvious way, every speed- and angle-driven
  dynamic becomes a pressure one.
- **A Krita PNG brush is inverted relative to a `.gbr`.** White is no paint and
  black is full — the opposite of `TipMask`'s convention. Read the `.gbr` way it
  gives a solid square with a hole in it.

| Krita | Umber |
|---|---|
| `brush_definition` `MaskGenerator@diameter` × the size curve's peak | `size` |
| `MaskGenerator@ratio` | `dab_ratio`, **reciprocated** — Krita scales the short axis |
| `MaskGenerator@hfade` | `hardness`, **inverted** — fade is softness |
| `Brush@angle`, in **radians** | `dab_angle` |
| `Brush@spacing` | `spacing` |
| `OpacityValue` × `FlowValue` × the opacity curve's peak | `opacity` |
| `Pressure<X>` + `<X>Sensor` + `<X>commonCurve` | `pressure_*`, `min_*_ratio`, `*_curve` |
| `RotationSensor` `id="drawingangle"` / `"fuzzy"` | `dab_angle_follows_stroke` / `dab_angle_jitter` |
| `ScatterValue` | `scatter` |
| `EraserMode`, `CompositeOp` | `mode` |
| `isAirbrushing` + `rate` | `dabs_per_second` |
| `SmudgeRateValue`, `SmudgeRadiusValue` (colorsmudge) | `smudge`, `smudge_radius` |

**Only two of Krita's paint engines are accepted**: `paintbrush` and
`colorsmudge`. `deformbrush` moves pixels around, `experimentbrush` fills an
outline, `hairybrush` simulates bristles, `spraybrush` scatters particles — they
are *different programs*, not settings, and a round dab wearing their name would
be pure invention. They are refused by name, and inside a bundle one refusal
does not take the other forty-five with it.

The tip is either a generated ellipse — which Umber draws exactly — or a
predefined brush naming a file. A preset with `embedded_resources` carries that
file base64-encoded in `<resources>`; a preset in a bundle finds it in
`brushes/`; a **loose** `.kpp` finds it in a sibling `brushes/`, which is the
layout every pack distributed as a directory uses and the difference between 14
and 22 of GDQuest's brushes arriving as stamps. When none of those works the
brush arrives round and the import *names the file it wanted*, because a stamp
brush quietly painting round is the failure the whole reader is written against.

A `.bundle` is a ZIP holding all of that plus `meta.xml`, which is where the
author and the licence live — and therefore where a `Credit` comes from.
`docs/brush-sources.md` explains why that matters more than it sounds.

### Importing a Photoshop brush (`.abr`)

Four incompatible layouts share the extension — 1, 2, 6.1 and 6.2 — and all are
big-endian. The reference is GIMP's own `app/core/gimpbrush-load.c`, which is
GPL-3.0 like Umber and is the only description of the format checked against
real files for twenty years.

**Sampled brushes only, deliberately, exactly as GIMP does it.** A Photoshop
brush is either a bitmap or a set of parameters, and in versions 6 and 10 those
parameters are not with the brush at all: they are in a separate `8BIMdesc`
section written in Photoshop's *descriptor* format, a nested self-describing
structure with a dozen type codes that would be a second format implemented
inside the first, for numbers Umber has four of. So a `.abr` brings its stamps
and nothing else; spacing, angle, roundness and scatter come out as Umber's
defaults, which for a stamp brush is what the `.gbr` reader does anyway. The
import says both — how many computed brushes were skipped, and that the settings
were left behind.

No `.abr` pack is fetched: see `docs/brush-sources.md`.

In the brush editor's **Tip** tab a stamp brush shows the mask, its size in
pixels, and a way back to the round dab. **Hardness is drawn dead there**, with
the reason underneath: a tip *replaces* the procedural falloff rather than being
multiplied into it — `select(round, masked, use_tip)` in `dab.wgsl` — so
hardness has nothing left to shape. A round brush shows none of this. Almost
every brush is round, and a permanent row saying so would be a control that
never does anything.

## Not done yet

- **Tips in the *shipped* library**, which is now the only thing between four
  fetched packs and 269 more brushes in the picker. `BrushPreset::tip` resolves
  against the user's library only. Shipping a stamp needs the masks in the
  binary — a generated `assets/tips/` and an `include_bytes!` table beside
  `builtin-brushes.ron` — and a rule for which of a pack's tips reproduce
  faithfully. The measurements that decide the second are in
  `docs/brush-sources.md`, and they also say why the first is not free: one
  pack's stamps are 10.7 MB of PNG against a 200 KB library.
- **Picking a cell per dab**, which is what would make a `.gih` a brush rather
  than five. The dab pass binds one tip per pass — that is what keeps a thousand
  tipped dabs a single draw call — so it would need the tip binding to become a
  small array and the dab instance to carry an index into it, chosen by the same
  seeded RNG that already drives scatter and angle jitter. Every pipe in the
  wild picks at random, so `sel0:` would not have to be honoured in full. Until
  then a pipe arrives as one preset per cell and says so.
- **A stamp brush's row in the library looks round.** `widgets::brush_row`
  paints its sample from opacity and hardness, which is what a procedural dab
  is made of. The brush editor shows the mask; the list does not.
- **Grain / paper texture** multiplied into dab coverage.
- **Elliptical tips.** The tip is stretched over the dab's bounding square, so a
  non-square mask loses its aspect ratio. The dab carries a single radius and
  has nowhere to record one.
- **Ellipticity driven by an input**, and scatter driven by pen speed — see
  "What a MyPaint `.myb` conversion loses" above for why each was left alone
  approximated.
- **`lock_alpha`, `colorize` and `change_color_*`.** No brush in the shipped
  pack sets any of them to a live value, so nothing in the library is waiting on
  them. They are worth building as painting features in their own right —
  `lock_alpha` especially — not as import fidelity.
- **Per-brush blend modes.**
- **Krita's other paint engines** — `spraybrush`, `hairybrush`, `deformbrush`,
  `experimentbrush`, `hatchingbrush`, `roundmarker`. A `.kpp` written by one is
  refused by name rather than approximated; between them they account for 13 of
  the 116 presets in the fetched Krita packs.
- **Krita's masking brush, paper texture, mirrored dabs and impasto**, and its
  brush-tip randomness and density. All are reported when a preset asks for one.
- **Photoshop's brush descriptors.** A `.abr` brings its stamps; its spacing,
  angle, roundness and scatter are in a nested binary descriptor section that
  would be a second format inside the first.

## What a user can set

The brush editor has four sections, following the design's naming where it has
a name for the thing:

| Section | Holds |
|---|---|
| Tip | size, hardness, opacity, spacing, airbrush rate, roundness, angle, angle-follows-stroke, stabilisation |
| Dynamics | pressure source, and pressure → size / opacity / hardness with their curves and floors |
| Scatter | scatter, size jitter, angle jitter, pressure → scatter |
| Blending | colour pickup, smear length, pickup radius |

That is every field of `Brush` except `mode`, which is the tool choice (Brush
or Eraser) rather than a brush setting, and is on the tool rail.

Two of the design's six sections are not drawn at all rather than drawn empty:
**Texture** has no engine behind it (see above) and **Wet edges** has none
either. **Stabiliser** is one slider and rides on Tip. Colour pickup needed a
home and none of the design's names is one, so **Blending** is a name of our
own — filing it under "Wet edges" would have borrowed a term that means
something else in every application that has it.

Roundness is shown rather than `dab_ratio`, because that is the design's word
and every other paint application's; the two are reciprocals. Angle and angle
jitter are disabled on a round dab, with a line saying why, since a circle has
no angle.

See `docs/brush-sources.md` for the packs that were considered and why the ones
that are missing are missing.
