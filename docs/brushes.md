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
git show HEAD:crates/umber-core/assets/builtin-brushes.ron > /tmp/before.ron
pwsh tools/fetch-brushes.ps1     # or: sh tools/fetch-brushes.sh
cargo run -p umber-core --example build-brush-library
cargo fmt --all                  # tip_table.rs is source, and is written raw
cargo run -p umber-core --example diff-brush-library -- \
    /tmp/before.ron crates/umber-core/assets/builtin-brushes.ron
cargo run -p umber-core --example survey-mypaint   # the tables below
```

`cargo fmt` is not tidiness there: `tip_table.rs` is a Rust file the generator
writes one entry per line, and CI runs `fmt --check`. Leave it out and a
regeneration that changed nothing still shows up as a diff.

The first step downloads the packs into `assets/brushes/`, which is git-ignored,
and records what it took in `assets/brushes/LICENSES.md`, which is not. The
second converts them and rewrites `builtin-brushes.ron`. Both steps are
deliberate acts rather than a `build.rs`: the packs are not in the repository,
so a build script would make a clean checkout unbuildable, and a generated file
that lands in a commit is a file whose diff can be read.

**The generator is reproducible**, and was not until it was checked. Running it
twice on an unchanged tree rewrote about a hundred lines in the last decimal
place, because MyPaint's `value = base + Σ mapping(input)` was summed in
`HashMap` order and floating-point addition is not associative — so Rust's
per-process hash seed moved the last bit. The inputs are a `BTreeMap` now.
Regenerating with nothing behind it produces byte-identical output, which is
what a committed generated file has to do before its diff means anything.

**The third step is what makes that true.** Fifteen thousand lines of
pretty-printed RON is not readable by hand: one field of one brush and every
field of every brush look alike in `git diff`. `diff-brush-library` answers the
four questions that matter instead — did a preset appear or vanish, which
*fields* moved and in how many brushes, did anything change collection, and is
the resulting spread of values sane. The regeneration that found the Krita
faults below moved 23 fields across 191 of 215 presets, and no reading of the
raw diff would have separated the deliberate changes from the two that were
bugs.

Preview thumbnails are never downloaded. In the MyPaint pack the brush
*settings* are CC0 but some of the previews are CC-BY, and not having the files
is the surest way not to ship them.

## What is in it today

**258 presets**: Umber's own six, all 196 MyPaint brushes, and 56 out of four
more packs — David Revoy's 2025-01 Krita bundle (21), Raghavendra Kamath's v2.1
(9), GDQuest's (26), and rubberduck's 60 GIMP stamps (none). All CC0 except
GDQuest's, which is CC-BY and therefore carries its credit.

**Read the counts off a run of the generator, not off this page.** It prints
the total and the refusal table every time, and the figures here have been
wrong twice by being added to rather than re-measured — once because a reader
changed and nobody regenerated, once because two branches counted from
different baselines. Everything below is from the run that produced the
committed library.

32 of those 56 stamp a **bitmap tip** and 6 paint through the author's own
**paper texture**: 25 masks and 10 pattern tiles, 8-bit greyscale PNG in
`crates/umber-core/assets/tips/` and `assets/patterns/`. Measured once at 624
kB of PNG, the release binary grew by 664 kB, so a bitmap costs the binary
about what it costs the directory. See "Tips in the shipped library" below.

The rest of what those packs hold needs a mask too and still does not ship. 338
brushes across the five packs carry one, and the numbers say where they went:

| | |
|---:|---|
| 11 | ship |
| 17 | are rubberduck's, whose masks this project does not redistribute — a licence decision, in `docs/brush-sources.md` |
| 310 | lost something else on the way in and would have been refused whatever happened to their masks; 257 of them for one reason alone — a `.gih` pipe's sequencing, which Umber cannot reproduce |

Every one of the 338 imports.

### Why the other 363 are refused

The generator prints this on every run, and it is the honest answer to "why is
my favourite brush not in here". A brush is counted under every reason it names,
so the rows overlap; **alone** is the number refused for that reason and nothing
else, which is what a fix aimed at that reason would actually ship.

| Refused for | mypaint | deevad | raghukamath | gdquest | rubberduck | total | alone |
|---|---:|---:|---:|---:|---:|---:|---:|
| animated brush sequences (a `.gih` pipe) | 0 | 12 | 1 | 2 | 252 | 267 | 259 |
| mirrored dabs | 0 | 9 | 3 | 7 | 0 | 19 | 7 |
| a paper texture in one of Krita's other blending modes | 0 | 5 | 5 | 8 | 0 | 18 | 6 |
| a mask this project does not redistribute | 0 | 0 | 0 | 0 | 17 | 17 | 17 |
| dab rotation driven by tilt, pen rotation or pressure | 0 | 8 | 1 | 4 | 0 | 13 | 2 |
| bitmap tips stored outside the file | 0 | 0 | 0 | 10 | 0 | 10 | 3 |
| square brush shapes | 0 | 0 | 5 | 2 | 0 | 7 | 0 |
| a paper texture whose strength varies over the stroke | 0 | 1 | 5 | 0 | 0 | 6 | 1 |
| brush-tip density | 0 | 0 | 5 | 0 | 0 | 5 | 1 |
| a paper texture whose cutoff Krita applies before scaling its pattern | 0 | 0 | 4 | 0 | 0 | 4 | 0 |
| brush-tip randomness | 0 | 0 | 4 | 0 | 0 | 4 | 0 |
| edge sharpening | 0 | 1 | 1 | 2 | 0 | 4 | 1 |
| star-shaped brushes | 0 | 0 | 1 | 2 | 0 | 3 | 1 |
| a dynamic driven by an input Umber cannot produce | 0 | 0 | 0 | 2 | 0 | 2 | 0 |
| a dynamic that varies in Krita and arrives constant here | 0 | 1 | 0 | 0 | 0 | 1 | 1 |
| a stamp too faint to make a mark | 0 | 0 | 1 | 0 | 0 | 1 | 1 |
| masking brushes | 0 | 1 | 0 | 0 | 0 | 1 | 0 |

333 refused in all, against 252 shipped. **"paper texture" used to be one row of
31, eleven of them alone**; it is three rows now, because the pattern itself
comes across and what is left is the parts of Krita's texture option Umber has
no model for, which are not one thing. Ten presets are refused for paper
reasons and nothing else, and the **alone** column no longer adds up to that:
three of the ten carry two paper losses at once, so they are alone under
neither. See the section under the Krita reader.

**Thirteen more presets are refused whole, before a brush comes out of them**,
because they name one of Krita's other paint engines: `experimentbrush` ×4,
`spraybrush` ×3, `hairybrush` ×2, `deformbrush` ×2, `hatchingbrush`,
`roundmarker` — GDQuest 7, Raghukamath 4, Revoy 2.

Only GDQuest's seven appear in the run's own output, and that is a hole worth
naming rather than rounding off: GDQuest ships loose `.kpp`, so each refusal is
a file `read_file` failed on, where the other six are *inside* bundles.
`BundleContents::refused` records them and `brushimport::read_file`'s `bundle`
arm throws it away, so nothing downstream can see them. Six brushes are missing
from a table this section calls the honest answer, which is exactly the shape of
omission the table exists against.

Three things the table says that are worth reading twice.

- **Not one of these refusals is about having nowhere to put a bitmap.** The
  shipped library has carried masks since `tip::builtin` arrived, so the tip
  half of the question is already answered; a *user's* tip library changes
  nothing here, because what the generator ships is not what the user stores.
  "Bitmap tips stored outside the file" is a `.kpp` naming a predefined brush
  that is in no pack Umber fetched — nothing in Umber can supply what is not in
  the download.
- **The `.gih` row's 257 are 252 of rubberduck's and five others**, and
  rubberduck's masks do not ship anyway. A dab pass that could rotate through an
  array of tips would therefore add 5 brushes to the library and 252 to what an
  *import* reproduces faithfully. Both are worth having and they are not the
  same claim.
- **`umber-core::dynamics` exists and `kpp.rs` does not use it.** That row is a
  refusal the *reader* makes rather than one the engine forces, and its own
  wording is wrong about these packs — no dynamic in any of them is driven by
  tilt or by stroke position. **19 of the 20 name `fuzzy`**, Krita's per-dab
  random: 9 alone, and 10 through a compound `sensorslist` holding pressure and
  fuzzy together (11 such dynamics, because "pack01-leaf" has two). The
  twentieth is GDQuest's "Ink speed", whose size is driven by `speed`. Umber has
  had `DabInput::Random` and `DabInput::Speed` since the MyPaint importer needed
  them, so **every one of the 20 names an input the engine can drive**. One of
  the fuzzy ones, "Blend Smoky", carries a second dynamic on `fuzzystroke` — a
  draw held for a whole stroke — which is the only input here with no
  equivalent. What is missing is the half of `mypaint.rs` that turns a mapping
  into a `Modulated` entry, which this reader never grew. See "The two things
  that would unlock the most" below.

  The **rotation** row is the opposite case and is genuinely blocked: 7
  `ascension` (tilt direction), 4 `rotation` (barrel), 1 `tangentialpressure` —
  none of which any desktop pointer here reports — and one `pressure`, which
  would want an Angle target driven by pressure that `dynamics` does not carry.

#### The two things that would unlock the most

Ranked by brushes gained for work done. The first is engine work and the second
is not, which is most of why the second is worth doing first.

1. **A `.gih` pipe chosen per dab.** 257 brushes refused for this and nothing
   else, more than every other reason put together. It needs the dab pass to
   hold an array of tips and an index per instance — engine work, and the only
   entry here that is. 252 of the 257 are
   rubberduck's, whose masks do not ship, so the *library* gains 5 and an
   import gains 252 faithful stamps. Both are real and they are not the same
   number.
2. **Krita dynamics as `Modulated` entries.** 3 brushes ship the day it lands,
   19 stop being approximated on import, and it needs no engine change at all —
   the table, the inputs and the targets are all there. It also closes a second
   fault on the way: `Preset::dynamic` and `has_foreign_sensor` both read
   `sensor_id`, the *first* id in the sensor, which for Krita's compound
   `<params id="sensorslist">` is the wrapper's own name. That is the exact bug
   `sensor_ids` was written for and it was only ever applied to rotation, so
   for those 10 presets — 11 dynamics — the pressure curve is being dropped as
   well as the random one. No shipped preset is affected, because all 20 are
   refused today.

One more, for scale: **paper texture**, which was 31 refusals with 11 of them
alone and is now 24 with 10, is the section below.

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

Counted over the 222 the generator converts; Umber's own six are on top of
these and sort the same way.

| Collection | Count |
|---|---|
| Pencils & sketching | 13 |
| Inks & pens | 32 |
| Markers | 5 |
| Charcoal, chalk & pastel | 6 |
| Paint & brushes | 58 |
| Watercolour & wet media | 22 |
| Airbrush & spray | 14 |
| Blenders & smudge | 22 |
| Erasers | 9 |
| Texture & grain | 13 |
| Foliage & fur | 9 |
| Effects & experimental | 20 |

Adding the stamp packs meant teaching `RULES` the words they use. They name a
brush after the mark — "Cracks", "Vegetation", "Exploding Sparks", "Waterfall" —
rather than after a medium, so without those rules rubberduck's pack arrives as
269 brushes in "Paint & brushes". None of the 196 MyPaint brushes moved
collection as a result, which is the check that the new rules are additions
rather than a reshuffle.

Exactly one brush has changed collection since, and not because a rule changed:
`classic/modelling2` reads its colour pickup entirely as a pressure mapping, so
once that was imported as a modulation instead of a base value its `smudge`
field fell below the half that the last-resort rule tests, and a brush called
"Modelling2" moved from "Blenders & smudge" to "Paint & brushes". That is the
right answer — it is a modelling brush — and it is a reminder that the settings
rule is the weakest evidence there is, which is why it sits last.

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

## What a MyPaint `.myb` conversion loses

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
  nothing is lost here. The engine and the library both have them now, and
  eleven Krita stamps ship through them; it is the Krita and GIMP packs they
  exist for.
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

Three things look like faults and are not:

- **`color_h` / `color_s` / `color_v`** are non-default in 52 brushes each — 63
  brushes set at least one — and the values are simply whatever colour was on
  the canvas when the file was saved. `(0.1006, 0.2536, 0.8196)` repeats
  verbatim in eleven unrelated brushes and `(0.002, 0.9615, 0.3833)` in nine,
  which is the whole story in two lines. MyPaint only applies them when
  `restore_color` is set, which **two** brushes do. Ignoring them is correct.
- **`opaque_linearize`** is non-default in 123 brushes, and 94 of those set it
  to **zero** — they are switching it off. It is MyPaint reducing per-dab alpha
  so that dabs compounding at the brush's spacing reach the requested opacity.
  Umber's `max` coverage reaches exactly `opacity` already.
- **`anti_aliasing`** is non-default in 100 brushes. It is a minimum edge
  fadeout in pixels, and Umber's dab shader applies one unconditionally, sized
  from the dab's short axis. Nothing is lost.

### The fields no source fills

Some of `Brush` has no counterpart in any format the library is built from, so
every imported preset carries Umber's own default there. That is only right when
the field is *dead* for that brush, and it was worth checking one by one which
of them are:

| Field | Default in | Live in | Verdict |
|---|---|---|---|
| `min_size_ratio` | 92 of 222 | the other 130, all of which set `pressure_size` | dead where defaulted |
| `min_hardness_ratio` | 154 | the other 68, all of which set `pressure_hardness` | dead where defaulted |
| `grain`, `grain_scale` | all but 6 | 6, all Krita presets whose texture is a plain Multiply | see "The paper texture" under the Krita reader |
| `grain_pattern` | every one | none, and it cannot be otherwise: an import names its own tile through `BrushPreset::paper`, which overrides the enum | the enum is the *shipped* set, and an imported paper is never one of them |
| `build_up` | 222 | none, and this row used to be 232/1 | see below |
| `stroke_span` | 166 | 37 read the `Stroke` input | 27 carry a span nothing reads; the editor draws it dead |
| `stabilization` | 26 (every Krita preset) | 51 MyPaint brushes set `slow_tracking` | Krita stores stabilisation on the *tool*, not the brush |

The two ratios are the useful result: the counts add up to 222 exactly, so no
brush that varies with pressure is falling back on a default, and the ones that
do not vary are carrying a number nothing reads.

The grain row said "nothing in any pack asks for paper" and was **wrong**, which
is worth leaving in the record rather than quietly correcting: 31 presets across
the three Krita packs switch a texture on, and `kpp.rs` never saw one because it
read `Texture/Enabled` where Krita writes `Texture/Pattern/Enabled`. Eleven of
them were shipping without their grain. A table of defaults is only evidence
that a field is dead if the reader that would have filled it in is looking in
the right place. It then said all 31 were refused for it, which was true when it
was written and is not now: six of them ship with their author's own paper.

`build_up` went with them. It was 232 default and 1 live — Raghukamath's
"Drybrush", measured — and that one brush asks for paper, so **no brush out of a
pack sets `build_up` any more**. Umber's own "Stipple chalk" still does, so the
blend state is exercised and the flag is still measured per brush rather than
defaulted, by `stroke_coverage`; every stamp an import produces gets the same
answer as before. What has gone is the *third-party* example of it, which is
what `crates/umber-render/src/canvas.rs` measured its `R8Unorm` accumulation
error against.

Note also that this table counts what the generator writes rather than
`preset::builtin()` — Umber's own six are on top of it — and that the two grain
rows are from the current run where the rest are from the one at 222 presets.
Stipple chalk used to be the one shipped brush that painted through paper; it is
now one of seven, and the other six are Krita presets carrying their author's
own tile.

`smudge_length` and `smudge_radius` look like the same case at a glance — 153
and 195 presets sit on Umber's default — and are not: those *are* MyPaint's
defaults, read out of the file through the same evaluation as everything else.

Krita's is the one place a default is doing real work. Its stabiliser belongs to
the freehand tool rather than to the preset, so there is nothing in a `.kpp` to
import and Umber's 0.35 is as good an answer as exists — but it does mean a
Krita brush arrives with smoothing its author never asked for, while a MyPaint
brush arrives with exactly what it did ask for, which is usually none.

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
| `.sut`, `.sutg` | Clip Studio Paint | one, or **a whole group** | `brushimport::clipstudio` |
| `.ron` | an Umber library | as many as it holds | `preset::parse_library` |

Three of those are containers, so `read_file` returns a `Vec` and the import
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

**Import…** in the brush library browser reads one, saves the mask into `tips/`,
and selects the brush — importing a brush is asking to paint with it, and a
stamp is unrecognisable in a list and obvious the moment it makes a mark.

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

A 4-byte `.gbr` is a **coloured stamp** and keeps its colour. It used to import
as its silhouette, because the stroke scratch is one coverage channel and a tip
was a coverage mask; see "Coloured stamps" below for why neither of those had to
change. A `.gpb` — GIMP's obsolete pixmap brush — is a `.gbr` with a whole
colour *pattern* stapled to the end of it, and reads through the same path with
the same result. GIMP writes **straight** alpha, which is the form `TipMask`
holds, so both planes go across as they are and
`brushimport::dropped_features` reports nothing at all for either.

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
tips and an index per instance. **`docs/brush-pipes.md` is the design**, and it
is where the figures below have to be re-checked; `cargo run --release -p
umber-core --example measure-pipes` is what produces them.

A rule is read **per dimension**, not once. Every pipe in every fetched pack is
`dim:1`, so `sel0:` alone is right for all 55 of them and is not right in
general: a `dim:2` pipe that turns with the stroke along one axis and shuffles
along the other loses two different things, and naming one of them sends
somebody looking for a rotating stamp when half the mark is a shuffle.

**Two collapses turn a pipe into one brush losing nothing**, and both are
decided from the file rather than from a judgement about what the stamps
depict:

| | |
|---|---|
| nothing walks | every dimension states `constant`, so GIMP paints the first cell for ever and the rest are unreachable — read off the header |
| the cells cannot differ | every cell is the same brush, so choosing between them makes exactly the mark one of them repeated makes — compared byte for byte, over the mask, its spacing and whether it carried a colour |

Two of the 55 are the second, both of them tips inside David Revoy's bundle. A
one-cell pipe is the degenerate case of it: a sequence of one has no sequencing,
so `ncells:1 sel0:incremental` is not a loss.

**`sel0:angular` is the exception, and it is named separately.** Of GIMP's eight
selection rules that one is not a shuffle: it picks the cell by the *direction
of the stroke*, so the cells are one mark drawn at `rank` rotations and painting
a curve turns the stamp through them. Umber's dab does exactly that natively —
`dab_angle_follows_stroke` turns the quad and its tip with it, continuously
rather than in `rank` steps — so one cell plus that flag would reproduce such a
pipe rather than approximate it.

It is deliberately not done, and the difference from the two above is the whole
reason. Collapsing to the first cell is right only if the other cells really are
that cell rotated, which is what `angular` *means* but not what the file *says*,
and a pipe of unrelated pictures walked angularly would lose every stamp but one
in silence. Deciding it from the pixels needs a resampler this crate does not
have and a threshold nobody could calibrate, because **not one pipe in any
fetched pack is angular**: 44 loose files and 11 inside the two Krita bundles,
51 `random` and 4 `incremental`. There is nothing to check such a collapse
against. Until there is, an angular pipe arrives as one preset per cell and the
import names the rule that was lost rather than the general one.

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
  dynamic becomes a pressure one. It also gates the setting's **value**, not
  only its curve — see "What the first Krita imports got wrong" below.
- **A Krita PNG brush is inverted relative to a `.gbr`.** White is no paint and
  black is full — the opposite of `TipMask`'s convention. Read the `.gbr` way it
  gives a solid square with a hole in it.

| Krita | Umber |
|---|---|
| `brush_definition` `MaskGenerator@diameter` × the size curve's peak | `size` |
| `MaskGenerator@ratio` | `dab_ratio`, **reciprocated** — Krita scales the short axis |
| `MaskGenerator@hfade` | `hardness`, **directly** — fade is the opaque fraction of the radius |
| `MaskGenerator@softness_curve` (`id="soft"`) | `hardness`, from where the curve crosses a half |
| `Brush@angle`, in **radians** | `dab_angle` |
| `Brush@spacing` | `spacing` |
| `OpacityValue` × `FlowValue` × the opacity curve's peak | `opacity` |
| `Pressure<X>` + a `pressure` sensor + its curve | `pressure_*`, `min_*_ratio`, `*_curve` |
| the same option's `fuzzy` sensor | a `Modulation` on `DabInput::Random`; its peak scales the setting |
| `RotationSensor` `id="drawingangle"` / `"fuzzy"`, including inside a `sensorslist` | `dab_angle_follows_stroke` / `dab_angle_jitter` |
| `RotationSensor@angleOffset`, degrees | added to `dab_angle` — the rake's lean |
| a bitmap tip's measured stroke coverage | `build_up` |
| `PressureScatter` × `ScatterValue` | `scatter` |
| `EraserMode`, `CompositeOp` | `mode` |
| `AirbrushOption/` **or** `PaintOpSettings/` `isAirbrushing` + `rate` | `dabs_per_second` |
| `SmudgeRateValue`, `SmudgeRadiusValue` (colorsmudge) | `smudge`, `smudge_radius` |

#### What the first Krita imports got wrong

Three faults, all found by auditing the shipped presets against their sources
rather than by a test, and all of the same shape: a value read without the thing
that decides whether Krita reads it at all.

- **`hfade` is hardness, not softness**, and it was inverted. Every generated
  Krita brush in the library arrived inside out — GDQuest's and Raghukamath's
  ink brushes as diffuse clouds, Revoy's "Eraser Kneaded Soft" and both GDQuest
  airbrushes as hard discs. What settled it is that a `.kpp` *is* a PNG and the
  image is **Krita's own preview of the brush**, so the pack answers the
  question directly; the seven presets that pin the direction are tabulated in
  the module docs of `brushimport/kpp.rs`. Three mask generators share the
  attribute: `default` fades inward from `hfade` of the radius, `gauss` blurs by
  `1 - hfade`, and `soft` ignores `hfade` altogether and states its falloff in
  `softness_curve`, which is now read instead.
- **`ScatterValue` was applied whether or not Krita's scatter option was on.**
  Krita writes the value at its default of 1 into every preset regardless, and
  `KisScatterOption` returns the unscattered position when the option is
  unchecked — so 93 of the 119 presets in the fetched packs were being sprayed,
  including an ink brush that got five radii of it. The enable flag is
  `PressureScatter`, the very rule stated two bullets above; it was being
  applied to the curve and not to the value.
- **Krita has spelled the airbrush option two ways** — `PaintOpSettings/` and
  `AirbrushOption/` — and both are in the packs, 45 presets and 52. Knowing only
  the first imported GDQuest's "Airbrush", which asks for a thousand dabs a
  second, as an ordinary distance-driven brush.

A fourth thing was being dropped in silence rather than got wrong. **Krita's
Sharpness** thresholds the finished mask into a hard, aliased edge; `dab.wgsl`
antialiases unconditionally and has nothing to switch off. It is the whole of a
pixel-art brush, so it is now named — which costs the library GDQuest's two
pixel-art presets, correctly: a one-pixel brush that paints a soft grey dot is
not the brush its author drew.

#### The paper texture, and what a texture library would buy

A fifth fault, and the same shape as the first four: a value read without the
thing that decides whether Krita reads it — except here the *key itself* was
invented. The reader tested `Texture/Enabled`. Krita has never written that:
every texture setting is under `Texture/Pattern/`, in `kis_texture_option.cpp`
at v4.4.8 and in `KisTextureOptionData.cpp` on master, which are the two ends of
the range these packs were written in. So the flag was false in all 119 presets
of the fetched packs, no import has ever mentioned a paper, and the table of
fields no source fills recorded "nothing in any pack asks for paper" as a
*finding*.

**31 presets switch a texture on**, at a live strength — thirty of the
thirty-one at 1.0 and the last at 0.45 — and **eleven of them were shipping**.
Several are named
for the grain they had lost: "F) Thick Dry Canvas", "GDquest Texture Fabric",
"GDquest Rock Texture Crevaces", "F) Rough Rake Textured", "C5) Thin Brush Hard
Edge Textured", both of Raghukamath's Drybrushes. Every one of them was refused
once the option was read, which is why the library dropped to 222 presets from
233; six have since come back with their paper, and the ten that have not are
the table two sections down.

`MaskingBrush/Enabled` sits two lines above it in the same reader, is spelled
correctly, and fires on the one preset that uses it. That is what made the
silence read as an absence of textured brushes rather than as a bug, and it is
the argument for the pack sweeps in this file: a hand-built fixture pins the
reader against itself, and only a real archive can say whether the reader is
looking in the right place. The fixture in `kpp.rs` had the invented key too.

**A texture library was not what unblocked these**, and the numbers are worth
keeping because they are what said which half of the job was which:

| | |
|---:|---|
| 31 | presets switch a texture on |
| 20 | carry the pattern base64-encoded in the preset itself |
| 11 | are Revoy's, naming six patterns his bundle ships under `patterns/` |
| 13 | are Multiply, which is what `Brush::grain` already is |

So every pattern was *available* — the store is a `BTreeMap` and a resolver, and
`.kpp`'s tips had already solved that exact shape — and every pack's licence is
verified inside its own download, which is what redistributing a bitmap needs:
the `ship_tips` question in `docs/brush-sources.md`, asked again about paper.
What was missing was the **model**. Krita's texture carries a texturing mode —
`TexturingMode` in `KisTextureOptionData.h`, and the packs use five of its
sixteen: Multiply (13), Subtract (14), a dodge, Hard Mix (softer) and Height ×2
— plus an inversion (11), a levels remap of the pattern
(`CutoffLeft`/`CutoffRight`/`CutoffPolicy`, 15), a brightness and contrast on it
(11 state them and 5 carry a live brightness), and a pressure curve on the
strength (25).

#### What was built, and why that half

The split that mattered turned out to be **which of those is a function of one
texel's grey**, not which of them is small.

- The inversion, the levels remap, the brightness, the contrast and the neutral
  point are *all* per-texel, so all five are **baked into the stored tile** at
  import by `TextureSpec::levels` — a 256-entry table, applied once, exactly
  reproducing `KisTextureMaskInfo::recalculateMask`. That is not an
  approximation and it costs the engine nothing: a tile is stored once and
  sampled for ever, `Brush` gains no field, the brush editor gains no control,
  and `mix(1.0, tile, strength)` is still the exact identity at strength zero.
- The mode is **not**. Subtract is `max(0, alpha − mask)` and no tile makes a
  multiply do that — at half coverage through a half-lit texel the two differ
  by a quarter of the mark.
- **Multiply is two arithmetics and both come across**, which is the one thing
  here that was got wrong and then corrected. `KisMaskingBrushCompositeOp`
  specialises on `use_soft_texturing`: with it on, Multiply is
  `alpha × (mask × strength + (1 − strength))`, which is Umber's grain written
  out; with it **off**, which is Krita's default and what all 31 presets use,
  it is `mul(src, dst, strength)` — a three-way product whose strength dims
  every dab whether or not the paper bites there. The two coincide at strength
  1.0, which is where all thirteen Multiply presets sit, so reading only the
  first was right about every brush that exists and wrong about the maths. The
  plain form's constant *is* a stroke opacity, so `TextureSpec::bite` folds it
  into `Brush::opacity` — the slot `FlowValue` already folds into — and both
  are exact.
- The strength's pressure curve is not either, and for a different reason: the
  grain strength is a *per-pass uniform*, so following pressure means moving it
  into the per-dab instance data.

Baking the four cheap ones brought back **six** presets — five of Revoy's and
GDquest's "Texture Fabric" — and took the library from 247 to 252. The eleven
that this section used to say were "shipping without their grain" had become
sixteen by the time the tiles could be read, because other work in between had
stopped refusing them for other reasons.

#### What the remaining ten would cost

Ten presets are still refused for paper alone, and the order of the two
remaining features is **not** the order it looked in before the tiles could be
read:

| Missing | Presets waiting on it | What it needs |
|---|---:|---|
| a grain strength that follows pressure | 4 | `grain` per dab rather than per pass: instance data, `StrokeBuilder`, a curve or a `dynamics` target, a control |
| Krita's Subtract | 3 | a `GrainMode` on `Brush`, a scalar in `DabParams`, a `select` in `dab.wgsl`, a control, a GPU test |
| Height ×2, Hard Mix ×1 | 3 | the same shader work again, twice more, for one preset each |
| levelling the pattern before it is scaled | 0 | nothing on its own: all four that name it also want the pressure curve above |

All four of the pressure-curve group are Raghukamath's and all four are
*Multiply*, so their tiles come across; what is missing is the ramp, and — for
all four of them, which is why the last row buys nothing on its own — the order
of the levelling against the scale. The Subtract three are "Pack01 Chalk02",
"Pack01 Crayon09" and "GDquest Rock Texture Crevaces".

Neither was built. Each buys three or four brushes for a change that reaches the
dab pipeline or the shader plus a control in the brush editor, where the bake
bought six for a table; and the Subtract path in particular has to be weighed
against the standing rule that a strength of zero is the exact identity — a
`select` keeps that true (`max(0, m − 0)` is `m`) but puts two extra ALU
operations on every fragment of every dab, paper or none. If either is picked
up, the pressure curve is worth more than Subtract and the two Height presets
and the one Hard Mix are worth neither.

Everything above is `cargo run -p umber-core --example build-brush-library`'s
own output; re-run it before quoting any of these figures.

#### And three more, all about how a dab turns

Found by asking the packs what they actually say about rotation rather than by
reading the reader. All three are silent losses, which for a *bitmap* tip is the
most visible kind there is: a stamp that was meant to turn and does not is a
comb.

- **A compound sensor defeated the reader.** Krita writes
  `<params id="sensorslist">` with a `<ChildSensor>` per input when a dynamic has
  more than one, and `sensor_id` took the first id it found — the wrapper's.
  Five presets in the fetched packs rotate that way, GDQuest's cloud and rock
  brushes among them, and every one of them laid each stamp the same way up.
- **`angleOffset` was dropped**, which is the rake's *lean*. Krita adds it to
  the drawing angle inside the sensor — `0.5 + drawingAngle / 2π +
  angleOffset / 360` — so it goes through exactly the transformation the heading
  does, and Umber composes the same two terms the same way in
  `angle = heading + dab_angle`. Four presets state one, between 92° and 139°:
  without it they drag their bristles *along* the stroke instead of across it.
- **A rotation driven by something Umber has no value for was on and did
  nothing.** `ascension` is tilt direction, `rotation` is barrel rotation, and a
  `pressure` rotation would need an `Angle` modulation this reader does not
  build. Named now, on the same terms as the other foreign sensors, which costs
  the library "B3) Basic Oval Brush" — correctly: what Krita draws for one of
  these at rest could not even be determined, and that is a better reason to
  refuse a brush than to ship it.

**Fan corners** is the one rotation feature approximated rather than named.
Krita adds dabs through a sharp corner so a rake fans round it; Umber's dab
turns with the heading and the heading turns at the corner, so a stroke differs
only where it changes direction abruptly. Six presets ask for it.

#### A dynamic is a *set* of sensors, and only one of them was being read

The rotation fix above stopped at rotation, and the same fault was sitting on
Size, Opacity and Scatter — where it cost both halves of the dynamic rather
than one. Krita states an option as a set of sensors and **multiplies** their
outputs together; `sensor_id` took the first id in the blob, which for a
compound `<params id="sensorslist">` is the wrapper's own name. So a preset
driven by pressure *and* something else lost its pressure curve entirely, and
was then refused for a foreign sensor it had never actually been tested for.
Eleven presets in the fetched packs drive one of the three that way. The only
reason none of them shipped painting wrongly is that the same fault refused all
eleven.

Both halves now go through `sensor_ids`. Pressure stays the curve on the brush;
the rest become modulation-table entries where Umber has the input, which
today means `fuzzy` — Krita's fresh uniform draw per dab, exactly
`DabInput::Random`. Because Krita multiplies, each sensor's **peak** lands on
the setting's own value and the entry carries the fraction of that peak the
draw asks for, which is the arrangement `mypaint`'s opacity path already uses
and the reason `Brush::size` and `Brush::opacity` stay "the value at the peak".
The units are the target's own, and all three differ: a log offset for size,
a factor for opacity, and `scatter × (factor − 1)` for scatter, because Umber
adds where Krita multiplies. That last is the one approximation — exact for a
brush whose scatter has no pressure sensor beside it, which is seven of the
nine in the packs, and at light pressure an under-estimate rather than a spray
nobody asked for.

**Two sensors stay named rather than approximated.** Krita's `speed` is a
fraction of a fixed maximum drawing speed and `DabInput::Speed` is MyPaint's
log-speed axis, on which 45 px/s reads 0.5; nothing in the preset says where
the other's axis begins, so a curve written for one cannot be placed on the
other from what is in the file, and a guess is a brush that thins at a speed
nobody draws at. `fuzzystroke` is one draw for a whole stroke where Umber's
random is one per dab, which would turn a single splash into confetti. One
preset in the packs asks for each.

Two more things fell out of reading the sensors properly:

- **`<Name>UseSameCurve` decides which of the two curves in the file is in
  force**, and Krita's editor leaves the other behind either way. Six options
  across four presets carry both.
  Deevad's "Eraser Kneaded Soft" states the flag false and a bare
  pressure sensor with no curve of its own, so Krita gives it a linear ramp
  where Umber was giving it the shared curve sitting unused beside it — the one
  brush already in the library that this changed.
- **A sensor that states no curve is the identity**, not a dynamic that does
  nothing: Krita's curve object is constructed as the diagonal and only a
  `<curve>` child replaces it.

The net is 233 presets to 235: "C3) Thin Brush Textured" arrives with both its
pressure ramp and an Opacity/Random entry running 0.42 to 1, and "Y) Splatters
Light" varies each stamp between a sixth and all of its 230 px, which is what
makes it splatter. A third, "X) Textured Chaotic Irregular", is still refused
and correctly — see the open question below.

**Two sentences, not one, for what is left behind.** A sensor Umber has no input
for and a sensor reaching a curve Krita switched off are different losses, and
one message covering both names a cause that is not the cause: `fuzzy` is an
input Umber demonstrably *can* produce. So `FOREIGN_INPUT` and `UNREAD_CURVE`,
with the first winning where both apply, for the reason the generator asks what
was dropped before it asks about the mask.

#### The open question: `<Name>UseCurve` switched off

This reader treats `Pressure<Name>` on with `<Name>UseCurve` off as **no
dynamic**. Krita's own default for that flag is on, and its curve object is the
diagonal, so the reading that fits everything else here is "the sensor, applied
straight" — a *linear* ramp rather than a constant. **34 of the fetched presets'
Opacity and Scatter options have it off**, so if that is right they are all
importing without a pressure ramp their author drew.

It is not settled here, and deliberately. This module's other polarity question
— `hfade` — was settled by looking at Krita's own preview inside the `.kpp`,
and that technique cannot separate these two readings: almost every preview
tapers because *size* follows pressure too. Changing 34 shipped brushes on a
reading nobody has checked is the larger risk of the two, and naming it as a
loss on all of them would be the same claim in the other direction. So the
reading stands, this paragraph is the record, and the one place it does surface
is where a *non-pressure* sensor reaches such a curve: `UNREAD_CURVE` names it,
which is what keeps "X) Textured Chaotic Irregular" out of the library rather
than shipping half of what its author drew.

**Only two of Krita's paint engines are accepted**: `paintbrush` and
`colorsmudge`. `deformbrush` moves pixels around, `experimentbrush` fills an
outline, `hairybrush` simulates bristles, `spraybrush` scatters particles — they
are *different programs*, not settings, and a round dab wearing their name would
be pure invention. They are refused by name, and inside a bundle one refusal
does not take the other forty-five with it — which is why
`brushimport::refusals` exists beside `read_file`: the brushes come back as the
`Ok`, so a preset that is not a brush has nowhere in that answer to go, and six
of the thirteen such presets in the packs are inside an archive. The generator's
refusal table was counting the seven loose ones and reading as though the rest
did not exist.

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

### Importing a Clip Studio sub-tool (`.sut`, `.sutg`)

A `.sut` is an **ordinary SQLite database**, and so is a `.sutg`. Four tables
either way — `Manager`, `Node`, `Variant`, `MaterialFile` — which is why one
reader serves both: a single sub-tool is the degenerate group, one node with no
children. The nodes are a linked list rather than a table order, so the palette's
own order comes off `NodeFirstChildUuid` and `NodeNextUuid`; each names *two*
settings blocks and the second, `NodeInitVariantID`, is the "reset to defaults"
copy that holds almost nothing.

**The schema is not fixed.** The two sample files declare 187 and 214 columns in
`Variant`, interleaved rather than appended, because the larger group holds a
fill tool and the fill, selection and shape tools share the table. Every column
is therefore looked up by name; a reader that counted them would take a brush's
rotation out of its neighbour's fill tolerance. A sub-tool with no `BrushSize`
at all is not a brush and is skipped, which sorts the fill tools out by the data
rather than by a tool-type number this build might not recognise.

**No SQLite dependency.** `umber_core::sqlite` is a read-only page and record
walker of a few hundred lines: a header, table b-trees, overflow chains, varints
and the serial-type encoding. `rusqlite` with `bundled` would put a C toolchain
in the build, which is the same problem `ureq` avoids by taking `rustls` — and
one that would only show up when a release was cut, since the desktop build
everybody develops against has a C compiler. Its module docs carry the argument
and the list of what it deliberately will not do.

**A tip and a paper are both the material's own pixels, and the thumbnail is the
fallback.** `MaterialFile.FileData` is a USTAR tar holding both:
`data/material_0.layer`, which is what the artist drew, and
`thumbnail/thumbnail.png`, a PNG preview of it with a longest side of 300.
`brushimport::csmaterial` reads the first — see below — and
`brushimport::clipstudio`'s `tip_for` and `paper_for` fall back to the second
for a material Clip Studio left out of the file or a container shape the reader
will not guess at. The fallback names itself; taking the material does not,
because there is then nothing to apologise for.

The thumbnail's coverage is `alpha × (1 − luminance)` and has to be both terms:
a brush tip is black on transparent, so its alpha is the mark, and a paper
texture is opaque grey, so its luminance is. Either alone turns the other kind
into a blank rectangle. The material needs no such reading — its first channel
*is* the coverage.

Build-up is then **measured** off whichever mask was bound, by
`tip::stroke_coverage`, the same function that decides it for the shipped
library — Clip Studio composites every dab as GIMP and Krita do.

A material larger than `TipMask::MAX_SIZE` is **reduced to it and said so**,
rather than refused as `TipMask::from_picture` refuses an oversized picture.
The difference is what refusing falls back to: there, the file on disk; here, a
300-pixel preview. The cap cannot simply be raised — device limits are
`downlevel_defaults`, which guarantees a `max_texture_dimension_2d` of exactly
2048, so a wider mask is one the dab pass could not bind at all. Nothing in the
sample files reaches it: the largest material is 1174 × 1120.

**What that costs in memory**, measured on the sample files: the whole
thirteen-brush `.sutg` carries four stamps totalling **1.44 MB** — 1.36 MB of
coverage in RAM and 0.09 MB of PNG in `tips/` — of which the 1174 × 1120
spatter brush is 1.25 MB and 0.05 MB on its own. The single-brush `.sut` is
0.07 MB. Against the thumbnail route those four were 0.20 MB, so the picture
costs about seven times what its preview did, per stamp brush.

A **paper** costs far less than that, and not because the pictures are smaller:
`UserLibrary::store_paper` shares a tile equal to one already held, where
`store_tip` deliberately does not. The `.sutg`'s six textured sub-tools name two
papers between them, so what is written is one 500 × 500 tile (0.24 MB of
coverage) and one 300 × 300 (0.09 MB, the thumbnail fallback) — not six.

#### Where the full-resolution pixels actually are

Recorded here because it took a day to find and because nothing else writes it
down. **There is no proprietary codec.** `data/material_0.layer` is a C2F
container — magic `\x89C2F\r\n\x1a\n`, then chunks of `[u32 le size][4-byte
tag][payload][u32 checksum]` tagged `HEAD`, `dATA`, `TAIL` — and its `dATA`
payloads open with a `u16` flag. The one with flag 1 is a fixed 5128 bytes of
something with no structure left in it. **The one with flag 0 is an ordinary
SQLite database**, and everything is inside it:

- Page size 1024, and **the file header and the first five pages are absent**:
  the payload's first page is page **six**, so a page number `n` names the byte
  range `(n − 6) × 1024`. This paragraph said *seven* until the reader was
  built, and the number is not a matter of taste — an overflow chain names
  absolute page numbers, and the picture is exactly the blob large enough to
  need one, so at any other offset the largest blob the database yields is the
  254 bytes of an *empty* mipmap level. Every other claim in this section was
  re-derived from the sample files at the same time; this is the one that did
  not survive.
- There is **no `sqlite_master`**, because it lived on page 1. The way in is
  `Database::scan`, which visits every page and decodes the table leaves, and a
  row is recognised by what is in it rather than by a table name. Note that an
  overflow page holds raw bytes, so one whose first byte happens to be `0x0d`
  decodes as a leaf and yields a spurious row — which is why a caller must
  identify what it wants by content and never by counting.
- The table wanted is `Offscreen` — `_PW_ID`, `MainId`, `CanvasId`, `LayerId`,
  `Attribute`, `BlockData` — and a material holds **three** of its rows: two
  empty mipmap levels and one with the pixels. `MainId` is 3, 7 and 9 in one
  file and 3, 8 and 10 in another, so it is not the key; having blocks is.
- `Attribute` opens with a short header whose first word is its own length, and
  then fields framed `[u32 name length][utf-16be name][payload]`. The first is
  `Parameter`, and its payload opens with the material's true width and height
  as plain integers, then the block columns and rows — `501 × 501` for a paper
  whose thumbnail is 300 × 300, `1174 × 1120` for a spatter brush whose
  thumbnail is 300 × 286. That is the resolution that was being given up.
  (The row that carries the pixels states one pixel less on each axis than the
  two empty levels do, in every material sampled.)
- `BlockData` is framed differently — `[u32 size][u32 name length][utf-16be
  name][payload]`, the size covering the whole record — and is one
  `BlockDataBeginChunk` record per block, each closed by a nested
  `BlockDataEndChunk` marker, then a `BlockStatus` record. A block's payload is
  `u32 index`, `u32 uncompressed bytes`, `u32 256`, `u32 256`, `u32 present`,
  and then, only where it is present, `u32 length + 4`, `u32` **little-endian**
  `length`, and a **plain zlib stream** — `78 01`, nothing exotic. The reader
  trusts neither length for the *end* of the stream: the record's own size
  minus the closing marker is the one bound the container guarantees.
- Blocks are 256 × 256, laid out row-major over `ceil(width / 256)` columns,
  and their channels are **planar**. `uncompressed bytes` is `channels × 65536`
  and says how many there are: 1 for a paper, 2 for a tip, 5 for a colour
  material. **Channel 0 is the coverage** — `0` no paint, `255` full, which is
  `TipMask`'s own convention — and where a second channel exists it is all
  zeroes in every material sampled, so reading it as an alpha and multiplying
  would give a mask that paints nothing.
- Round-tripped against the sample files: decoded, downscaled and compared with
  the shipped thumbnail's own coverage, the mean absolute difference is 0.0002,
  0.0397, 0.0612 and 0.0680 on a 0..1 scale, which is the resampling and
  nothing more.
- **The five-channel shape is refused rather than guessed at.** It is an alpha
  that is solid and four that look like colour; `alpha × (1 − luma)` over the
  first three is the obvious reading, it was tried, and it lands 0.13 away from
  that material's own thumbnail — twice the worst above, and unexplained. The
  thumbnail is a smaller picture of the right thing; a mask that is *plausibly*
  a paper is the quietly wrong picture this project refuses everywhere.

Two of the six materials in the sample files take the fallback: that one, and
one whose flag-0 payload is not a headerless database at any page size or first
page the reader sweeps.

`umber_core::sqlite::Database::headerless` and `::scan` are the second entry
point this needed, and `flate2` — already in the tree behind `zip` and `png` —
is now a direct dependency of `umber-core`, built `rust_backend` so no C
toolchain enters the build.

**Effect sources are read for pressure, speed and randomness.** Every setting
carries a bitmask of which inputs drive it, a floor per input and a control-point
curve. Clip Studio's Dynamics dialog lists its sources as **Pen pressure, Tilt,
Velocity, Random**, and the bits are that list from bit 4 up: `0x10`, `0x20`,
`0x40`, `0x80`. Three things agree. Bit 4 is set on the size effector of every
pressure-sensitive brush in the samples and nothing else; bit 7 is the only bit
ever set on the hue, saturation and brightness effectors, which is what colour
jitter is; and Ken Evans' `CSPBrushInfo`, an independent decoding of these same
blobs, reads the four the same way. A fifth bit, `0x100`, is declared as
supported by brush size and its neighbours and is never switched on in either
sample file; it is named as unrecognised rather than guessed at.

**Pressure and randomness driving a setting Umber has no field for are lost and
deliberately not named.** They are as lost as a tilt mapping, so reporting them
was tried — and it is wrong here for two compounding reasons. The sweep runs
over a schema of 187 to 214 columns and cannot tell a live effector from one
whose bits Clip Studio left behind when the setting was switched off, which is
the trap `BrushUseIn`, `BrushRotationEffector`, `BrushAutoIntervalType` and the
texture reference each read a separate field to avoid. And those two bits are set
on far more columns than tilt or velocity ever are: the random bit is the *only*
bit ever set on the hue, saturation and brightness effectors in either sample
file, so a brush with colour jitter switched off would have apologised for a
mapping it does not have, and one with it switched on would have said the same
loss twice, once vaguely. A list that cries wolf is one a reader learns to skip,
which costs the losses that do matter — the argument the skipped fill tool and
the automatic dab interval already make. Naming these properly needs the enable
flag beside each effector, and that means learning what those columns are
called.

**A dynamic's floor is half of what it says, and it is carried with the curve.**
Clip Studio states the minimum as a percentage of the setting's own value, which
is exactly what `Brush::min_size_ratio` means for size. Opacity has no such
field — coverage genuinely reaches zero, so Umber does not want one — so the
floor is folded into the response curve instead, where a fixed row of evenly
spaced samples represents `f + (1 − f) × curve(p)` exactly. Reading the curve
alone dropped it, and a brush whose author had it painting from six tenths
arrived painting from nothing: every stroke a fraction of the strength it was
set to, reaching the colour asked for only after being laid down several times,
which is indistinguishable from an opacity control that does not work.

**Per-dab coverage is Opacity times Brush density, under pressure as well as
under speed.** Clip Studio's Opacity is the whole stroke's and its Brush density
is one dab's, and they multiply; `SPEED_TARGETS` already composed them that way
for velocity while the pressure half read `BrushOpacityEffector` alone, so a
brush whose density followed the pen imported painting at full density
throughout. Two curves multiply sample by sample, which is the whole reason one
`ResponseCurve` can carry both — exact at the five knots and bowing by at most
`Δa·Δb/4` between them, since the product of two piecewise-linear curves is
quadratic. That is the resolution `ResponseCurve` has at all, and the bound
`curve_for` already accepts when it resamples Clip Studio's control points onto
those knots.

**Velocity is Umber's `Speed` input, and it reaches size and per-dab opacity.**
The shape is the same in both engines: full at a standstill, falling towards the
input's floor as the pen moves. So the modulation's `low` is that floor and its
`high` is the untouched value — for size in log units, because that is how
`DabTarget::Size` composes, and for opacity as a factor, because that is how
`DabTarget::Opacity` composes. Velocity on any other setting has nowhere to land
and is named. The two apps' speed *scales* are not the same number of pixels per
second, so the mark thins in the right direction over roughly the right stretch
of hand movement rather than exactly Clip Studio's.

**The dab's angle answers to a different list of sources in the same bits, and
`1 << 6` on it is the stroke's own heading rather than velocity.** Clip Studio's
*Direction* dynamic has its own dialog and the manual lists it as **None,
direction of pen, pen tilt, direction of line, random** — no velocity anywhere in
it. `BrushRotationEffector` is therefore the one column whose bitmask means
something else, and putting it through the sweep's reading was wrong twice at
once: every sketching pencil in both sample files imported as a **fixed nib**
where its author had the tip following the mark, and each raised
`SPEED_ELSEWHERE` about a setting Clip Studio cannot drive with speed at all —
a wrong mark under a note pointing somewhere else, which is the worst shape this
class of bug takes. It was also the *only* source of that sentence in either
file. Direction of line is `Brush::dab_angle_follows_stroke` exactly, `dab_angle`
becomes the lean on top of the heading — which is what a stated angle means in
Clip Studio once a direction source is on, and one sample brush leans 45° off
the line — and the flag is *assigned* rather than only set, because Umber's own
default has to be switched off for a nib.

**One bit is anchored and the rest follow the dialog's order, which is a weaker
footing than the four ordinary sources have and is worth saying plainly.**
`1 << 7` is random, and it is the bit that carries an amount: of the thirteen
brushes, the eight without it hold `BrushRotationRandomScale` at its untouched
100 and not one holds anything else, while four of the five with it hold a
deliberate 45 or 10. The correlation runs one way — the fifth sets the bit and
leaves the amount at 100, a full turn and a legitimate setting — so what it pins
is that nobody sets the amount without the bit. That is also the one bit this
reader already had right, so the imported jitter is unchanged.

Random being last then puts the other three on bits 4, 5 and 6 in the dialog's
own order, and that is the whole of the argument for `1 << 6`. It sits on four
elongated, textured sketch pencils, one leaning 45° off the line — a reading a
painter would recognise, and **not proof**: the same four are a plausible
pen-tilt brush too, so if the manual's order is not the file's order, `1 << 6` is
pen tilt and those four import as rakes that should be nibs. That is how this
change can be wrong, and it is a wrong *mark* where the bug it replaces was only
a wrong note. `1 << 5` then falls on the two flat brushes, both 30% thick and
stated at 90°, and driving a flat marker's angle from pen tilt is a stock Clip
Studio recipe; `1 << 4` falls on three round brushes, and "direction of pen" is
the *azimuth* of the tilt rather than its amount, so it is still tilt. `1 << 8`
is never set in either file and therefore keeps the unrecognised-source wording:
a later Clip Studio adds "rotation of pen axis" to this dialog and appending it
is the reading that fits, but that is an inference about a version and an
insertion point stacked on the inference above, for a bit nobody has observed.

**A direction source is only reported where the angle can be seen.** A round dab
with no tip is the same picture at every angle, so a tilt-driven direction on one
is a setting whose absence nobody can point at, and naming it is the cry-wolf
failure `unreachable_inputs` spends two paragraphs refusing — two of the thirteen
brushes are exactly that shape. `dab_has_angle` answers the elliptical half and
the tip is the other, combined exactly as `Editor::tip` combines them for the
brush editor, so a round *stamp* brush still says what it lost: a bitmap is not
rotationally symmetric whatever the dab's roundness.

**Pen tilt is dropped, and not for want of knowing which bit it is.** Umber has
no tilt input on any platform it runs on — winit carries tilt only inside iOS's
`Force::Calibrated`, and the `WM_POINTER` path a Windows pen arrives through
does not surface it — so a tilt modulation would be evaluated at a value the pen
never produces, for ever. That is the "control that lies" the interface rules
refuse, one level down. It becomes worth building the moment an input source
exists; see the pressure section of `CLAUDE.md` for what that would take.

**The taper arrives at the start of a stroke and not at its end.** `BrushUseIn`
with `BrushInLength` is a size ramp over the first stretch of the mark, which is
exactly `DabInput::Stroke`: `stroke_span` becomes the length in dab radii, so a
brush scaled up tapers over a proportionally longer mark, and `stroke_hold` goes
to its ceiling so the ramp never wraps — a taper happens once. `BrushUseOut`
cannot follow: it is measured back from an end the engine does not know until
the stroke is over. The floor is `DabTarget::Size`'s own `-2` in log units
rather than zero, because a log offset cannot state zero; `exp(-2)` is a dab an
eighth of its width, which reads as a point.

**A sub-tool that is not a brush is skipped without a word.** A `.sutg` is a
tool group and one holding a fill or a selection tool is the ordinary case, not
a loss — the same argument the automatic dab interval gets below.

What else carries: hardness, opacity, stabilisation (`FlickerReduction`), the
dab's flatness (`BrushThickness`, as `1 / thickness`, since `Brush::size` names
the long axis either way) and its angle, the fixed dab interval, the spray as
scatter, the paper's strength, tile size **and its own picture**, and the
underlying-colour mixing as one smudge amount. Clip Studio splits mixing into how much paint the brush
carries and how dense it is, so the stronger of "carries none" and "is dense" is
what survives — which puts a pure blender at 1.0 and an oil brush at its density,
and says it approximated either way.

**The paper is read only where the reference names a material.** Clip Studio
leaves a setting's value in the file when the setting is switched off — the trap
`BrushUseIn`, `BrushRotationEffector` and `BrushAutoIntervalType` are each read
to avoid — and a stale texture reference is the same trap with the worst
consequence of the three, because grain **multiplies coverage**: a brush that was
never textured paints through a paper it does not have, mottled, weaker than its
opacity claims, and darker each time the stroke is laid down again, since the
pits are anchored to the document and a second pass composites over the first.
Deliberately not gated on the material being *present*, only on it being named:
Clip Studio leaves an installed one out of the file and expects to find it
locally, exactly as it does for a tip. The two failures are told apart — a
reference holding no materials is a texture that was never set and there is
nothing to report, while one naming a material this reader cannot resolve is a
paper the brush genuinely has, and it is named rather than passed over, which is
the answer `UNUSABLE_TIP` already gives for the analogous tip.

**And the paper's own picture comes across**, into the user's texture library,
with the preset naming it — see "A paper of your own". It used to become
`GrainPattern::Tooth` at whatever strength the file asked for, on the reasoning
that Umber's papers were a closed set. They still are; what changed is that
`BrushPreset::paper` can name a tile beside them, and substituting inside the
closed set was wrong for the reason above turned round: `Tooth`'s mean is 0.775,
so every textured brush arrived painting at about 78% of the opacity its author
set, through pits nobody drew. A material this reader cannot resolve now paints
flat and says so. The picture is taken only where the grain actually bites,
which is the threshold the renderer binds a tile at — a strength left in the
file at zero is a setting that was switched off, and a tile stored for it is a
file per sub-tool that nothing samples.

**The paper is the material's own pixels, on the tip's terms**, through the same
`csmaterial` reader and with `thumbnail.png` as the same fallback; the fallback
is named and taking the material is not. Three things ride on the route and only
the first is the one anybody expects.

- **Resolution.** The 500 × 500 paper in the sample files was arriving as its
  300 × 300 preview.
- **Polarity.** `csmaterial` hands back *ink* and a grain texel is the fraction
  of the dab that **stays**, so the plane is complemented. It is an exact
  complement rather than an approximate one: for straight-alpha pixels,
  `1 − a(1 − L)` is `(1 − a) + aL`, which is `tip::grain_of` written out — so
  the composite-over-white that rule insists on comes across for free, and on
  the neutral grey a paper is, Rec. 601 and Rec. 709 agree exactly, so the two
  routes into a tile cannot disagree about a texel. Measured over the four
  readable materials, the complement of plane 0 lands 0.0397..0.0987 of a level
  from `grain_of` of the thumbnail — the resampling — against 0.53..0.93 the
  other way round.
- **Whether it tiles at all.** A preview render is under no obligation to, and
  the browser's seam check judges whatever it is given. The 500 × 500 paper's
  material declares `isTiling` in `icedata/layerData.xml` and joins to itself
  within its own noise (a signed step of 2.4 levels across the join against an
  interior figure of 2.0); its thumbnail steps by **62** against an interior
  figure of 2.9, and by 54 on the other axis. Every brush carrying that paper
  was being reported as drawing a grid over the canvas, and the grid was the
  preview's.

**The tile size is the material's own size times `TextureScale2`**, which is
what that percentage means and what could not be worked out while the picture
was a preview capped at 300. `GRAIN_TILE_AT_FULL_SCALE` (256) still stands in
where the material could not be read — and it used to stand in for every case,
which put the sample file's Sketch brushes on a 256 × 0.19 ≈ 49-pixel tile where
their 500 × 500 material at 19% is 95: a paper twice as fine as its author's,
under a 6-pixel pencil. A material larger than `TipMask::MAX_SIZE` is reduced
and named, as a tip is, and the tile size is taken from the size **before** the
reduction — a reduced tile has to cover the same document ground as the picture
it came from, or the grain changes frequency to fit Umber's texture budget.

**Where it cannot cover that ground, `dropped::PAPER_SPACING` says so.**
`Brush::MAX_GRAIN_SCALE` is 2048, and the clamp was unreachable while the tile
was always 256 — it needed a `TextureScale2` of 800%. Reading the material's own
size makes it overlap `REDUCED_PAPER` *exactly*: both fire above
`TipMask::MAX_SIZE`, and at the default 100% a 4096-texel paper asks for a
4096-pixel tile and gets 2048, so the grain recurs twice as often as its author
set. The two are separate notices deliberately — one is a picture coarser than
it was drawn, the other is that picture repeated at a finer pitch, and folding
the second into the first would put a sentence about softening over a change of
spatial frequency.

**And a paper that cannot be resolved at all paints flat in *both* halves.**
The strength is read from `TextureDensity` before the picture is looked for, so
leaving it behind when the picture fails is not "no paper": `paper` unset means
`BrushPreset::paper` unset, which sends `Editor::paper_tile` to
`brush.grain_pattern` — never written by this converter, and `Brush::default()`'s
`Tooth`. That is the 78% substitution above, arriving by the back door on
exactly the brushes whose material Clip Studio left out of the file. The failure
arm zeroes the strength, which is the dab pass's exact identity, and
`a_paper_the_reader_cannot_resolve_is_named_and_paints_flat` asserts the brush
rather than the loss string — it passed for as long as it did not.

Measured, because it is the first thing to look at if an imported brush is ever
reported painting weaker than its opacity says: the `Sketch` brush in the sample
file used to import at `grain: 1.0` on `Tooth`, whose texels run 0.569..0.988
with a mean of 0.775 — so a single stroke at full opacity could not reach 1.0
anywhere, and a second pass over the first was darker, because the pits are
anchored to the document and the second stroke fell into the same ones.

#### The paper decides build-up, and the tip's reading could not see it

It was reported painting weaker than its opacity says, and the paper above was
not why. `Sketch` carries its author's own tile now — a 500×500 grunge scatter,
mean **0.272**, brightest texel 255 — at `TextureDensity` 100. Under the `max`
blend that is the entire stroke at 27% of the opacity it was set to, for as long
as the stroke lasts. Clip Studio composites every dab, which is the sentence the
tip's build-up measurement already rests on here, and the grain is anchored to
the document, so every dab reaching a pixel is scaled by the *same* texel: at a
spacing of 0.100 those faint texels build towards solid and the same tile reaches
**0.771**. Nearly three times the mark, and gritty rather than a flat wash.

Two things kept `stroke_coverage` from catching it, and only the first is an
oversight:

- It ran **inside the tip block**. Two of the four textured sub-tools in the
  reported file carry no bitmap tip at all, so nothing measured them.
- It takes the **peak**, and for a paper the peak is not the mark. A tip is
  stretched over its dab, so a `max` stroke is capped at the mask's brightest
  texel and the whole mark is capped with it. A paper is sampled at the document
  pixel: its brightest texel survives any strength, so peak agreement is 1.0 on
  a tile that is taking three quarters of the stroke away.

So `tip::grain_coverage` is the twin that takes the **mean**, with the strength
folded in through the dab pass's own `mix(1, t, strength)` — zero strength is the
exact identity there as everywhere else. It needs no stamping loop, because
there is no geometry: `max` is exactly the tile and compositing is
`1 − (1 − t)^n` for the `n = 1 / spacing` dabs deep a point sits under. Both
readers ask it, `|=` beside the tip's answer, since either is reason enough and
neither may take the other's off.

**A stencil is the boundary and answers no.** Where a tile holds only 0 and 1
there is nothing for compositing to build — a texel at zero stays there however
many dabs land on it — so the two rules make the identical mark and the cheaper
one is right. The rule is for a grain that is *faint*, not one that is merely
dark, and `a_paper_that_caps_a_stroke_asks_for_build_up_where_a_tips_peak_cannot`
pins both ends of it.

In the shipped library this changed exactly six presets, all of them Krita's and
all at `grain: 1.0`, and nothing else: no preset appeared, none vanished, and
`build_up` is the only field that moved. The guard is
`a_shipped_stamp_paints_at_the_strength_it_was_drawn_at`, which now measures both
halves of the mark and agrees with the generator independently.

**The `Dual*` columns are a second whole brush, and `UseDualBrush` is the only
field that says whether it is live.** Clip Studio's dual brush stamps a second
brush on top of the first at the same time: `Variant` carries a parallel copy of
everything — `DualSize`, `DualFlow`, `DualHardness`, `DualInterval`,
`DualRotation`, `DualPatternImageArray`, a complete `DualTexture*` block and a
complete `DualSpray*` one — which is `2-Brush tip`, `2-Spray effect`,
`2-Stroke` and `2-Paper quality` under `2-Brush shape` in the interface. Four
columns are about the pairing rather than copies of it: `UseDualBrush`,
`DualBrushCompositeMode` (thirteen modes, of which "Height (Linear)" exists
nowhere else in Clip Studio), `SyncDualBrushSize` ("Link to main brush size")
and `ChangeRGBByDual` ("Apply RGB value" — whether the second tip's colour is
applied or only its alpha).

Umber binds one tip and one paper per brush, so there is no half of this worth
painting and `dropped::DUAL_BRUSH` names it. What matters is the gate. **All
thirty variants across the two sample files have `UseDualBrush = 0`**, and every
one has residue beside it — and not the same residue: the `.sut` leaves
`DualSize = 30` where the `.sutg` leaves that column null and
`DualTextureDensity = 50` instead, with `DualBrushCompositeMode = 1` and a
`DualTextureDensityEffector` blob in both. So no neighbour of the flag can stand
in for it, and reading one would put "dual brushes" on the notice of every Clip
Studio brush anybody has ever imported, over a feature none of them uses. That
is the same trap `BrushUseIn`, `BrushAutoIntervalType`, the rotation effector and
the texture reference are each read to avoid;
`a_dual_brush_that_is_switched_off_is_not_reported_from_the_values_left_beside_it`
is what pins it, and the fixture's `VARIANT_COLUMNS` carries those five unread
columns for no other purpose. No `Dual*Effector` has an enabled bit set in
either file, which is why `unreachable_inputs` skipping that whole prefix costs
nothing today.

An automatic dab interval is the one thing deliberately **not** reported. Umber
picks a spacing too, so an automatic one arrives as an automatic one; and every
brush in both sample files is set that way, so a note about it would appear on
every import ever made and train the reader to skip the list that carries the
losses that matter.

No `.sut` pack is fetched and no sample file is committed: the fixtures are built
in the test module like every other reader's, and the reader was run against real
files once during development.

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
  `a + 1(1 − a)` is exactly 1. An `R16Float` scratch would close a 0.4 % gap at
  twice the bandwidth of the hottest texture in the frame — measured against the
  one shipped build-up preset it is worth at most 3 levels of 255, and it could
  not carry the pen's 1024 pressure levels any further, because the layer alpha
  it commits into is 8-bit too. CLAUDE.md's "Pressure" section has the working.

Build-up only means anything where a dab is not solid: a bitmap tip, paper
grain, or a pressure-opacity ramp. For an ordinary brush per-dab coverage is
exactly 1.0 and the two rules agree — which is why nothing in the MyPaint pack
uses it.

## Per-brush blend modes

A layer could multiply, screen, overlay or add onto what was beneath it and a
brush could not: every stroke was source-over. `Brush::blend` is the same
`BlendMode` a layer carries — not a second enum beside it — and it is evaluated
by the same function.

- **The maths lives once, in `shaders/blend.wgsl`.** That file is a prelude
  concatenated in front of both `composite.wgsl` and `commit.wgsl`, so the
  preview and the thing that replaces it at pointer-up compile from one
  statement of what Multiply is. CLAUDE.md's rule that those two must implement
  identical blending maths used to be a rule two files were disciplined into
  keeping; now it is a function they both call. Linear light, premultiplied
  colour, so a brush set to Multiply and a layer set to Multiply mean the same
  thing.
- **It belongs to the composite and commit step, not the dab pass.** The dab
  pass never sees the layer, so it could not evaluate a mode that is a function
  of what is underneath — and anything put there would touch the scratch, which
  holds coverage in 0..1 and must go on doing so. The `max` still saturates,
  `Brush::opacity` is still applied exactly once at commit, and a selection
  still clips by the one multiply it always did.
- **The commit needs a copy of the layer, and Multiply is why.** No combination
  of fixed-function blend factors produces `B(Cb, Cs)`, so a blended commit
  cannot hand its result to the blender: `fs_blend` computes the whole thing and
  is drawn with `blend: None`, reading the destination out of a copy because a
  colour attachment may not also be sampled. The same constraint `flip.wgsl`
  works around, for the same reason.
- **The copy is per damaged piece.** A backdrop spanning the stroke's bounding
  rectangle would be canvas-sized for a thin diagonal — the 381 MB the tiled
  undo patch exists to avoid, put back on the GPU. A piece is a contiguous *run*
  of cells within one row of the 64-pixel damage grid, so a row may hold several
  and the count follows how much the stroke zig-zags rather than only how long
  it is; what is bounded is the backdrop, at `canvas width × 64`, because a
  piece is never taller than a cell nor wider than the stroke's own rectangle.
  The cost is a render pass per piece, since a copy cannot be recorded inside
  one; that is once, at pointer-up, on a path that already blocks on a readback
  for the undo patch.
- **A pass per piece is the part to revisit first if this ever needs to be
  cheaper**, and the argument that produced it only forbids *interleaving*
  copies and passes — not recording every copy first and then drawing one pass.
  Copying the pieces into a single atlas and drawing them under the per-piece
  scissor and dynamic offset that already exist would be one pass, at the cost
  of holding the total piece area at once (6.8 MB for the thin diagonal above,
  but 381 MB for a wash that genuinely covers the canvas — so it would have to
  batch to a byte budget rather than assume one atlas fits). Nothing on the
  desktop needs it: the scissor makes an extra pass nearly free on an immediate
  mode GPU. A tile-based renderer is where it would bite, because wgpu's render
  area is the whole attachment and each pass would load and store every tile of
  the slice — which is Android and iOS, and neither has ever been built.
- **Normal is untouched.** One pass, the fixed-function blender, no copy and no
  allocation — and the preview writes `s + lay * (1 - s.a)` directly rather than
  routing through the general form, because those two agree exactly where the
  general form would differ in the last bit of floating point.
- **An eraser has none, and the control is not drawn for one.** A blend mode
  combines a colour with what is under it and an eraser deposits none: it is a
  different blend state, `src_factor: Zero`, and there is nothing there for
  Multiply to be a mode of. A stroke on a *mask* has none either — a mask holds
  coverage on one channel and its preview is a one-channel blend written to
  match the commit. Both are coerced at `Editor::begin_stroke`, the one gate
  where the mode and the colour already are.
- **A brush row does not preview it.** A row is a swatch on a panel with no
  picture underneath it, so there is nothing for a mode to combine with, and
  drawing one would be a fourth CPU copy of the blend maths beside the shared
  WGSL function.
- **`.myb` has none of this** — MyPaint's `Eraser` setting is the paint/erase
  switch and nothing else — so the importer invents nothing and reports nothing
  dropped. Every existing `brushes.ron` still loads: `Brush` carries
  `#[serde(default)]` on the container, so an omitted field is Normal.

`a_blended_stroke_previews_exactly_as_it_commits` and its partial-opacity twin
are the guards that matter: they stamp, read the preview, commit, and read
again, for every mode.

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

## Coloured stamps

A "coloured stamp" is a tip whose *pixels carry colour*, not just coverage: a
leaf, a spatter of two hues, a texture that stamps its own palette rather than
the brush's. Umber's tips were coverage only, so every such brush arrived as its
own silhouette, and the shipped-library generator refused it outright — nothing
goes out under an author's name painting unlike their brush.

**What carries colour today is GIMP's `.gbr` and `.gpb`, and an RGBA tip already
in the library.** The engine is general and the model is shared, so a reader
that has a colour to hand only has to call `TipMask::coloured` — but Krita's
`.kpp` colour stamps and Clip Studio's still discard theirs, and this section
does not claim otherwise. How many presets that unblocks has not been
re-measured; `docs/brush-sources.md` says what to run rather than guessing.

**It turned out to be a third source of per-dab colour, not something new**, and
that is the whole of why it cost so little. The obvious reading is that a tip
carries colour per *texel* where the existing path carries it per *dab*, and
that the difference needs a second scratch texture. It does not, because **there
is already one and it is a texture**: `CanvasRenderer`'s colour scratch holds a
colour per fragment, and a smudging dab merely happens to write one flat colour
across its own footprint. A stamp writes a different colour at each fragment
into the same target, through the same two coloured pipelines, and everything
downstream reads it through the code it already shared.

So none of this was touched: the `R8Unorm` coverage scratch, its `max` blend,
the build-up target, the selection clip, the four dab pipelines,
`composite.wgsl`, `commit.wgsl`. `dab_coverage` is byte for byte what it was.
`a_coloured_stamps_preview_and_its_commit_agree` is the guard, run across Normal
and all four blended commits.

What was added:

- **`TipMask` gains an optional colour plane.** Straight sRGB, three bytes a
  texel, for the reason `clipboard::Clip` holds a picture that way: it is what
  the file held, so the PNG round trip is byte for byte. Three channels and not
  four, because the fourth would be the coverage written down twice — two
  numbers that could disagree about the edge of a stamp.
- **On disk it is an 8-bit RGBA PNG** in the library's `tips/`, where a mask is
  greyscale. `from_png` reads RGBA as a stamp — the alpha is the coverage,
  unambiguously — and still refuses plain **RGB**, which genuinely has no answer
  to "which channel is the coverage".
- **One binding on the dab pass**, `Rgba8UnormSrgb`, premultiplied in linear
  light with the coverage in the alpha: the layer array's convention and for its
  reason, since only premultiplied colour may be bilinearly filtered without
  haloing the edge of a stamp. `fs_colored` un-premultiplies it and it
  **overrides** `in.color`, which is what a coloured stamp means — the picture in
  the file is the mark, so the palette has nothing to say about it, exactly as
  GIMP's pixmap brushes and Krita's colour stamps behave. `fs`, every ordinary
  stroke, never samples it.
- **The premultiply is `umber-core`'s**, `TipMask::colour_premultiplied`, so it
  is testable without a device and reuses `docimport::srgb`'s encoder rather than
  being a second copy of it.

Which pipeline a stroke uses comes from **one snapshot**. `Brush` cannot answer
"is the tip coloured", because a tip is a *name* the editor resolves — the same
split `Brush::dab_has_angle` already has — so `Editor::begin_stroke` combines
the two halves into `StrokeStyle::per_dab_color` and `app.rs` builds its
`DabStyle` from that same field. That collapses a pair that used to have to
agree: the dab pipeline was read off `StrokeBuilder::is_coloured` and the
preview and commit off `stroke_style`.

It is **refused at that one gate** for an eraser and for a stroke on a mask.
Neither has anywhere for a colour to land: an eraser deposits none, and a mask
is read on `.r`, so a stamp's reds and blues would become "reveal" and "hide". A
coloured stamp used for either paints as the mask it also is, and costs no
colour attachment at all. The smudge probe is gated on the *brush* rather than
on the style, so a coloured stamp does not sample the canvas it never reads.

**The refusal has to reach the dab pass, not only the pipeline choice**, and
getting that wrong was the one real bug a critic found in this work.
`per_dab_color` turns on for a smudging brush as well, so a brush that is
*both* takes the coloured pipeline for its own reason — and `set_tip` deciding
the stamp's colour by itself would have gone on stamping it into a mask, which
previews grey and commits red. So `set_tip` takes the answer as an argument,
`Editor::stroke_stamps_colour` is the snapshot both halves are built from, and
the `Arc`-identity early-out tests that answer as well as the mask: picking up
the eraser does not change the tip.
`a_stamp_told_not_to_colour_paints_what_the_dab_carried` and
`refusing_a_stamps_colour_is_noticed_even_though_the_tip_did_not_change` are
the pair.

A coloured tip costs **five bytes a texel** — one of coverage, four of colour —
where a mask costs one. Only the brushes that carry a colour pay it.

**A picture imported as a tip is still read as coverage**, and that is decided
rather than missing. A black-on-transparent PNG is overwhelmingly somebody's
*mask* — it is how every brush pack on the internet distributes one — and
reading its colour would turn a stamp that has always painted in the palette
colour into one that paints black. Colour arrives only where the file states it
is one: a `.gbr`'s depth of 4, a `.gpb`'s trailing pattern, an RGBA tip in the
library. An explicit "import as a colour stamp" is a control somebody has to be
offered, not a reading to take behind their back — see "Not done yet".

The brush editor's stamp thumbnail shows a coloured stamp's own colour rather
than the theme's ink, because a grey leaf that paints green is a picture that
lies. The brush *row*'s stroke sample still previews in the palette colour: it
is a CPU rasteriser and the deliberate one exception to "no second copy of the
blend rules", so it stays the one thing it is, in the same way it already
declines to show a brush's blend mode.

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

### A paper of your own

`BrushPreset::paper` names a tile in the user library's `papers/` directory,
exactly as `BrushPreset::tip` names one in `tips/`, and it overrides the enum
where it is set. The enum stays closed: a `Custom(String)` arm would end
`Brush: Copy` and would have to be carried through every `Brush` a preview, a
stroke snapshot and an undo entry copies. `Editor::paper_tile` is the one place
a name becomes a tile — the user's library first, then the shipped table, which
is `Editor::apply_preset`'s order for the tip.

- **A name that resolves to nothing paints flat**, and does *not* fall back to
  one of the three. Substituting a paper the author did not choose is the bug
  the Clip Studio importer used to have: grain multiplies coverage and `Tooth`'s
  mean is 0.775, so every textured brush arrived painting at about 78% of the
  opacity it was set to, through pits nobody drew. That is also why a texture
  the importer cannot resolve is still named as a loss.
- **A picture read as paper is read as its *brightness*, with transparency
  composited over white** — `tip::grain_of`. There is nothing to guess, unlike a
  tip: the tile multiplies coverage, so the value at a texel already means how
  much of the dab that texel keeps. It is very nearly the negative of
  `coverage_of`'s ink reading, and it has to be — ink is where the paint goes,
  grain is where the paint stays.
- **Whether a tile joins to itself is measured and reported, not enforced.**
  `tip::seams` compares the step across each edge against the steps inside the
  tile; a paper that fails draws a hard line every `grain_scale` pixels across
  the whole canvas. Textures authored for a painting application are usually
  made to tile, so refusing one would turn away most of what people have, and
  mirroring it into place swaps a seam for an axis of symmetry through every
  stroke — a different artefact rather than none. So it is said out loud, at the
  import and on the row, and the preview draws the tile **two by two** so the
  join is visible at all.
- **A tile is written once and shared.** `UserLibrary::store_paper` reuses a
  paper equal to one already held, where `store_tip` does not: a Clip Studio
  texture material is referenced by every sub-tool in a file that uses it, so a
  group of fifteen brushes over one paper is one picture rather than fifteen.

## Tips in the shipped library

`BrushPreset::tip` used to resolve against the *user's* library only, so nothing
shipped could carry a stamp. It now falls through to `tip::builtin`, which
decodes an `include_bytes!` table generated from the files in
`crates/umber-core/assets/tips/`. Both sources hand back an `Arc<TipMask>` that
is stable for the life of the process, which is what `CanvasRenderer::set_tip`'s
identity check needs.

**Twelve shipped brushes use it**, carried by eleven masks.

One is Umber's own: **Stipple chalk**, a sparse speckle drawn by
`examples/build-bitmaps.rs`. Sparse on purpose — a dense silhouette would paint
identically under either coverage rule and would demonstrate nothing. Its
brightest texel is 0.44, so it ships with `build_up` set, and
`a_shipped_stamp_paints_at_the_strength_it_was_drawn_at` checks that flag
against the measurement rather than against anybody's memory. It checks the
other eleven the same way, which is what makes the flag a measurement rather
than a habit — and **it is now the only shipped stamp that needs build-up**.
Raghukamath's "Drybrush", peak texel 0.878, was the other one and asks for a
paper texture Umber does not carry, so it no longer ships.

The other eleven are Revoy's, Raghukamath's and GDQuest's stamps, written by
`examples/build-brush-library.rs` into the same directory. Three things about
how they get there:

- **Deduplicated by content.** Ten masks carry eleven brushes, because a pack
  routinely cuts several presets from one stamp. That is exactly what
  `BrushPreset::tip` holding a *name* buys — one file, one embedded copy, one
  GPU upload, and `CanvasRenderer::set_tip`'s identity check skipping the upload
  when a second such brush is picked up. A mask takes its name from the **first**
  preset to use it, so refusing an earlier user hands the name to a later one:
  one file was renamed when the textured presets stopped shipping, and that is
  the rule working rather than a fault.
- **At their original resolution.** A cap was measured rather than assumed and
  is not worth having: the median mask in the packs is 350 px, so capping the
  long side at 512 saves 6% of the bytes and capping it at 256 flips eleven
  build-up verdicts and takes one mask below the strength at which eight-bit
  coverage can accumulate at all. Ten masks at full size are 295 kB; measured
  once at fifteen masks and 624 kB, the release binary grew by 664 kB, so a mask
  costs the binary about what it costs the directory.
- **The generator owns the pack half of the directory.** It deletes the masks a
  previous run left behind, so a brush that stops shipping cannot leave a
  megabyte in the binary that nothing references, and it rewrites `tip_table.rs`
  itself rather than depending on `build-bitmaps` being run afterwards.

Adding a stamp of Umber's own is dropping an 8-bit greyscale PNG into that
directory and re-running `cargo run -p umber-core --example build-bitmaps`,
which rewrites the table from the listing. The table *is* the listing, so a file
that is not there is not in the binary and one that is cannot be forgotten. Both
generators write it, through the same `examples/common/table.rs`, so either can
be run on its own.

**Which packs' masks may be shipped is a separate decision from which packs may
be converted**, and a stricter one: converting a stamp is describing somebody's
work and shipping its mask is redistributing the work itself, in every release
on every platform. `Pack::ship_tips` in the generator is where that is recorded
per pack. See `docs/brush-sources.md`.

## Not done yet

- **rubberduck's stamps in the shipped library.** Three packs' stamps ship now
  and this one's do not, on the licence rule rather than on size or on
  machinery: its CC0 is declared on the OpenGameArt submission page and nowhere
  inside the download. It is 17 brushes and 1.2 MB. All 269 import today. See
  `docs/brush-sources.md`, which has the measurement and the one line that
  reverses it.
- **Picking a cell per dab**, which is what would make a `.gih` a brush rather
  than five. **`docs/brush-pipes.md` is the design, the measurements and the
  order to build it in.** The short version: the tip binding becomes a
  `texture_2d_array` and the dab instance carries an index, chosen by the same
  seeded RNG that already drives scatter and angle jitter — and the thing that
  actually stands in the way is neither memory (the largest cell array in the
  packs is 1221 kB against a canvas-sized scratch) nor the shader (a cell index
  is a third coordinate on one `textureSample`), but what a `TipMask` *is*. A
  pipe has to be one mask of several cells, because `Brush` is `Copy` and
  `BrushPreset::tip` holding one name is what lets two brushes share one upload
  — and that changes the meaning of "the mask" for the tip canvas, the library
  writer, the shipped table, `stroke_coverage` and the row preview. It should
  follow the colour plane through that file rather than race it. The prize is
  the *import* rather than the shipped count: 252 of the 257 presets refused for
  sequencing alone are rubberduck's, whose masks are not redistributed either
  way, so the library gains about five and somebody who owns that pack gains 252
  faithful stamps.

  **`angular` stays unbuilt on purpose**, and the measurement is why: not one
  pipe in any fetched pack is angular — 55 pipes, 51 `random`, 4 `incremental`.
  A single cell plus `dab_angle_follows_stroke` would reproduce one *better*
  than the file describes it, continuously rather than in `rank` steps, and
  there is nothing to check that against.
- **A wider grain model, and the papers the packs actually ask for.** A paper of
  your own now exists — `BrushPreset::paper` names a tile in the user library —
  so this is no longer about somewhere to put a picture. One gap remains:
  Krita's `.kpp` papers are not resolved at all — what a `.kpp` names is a
  pattern resource in a sibling bundle, the shape `.kpp`'s tips already solved.
  (A Clip Studio paper used to arrive as the material's **thumbnail** rather
  than its own pixels; `paper_for` now takes the same `csmaterial` route
  `tip_for` does.)

  The shipped library is what makes it worth doing: 31 Krita presets ask for a
  paper and 11 of them would otherwise ship. The store was **necessary and not
  sufficient** — the patterns are all available, 20 embedded in their presets
  and 11 in Revoy's bundle, and the licences are verified inside the downloads,
  yet only 7 of the 31 are the plain multiply `Brush::grain` performs. Four of
  the eleven come back with the store alone (two exactly, two once a stated
  brightness is baked into the stored tile); the other seven need Krita's
  Subtract mode, an inverted pattern or a cutoff remap, in that order of
  frequency. See "The paper texture, and what a texture library would buy".
- **A bundle where *every* preset is refused still reports nothing.**
  `brushimport::refusals(path)` now carries a bundle's foreign-engine refusals
  out to the generator, which is why its table reports thirteen rather than
  seven — but a bundle that fails whole gets no further than `read_file`'s
  error, which states the count and one reason. Widening what `read_file`
  returns would close it and touches all nine arms and every caller.
- **Importing a picture *as* a colour stamp.** The engine carries one and the
  library stores one, and the only doors into it today are the file formats that
  say so in their own bytes — a `.gbr` at four bytes a pixel, a `.gpb`'s
  pattern, an RGBA tip already in `tips/`. `TipMask::from_picture` deliberately
  keeps reading a picture as coverage, because a black-on-transparent PNG is
  overwhelmingly a mask and reading its colour would silently change what every
  such brush paints. What is missing is the *control* — a choice offered at the
  import, beside the sentence that already names which reading was taken.
- **Drawing a colour stamp on the canvas.** A tip document is transparent and
  `TipMask::from_alpha` takes its alpha and discards the colour, which is the
  rule that makes what you paint what stamps. Keeping the colour is now
  expressible and is a second question for the same control as above: the
  canvas would have to say which of the two it was for, since a mask drawn in
  red and one drawn in black must stay the same stamp.
- **A row's sample ignores the modulation table.** `widgets::brush_sample` is a
  miniature dab loop of its own rather than a `StrokeBuilder`, so a brush whose
  ellipticity is thrown per dab draws its row as though it were not. Fixing it
  properly means the sample driving a real stroke builder — which would also
  give it speed and stroke position, neither of which a static row has. A stamp
  brush's row *does* show its mask; it is the per-dab variation that is missing.
- **`lock_alpha`, `colorize` and `posterize`.** All three change how a dab
  *composites*, so they belong to `commit.wgsl` — and every change there has to
  be made identically in `composite.wgsl` or the stroke jumps at pointer-up.
  `lock_alpha` additionally needs the layer's own alpha read as a mask, which
  the stroke scratch has no channel for. No brush in the shipped pack sets any
  of them to a live value, so nothing in the library is waiting on them; they
  are worth building as painting features in their own right, `lock_alpha`
  especially, rather than as import fidelity.
- **`custom_input`**, and with it the last third of MyPaint's own mappings. See
  "What a MyPaint `.myb` conversion loses".
- **Krita's other paint engines** — `spraybrush`, `hairybrush`, `deformbrush`,
  `experimentbrush`, `hatchingbrush`, `roundmarker`. A `.kpp` written by one is
  refused by name rather than approximated; between them they account for 13 of
  the 116 presets in the fetched Krita packs.
- **Krita's masking brush, mirrored dabs and impasto**, and its brush-tip
  randomness and density. All are reported when a preset asks for one. Its
  paper texture is a near miss and a narrower one than it was: Umber can now
  hold a bitmap paper of its own, and what a `.kpp` names is a pattern resource
  in a bundle beside it — so this is a resolution problem rather than a missing
  engine feature, of the shape `.kpp`'s tips already solved.
- **Photoshop's brush descriptors.** A `.abr` brings its stamps; its spacing,
  angle, roundness and scatter are in a nested binary descriptor section that
  would be a second format inside the first.

## What a user can set

The brush editor has five sections, following the design's naming where it has
a name for the thing:

| Section | Holds |
|---|---|
| Tip | size, hardness, opacity, spacing, airbrush rate, roundness, angle, angle-follows-stroke, stabilisation |
| Dynamics | pressure source, and pressure → size / opacity / hardness with their curves and floors |
| Inputs | the modulation table — target, input, both ends of its range and its curve — plus the stroke ramp and hold |
| Scatter | scatter, size jitter, angle jitter, speed lead, pressure → scatter |
| Texture | build-up, paper strength, which paper — the shipped three or one of yours — and tile size |
| Blending | blend mode, colour pickup, smear length, pickup radius |

That is every field of `Brush` except `mode`, which is the tool choice (Brush
or Eraser) rather than a brush setting, and is on the tool rail.

Build-up and the paper share a section deliberately. Both are about a mark made
of many faint stamps rather than one solid one: grain is what makes it faint,
and build-up is what lets a second pass make it darker. A textured brush without
build-up paints one pass and then stops responding, which is the surprise the
pairing avoids.

**Wet edges** is the one section of the design's six still not drawn at all
rather than drawn empty; it has no engine behind it. **Stabiliser** is one
slider and rides on Tip — and is on the tool options strip as well, as a third
rail beside size and opacity, because it is the one brush setting a painter
adjusts while a line is being drawn. Two of the six names are our own, and both were needed
because the design has no word for the thing:

- **Blending**, for colour pickup and the brush's blend mode. Filing it under
  "Wet edges" would have borrowed a term that means something else in every
  application that has it.
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
