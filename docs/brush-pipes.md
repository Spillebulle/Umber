# A cell per dab: what a GIMP brush pipe would take

`.gih` is the largest single refusal in Umber's shipped brush library, and it
is the one refusal whose fix is an engine change rather than an importer
change. This is the design for that change, the measurements it turns on, and
the reasons it is not built yet. It is written before the work for the reason
`docs/layer-folders.md` was: the shape of it is not obvious, three of its
pieces belong to files several other things also want, and a first draft nobody
argued with would be wrong.

Nothing here is built. `crates/umber-core/src/brushimport/gih.rs` is what
exists, and it does the two things that *can* be done without touching the
engine.

## The numbers, and where they come from

`cargo run --release -p umber-core --example measure-pipes`, over the packs
`tools/fetch-brushes.*` fetches. **Re-run it before quoting any of these** —
the same rule `measure-undo.rs` and `measure-history.rs` state for theirs.

| | |
|---:|---|
| 55 | pipes: 44 loose `.gih`, 11 inside the two Krita bundles |
| 311 | cells between them |
| 51 | walk their cells at **random** |
| 4 | walk them **incrementally** |
| 0 | **angular** — the one rule Umber's dab could reproduce natively |
| 0 | with more than one dimension |
| 9 | cells in the widest pipe (`scratches_rough.gih`, 300² each, 791 kB) |
| 1221 kB | the largest cell array, every cell padded into the common box (`painted-style.gih`, 5 × 500²) |
| 0 | pipes whose cells differ in size — legal, and none of them does it |
| 2 | pipes whose cells are all **the same brush**, so nothing is lost collapsing them |
| 43 / 252 | of those pipes and cells are rubberduck's |

The library generator (`cargo run --release -p umber-core --example
build-brush-library`) prints the other half: **257 presets refused for a `.gih`
pipe's sequencing and nothing else**, out of 267 that touch one at all, against
233 shipped. 252 of the 257 are rubberduck's — the row above is where that
number comes from — and rubberduck's masks are not redistributed
(`docs/brush-sources.md`), so they would not ship even with the sequencing
solved.

That asymmetry is the whole shape of the problem. **The shipped library gains
about five brushes. An import gains 252 faithful stamps** — for somebody who
owns rubberduck's pack and brings it in themselves. Anyone weighing this work
should weigh the import, not the count on the tin.

## What a pipe is, and what Umber does today

A pipe is several `.gbr` cells concatenated behind a one-line text header, plus
a rule for choosing between them per dab. Umber binds **one tip per pass** —
that is what keeps a thousand tipped dabs a single draw call — so a pipe cannot
arrive as one brush that rotates through five stamps. It arrives as one preset
per cell, `Bark 1` … `Bark 5`, each saying which rule it lost.

That loses no pixels and it does lose the mark: a stroke that picks a cell at
random per dab does not look like any one of its cells repeated. It is also
269 loose brushes in the picker where rubberduck drew 60.

Two collapses *are* built, and they are built because each is decided from the
file rather than from a judgement about what the stamps depict:

- **Nothing walks.** Every dimension states `constant`, so GIMP paints the
  first cell for ever and the rest are unreachable. Read off the header.
- **The cells cannot differ.** Every cell is the same brush, so choosing
  between them makes exactly the mark one of them repeated makes. Compared byte
  for byte. Two of the 55 are this.

Neither has a threshold in it. That is the line the third case falls the wrong
side of.

## `angular`, and why it is not a shortcut

`sel0:angular` is the one rule that is not a shuffle: it picks the cell by the
*direction of the stroke*, so the cells are one mark drawn at `rank` rotations
and painting a curve turns the stamp through them. `Brush::dab_angle_follows_
stroke` turns the quad and its tip with it, continuously rather than in `rank`
steps — so one cell plus that flag would reproduce such a pipe *better* than
the file describes it, not approximate it.

It is not built, and the reason is not that it would be hard. Collapsing to
cell 0 is right only if the other cells really are cell 0 rotated, which is what
`angular` *means* and not what the file *says*. A pipe of unrelated pictures
walked angularly would lose every stamp but one, in silence. Deciding it from
the pixels — rotate cell 0 by `i × 360/n` and compare — needs a CPU resampler
this crate deliberately does not have, and a similarity threshold that would
have to be calibrated against real files.

**There are no real files.** Not one pipe in any fetched pack is angular. So
the check could be written and could never be tested, and the shipped library's
rule is that nothing goes out under an author's name that paints unlike their
brush. If an angular pipe ever turns up, this is the first thing to build and
the measurement above is where to notice it.

## The change: a cell per dab

The dab pass would hold an **array** of cells and each dab instance an index
into it, chosen by the seeded per-stroke RNG that already drives scatter and
angle jitter. Every fetched pipe picks at random or in order, so `selN:` would
not have to be honoured in full — `random` and `incremental` are one attribute
apart, and the four rules nobody uses can stay unreproduced and named.

Five pieces, in the order they constrain each other.

### 1. `TipMask` gains cells, and a pipe is one mask

The temptation is `BrushPreset::tip: Vec<String>`. That is the wrong shape, and
`Brush` is what says so: it is `Copy`, so nothing on it can be a `Vec`, and
`BrushPreset::tip` holding a name rather than a picture is what lets two
brushes share one stamp and one GPU upload.

So a pipe is **one mask holding several cells**: `TipMask` gains a cell count,
its coverage is the cells end to end, and `TipMask::aspect` answers for one
cell rather than for the sheet. `BrushPreset::tip` stays a name and stays one.
On disk it is one greyscale PNG — a vertical strip, so a row of the file is a
row of a cell and the encoder's filters see what they expect — and the cell
count rides in the file rather than beside it, because a mask whose cell count
lived in the `.ron` would be two files that have to agree.

**`TipMask::MAX_SIZE` has to become a cap on the *cell*, and the strip has to
stay the disk form.** This is the piece it is easiest to get to the point of a
compile before noticing. `MAX_SIZE` is 2048 because `downlevel_defaults`
guarantees exactly that for `max_texture_dimension_2d`, and `TipMask::new`
refuses either axis over it. As a vertical strip, `scratches_rough.gih` is 9 ×
300 = 2700 rows, `blocky.gih` is 2800 and `painted-style.gih` is 2500 — so most
of the pipes in the packs, including the two this document quotes as its
headline figures, would be refused by the very constructor they are meant to
go through. The cap belongs on one cell's dimensions, with the layer count
checked against `max_texture_array_layers` separately; the strip is how the
bytes sit in a file and in memory and is never a texture. That change to
`MAX_SIZE`'s meaning is part of the cost in "What it costs", not a detail below
it.

A pipe whose cells differ in size is legal, and would be padded into the common
box on the way in, because an array's layers share one size. Padding is not
resampling: the empty margin is coverage of zero, which is the exact identity,
and it is what `TipMask::aspect` already replaced for the *outer* shape. **No
pipe in any fetched pack is ragged** — `measure-pipes` marks one with a `*` and
has never printed one — so this is the case with no evidence behind it, and it
should be built from the format's own permission rather than from a file
somebody can point at.

This is the piece that costs the most elsewhere. `tip.rs` is read by the brush
editor's tip canvas, `UserLibrary::save`, `tip::builtin`, `stroke_coverage` and
`widgets::preview_dabs`, and every one of them means "the mask" where it would
now have to mean "a cell of the mask".

### 2. The dab pass binds a `texture_2d_array`

`dab.wgsl` samples `tip: texture_2d<f32>`. It becomes `texture_2d_array<f32>`
and the instance carries a `cell: u32` alongside `angle` and `aspect`. Layers
of an array texture share one size, which is what the padding in (1) is for.

Nothing else in the shader moves. The tip modulates coverage and touches no
blend state, so the `max`, build-up, the selection clip and stroke opacity
applied once at commit are all untouched — a cell index is a third coordinate
on one `textureSample` and nothing downstream learns there was one.

There are four dab pipelines, from two independent binary choices, built by one
loop over one descriptor. This adds no fifth: the array binding replaces the 2D
one for **every** pipeline, with a one-layer array standing in for an ordinary
tip. That is deliberate — a fifth pipeline would be a second code path for the
common case, and `use_tip` already shows how a placeholder keeps one bind group
layout.

### 3. `set_tip` keeps its `Arc` identity check exactly

`set_tip` is called from `start_stroke` and nowhere else, and skips the upload
when the mask is the same `Arc` — identity, not equality, because comparing a
megabyte of coverage per stroke would put back the cost the check exists to
avoid. **None of that changes.** A pipe is one `Arc<TipMask>` like any other
mask, so it is one comparison and one upload, and the array is built once per
stroke rather than per frame.

The rule that a tip may not change mid-stroke also stands, and now covers more:
what varies per dab is the *index*, which is a vertex attribute, not a binding.
Rebinding a texture mid-pass would still restamp what is already in the scratch.

### 4. The index is drawn from the stroke's own RNG

`StrokeBuilder` seeds its RNG per stroke, never from the clock, so a stroke
redraws identically and undo/redo reproduces the same marks. A cell index is
one more draw from that stream, and it is subject to the rule every draw here
is subject to: **guarded by the setting that reads it.** A brush with one cell
must draw nothing, or every other feature's numbers reshuffle and
`a_brush_with_no_new_dynamics_emits_exactly_what_it_used_to` fails.

`incremental` is a counter rather than a draw, and it must not touch the stream
at all for the same reason.

### 5. The importer stops splitting

`gih.rs` builds one `TipMask` of `n` cells instead of `n` brushes, and
`sequence_losses` empties for `random` and `incremental`. `Selection` is
already the enum that says which rules are honoured; the four that are not —
`velocity`, `pressure`, `xtilt`, `ytilt` — keep the general sentence, and
`angular` keeps its own.

## What it costs

**Memory is not the obstacle, and this is worth stating because it reads like
it should be.** The widest pipe in the packs is 9 cells; the largest cell
array, padded, is **1221 kB**. The stroke scratch is canvas-sized — 100 MB on a
10000² canvas — so a pipe's cells are a rounding error beside the texture the
dab pass already read-modify-writes per fragment.

The limits are the thing to check rather than the megabytes.
`max_texture_array_layers` is **256** under `downlevel_defaults`, and
`using_resolution` raises only the three `max_texture_dimension_*` fields, so
`gpu.rs` does not lift it — against a measured widest of 9 and a `MAX_CELLS` of
1024, so **the cell count needs a cap the array can hold**. And
`TipMask::MAX_SIZE`, today a cap on the whole mask, has to become a cap on one
**cell**, for the reason §1 gives. Those two are the new limits this
introduces, and neither is a size somebody runs out of.

**The shipped library gains about five presets, and that is the smaller half of
the reason to do it.** 252 of the 257 refused for sequencing alone are
rubberduck's and are refused for their masks regardless, so the generator's
total moves by roughly the five that are not. What changes is the *import*:
269 of rubberduck's stamps arriving as 60 brushes that paint like rubberduck's,
instead of 269 that do not.

(The `.gih` work that has already landed — the two exact collapses — moves the
total by **nothing**. It took a false sequencing notice off four of David
Revoy's presets, and every one of the four is still refused for something else.)

The cost that is real is **`tip.rs`'s contract**, and it is the reason this is
a document rather than a branch. A mask that is a sheet of cells is read
differently by the tip canvas, the library writer, the shipped table, the
coverage measurement and the row preview, and each of those is a place where
"the mask" quietly means "one cell". A colour plane is landing in the same
struct. Two changes to `TipMask`'s meaning at once, in a file three other
pieces of work also touch, is how a merge produces something nobody designed.

## What to do first

1. **Nothing, until `tip.rs` is quiet.** This is a change to what a `TipMask`
   *is*, and it should follow the colour plane rather than race it.
2. Then (1) and (2) together, with an ordinary one-cell tip going through the
   array path from the first commit — so the common case is the tested case
   rather than a branch beside it.
3. Then (4) and (5). The importer is the smallest piece and the last one: until
   the engine can paint a pipe, splitting it into cells is still the reading
   that loses no pixels.

## What would move the numbers, and is not this

- **rubberduck's masks shipping.** One field in the generator (`Pack::ship_tips`)
  and a licence decision that is not a technical one — `docs/brush-sources.md`
  has it. With the sequencing solved *and* that flipped, the shipped library
  gains 60 stamp brushes rather than five. The two are independent and the
  licence one is the larger.
- **`.gpb` colour.** Four of David Revoy's presets stopped reporting a
  sequencing loss when the identical-cell collapse landed, and every one of them
  is still refused for colour. Two of the four are refused for colour *alone*.
