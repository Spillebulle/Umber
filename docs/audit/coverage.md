# Coverage audit: guards that do not guard, and code nothing drives

A read-only sweep of the workspace against the failure catalogue in `CLAUDE.md`'s
"Testing" and "Partial exhaustiveness is worse than none" sections. **Nothing
here was built or run** — every entry is a mutation a fix agent can run, with the
result this audit predicts and the consequence if the prediction holds.

Read the two tables first. The evidence below each is what a fix agent needs to
act without re-deriving the reasoning: a file, a line, the edit to make, and what
should be expected to fail.

## Part (a) — suspect guards

| # | Rank | Guard | Subject it claims | Mutation |
|---|---|---|---|---|
| A1 | **BLOCKING** | `umber_shellext::tests::an_opaque_pixel_survives_the_premultiply_exactly` | the premultiply, the BGRA swap and the top-down flag in `to_bitmap` | drop the BGRA swap; flip `biHeight`'s sign; swap `biWidth`/`biHeight`; drop the `+ 127` |
| A2 | SUBSTANTIVE | `docimport::preview::tests::fitting_never_enlarges_and_keeps_the_proportions` | `Preview::fit_within` fits **both** edges inside the box | `w.max(h)` → `w` at `preview.rs:90` |
| A3 | SUBSTANTIVE | `gpu_pipeline::writing_a_slice_moves_its_revision_and_leaves_the_others_alone` | "**every** route that writes a slice moves that slice's counter" | delete the `touch_slot` loop in `flip_layers`, `canvas.rs:6513–6515` |
| A4 | SUBSTANTIVE | `docformat::history::tests::a_history_that_cannot_be_placed_is_dropped_rather_than_replayed` (and the reader's other refusals) | `docimport::history::load`'s eleven refusal branches | delete the per-piece bounds check, `docimport/history.rs:233` |
| A5 | MINOR | `export::tests::a_format_only_admits_to_what_it_actually_costs_this_document` — trailing loop | `needs_matte` | none needed: the loop is `needs_matte`'s body verbatim |
| A6 | MINOR | `docimport::preview::tests::an_ora_thumbnail_comes_out_of_the_merged_image` | the merged-image reader's size | transpose the `(width, height)` read out of `mergedimage.png` |

## Part (b) — unguarded surface

| # | Rank | Code | Rule it carries | Nothing drives it because |
|---|---|---|---|---|
| B1 | **BLOCKING** | `exportdlg::show` (`exportdlg.rs:108`) | "every loss is named *before* the write"; the matte and quality gates | no `ctx.run_ui` test in the module at all |
| B2 | **BLOCKING** | `to_bitmap`'s channel order and row order (`umber-shellext/src/lib.rs:287`) | what Explorer draws for every `.ora`/`.kra`/`.psd`/`.clip` | the one COM test reads no pixel and its fixture is flat and square |
| B3 | SUBSTANTIVE | `DabTarget::is_colour` (`dynamics.rs:215`) | decides `StrokeStyle::per_dab_color` for a whole stroke | `matches!` over a 10-variant enum; **no test calls it** |
| B4 | SUBSTANTIVE | `ExportFormat::carries_alpha` / `has_quality` / `quantises` (`export.rs:140–151`) | which losses an export admits to | three `matches!` where `extensions()`' exhaustive `match` makes the compiler *look* like it has your back |
| B5 | SUBSTANTIVE | the three `vram::*_refused` call sites (`app.rs:2223, 2446, 3913, 4625`) | that a card that refuses storage produces a sentence and not a crash box | **self-admitted** at `vram.rs:45–53` |
| B6 | SUBSTANTIVE | `docimport::history::load`'s bounds and shape checks (`docimport/history.rs:163–246`) | a patch never replays into pixels it does not name | 0 tests in the module; the four branches that *are* reached never read `reason` |
| B7 | MINOR | `Grid::tiles_over`'s canvas clip (`tile.rs:318`) | a copy never names a tile that is not there | its guard drives only rectangles wholly inside the canvas; its doc claim "no caller outside the tests" is now false (10 callers) |
| B8 | MINOR | `Loading::fraction` / `Loading::detail` (`loading.rs:135, 141`) | what the open dialog says | both tests drive `pack`/`unpack` only |
| B9 | MINOR | `Preview::fit_within`'s fallback (`preview.rs:100–108`) | `Preview`'s size-matches-buffer invariant | the branch is called unreachable and bypasses `Preview::new` |

---

# Evidence

## A1 / B2 — the Explorer thumbnail (BLOCKING)

`crates/umber-shellext/src/lib.rs`. This is the strongest finding in the sweep:
it is CLAUDE.md's headline failure ("a guard that restates the panel's own rule
inside the test can only agree with itself") committed verbatim, inside `unsafe`
code, on a path an artist sees every time they open a folder.

**The guard restates its subject.** `an_opaque_pixel_survives_the_premultiply_exactly`
(line 451) is:

```rust
let pre = |c: u8, a: u8| ((u32::from(c) * u32::from(a) + 127) / 255) as u8;
for c in [0u8, 1, 127, 128, 254, 255] { assert_eq!(pre(c, 255), c, …); }
```

`pre` is a closure **declared in the test**. It is a copy of the closure at
`lib.rs:325` and the test asserts about the copy. The production line is never
executed. The doc comment above it says "A preview becomes a bitmap of the same
shape, premultiplied, top-down" — the test does none of those three things.

**The only test that runs `to_bitmap` reads no pixel.**
`a_stream_of_a_document_becomes_a_bitmap` (line 528) drives the real COM
sequence, then asserts exactly `bmWidth == 32`, `bmHeight == 32`,
`bmBitsPixel == 32`. Its fixture is `document(64)`, which is `size × size`
(square) and `[10, 200, 60, 255].repeat(...)` (one colour everywhere). Both
shapes are named in CLAUDE.md as the ones that hide a class of bug — squareness
hides a transposition, a flat buffer hides an index bug.

So every one of these is unguarded:

| `lib.rs` | line | mutation | what ships |
|---|---|---|---|
| BGRA swap | 326–328 | `dst[0] = pre(src[0]); dst[2] = pre(src[2]);` | red and blue swapped in every Umber thumbnail |
| top-down flag | 300 | `biHeight: height as i32` | every thumbnail upside down |
| axes | 296/300 | swap `biWidth` and `biHeight` | a square fixture cannot see it |
| rounding | 325 | `+ 127` → `+ 0` | every thumbnail a level dark; the test's own comment says "nothing would ever say so" |

**Expected result of all four: all 11 tests in `umber-shellext` green.**

The fix is one test, and it needs no desktop for the arithmetic half:
build a `Preview` that is **non-square** and whose four corner pixels are four
different colours with an alpha that is not 255, call `to_bitmap`, then read the
DIB back through `GetObjectW` + `GetBitmapBits` (or `GetDIBits`). The corner
check is what settles orientation; distinct channels are what settle BGRA. Gate
the GDI half the way `an_opaque_pixel_survives_the_premultiply_exactly` says it
wanted to be gated, but do not let the gate become a second closure.

## A2 — `fit_within` has no portrait fixture

`crates/umber-core/src/docimport/preview.rs:82`:

```rust
let scale = f64::from(max_edge) / f64::from(w.max(h));
```

Every fixture that reaches it is landscape or square:

| fixture | where | shape |
|---|---|---|
| `4 × 2` | in `fitting_never_enlarges_…`, `preview.rs:362` | landscape |
| `1920 × 1080` | same test | landscape |
| `2500 × 625` | same test | landscape |
| `10000 × 5` | same test | landscape |
| `CLIP_PREVIEW` `12 × 6` | `fixtures.rs:1534` | landscape |
| ORA merged `8 × 8` | `preview.rs:314` | square |

There is no portrait `Preview` anywhere in the crate.

**Mutation:** `w.max(h)` → `w` at line 90. **Expected: green.** Also
`if max_edge == 0 || (w <= max_edge && h <= max_edge)` at line 84 → drop the
`&& h <= max_edge`; **expected: green** (the `4 × 2` case still returns early on
`w`).

**Consequence:** a portrait document — an A4 page, a phone screenshot, most
sketches — is fitted to `max_edge` *wide* and up to `max_edge × h/w` tall, i.e.
larger on its long axis than the box Explorer asked for. Add one
`Preview::new(1080, 1920, …)` case asserting `fit_within(256).size ==
UVec2::new(144, 256)` and one `600 × 1200 → 128 × 256` and both mutations die.

## A3 — "every route" is three routes

`crates/umber-render/tests/gpu_pipeline.rs:6413`. The doc comment reads
"**Every** route that writes a slice moves that slice's counter and no other";
`crates/umber-app/src/thumbs.rs:5` repeats the same claim ("the renderer bumps in
**every** method"). The test drives three: the ordinary commit, `write_layer_rect`
and `clear_layer`.

`self.touch_slot(...)` appears at `crates/umber-render/src/canvas.rs` lines
5909, 5916, 6036, 6417, 6439, 6514, 7194, 7221, 9799, 10230, plus
`touch_all_slots` at 5063 and 6451. Of those, only 6036 (commit), 6417
(`clear_layer`) and 10230 (`write_layer_rect`) are reached by that test, and 7194
is separately held by `a_dragged_float_carries_the_effect_derived_from_it`.

**Mutation (best):** delete the loop at `canvas.rs:6513–6515`:

```rust
for &slot in slots {
    self.touch_slot(slot);
}
```

inside `flip_layers`. **Expected: green** — `a_flip_mirrors_the_canvas_and_flipping_twice_restores_it_exactly`
and `a_flip_mirrors_a_sparse_layer_…` both read pixels and neither reads a
revision.

**Consequence:** after a canvas flip every layer thumbnail in the panel goes on
showing the picture in the old orientation until something else writes that
slice, and `EffectCache`'s `source_revision` (`canvas.rs:8597`) does not move, so
a baked drop shadow is not re-derived from mirrored pixels.

**Second mutation:** delete `self.touch_slot(slot)` at `canvas.rs:6439`
(`fill_layer_white`). `a_new_mask_stores_nothing_and_reveals_everything` reads
pixels, not revisions; `mask_revision` at `canvas.rs:8598` keys the effect cache.
Expected: green.

This is the exact recurrence CLAUDE.md predicted at the end of the `slot_revision`
bullet — "a rule enforced inside N methods still needs somebody to check that N is
all of them". The honest repair is to widen the guard to every writer, or to
narrow both doc comments to name the three that are driven.

## A4 / B6 — the saved-history reader

`crates/umber-core/src/docimport/history.rs` is 296 lines with **0 `#[test]`**.

Four of its eleven refusals *are* reached, from
`docformat::history::tests::a_history_that_cannot_be_placed_is_dropped_rather_than_replayed`
(`docformat/history.rs:976`): a renamed layer, a resized canvas, a newer manifest
revision, a missing patch entry. But that test and its sibling at line 1346 both
assert `matches!(w, ImportWarning::HistoryDropped { .. })` — **the `reason` is
never read anywhere in the workspace.** A scan for each refusal's own sentence
finds no test that names it.

The branches nothing reaches at all:

| `docimport/history.rs` | refusal |
|---|---|
| 144 | `position` past the entry count |
| 163 | a structural entry (its own comment says removing it turns a merely-newer file into a corruption diagnosis) |
| 172 | a flip entry carrying pixels |
| 183 | an entry naming a layer the document does not have |
| 188 | the entry rectangle running off the canvas |
| 233 | **the per-piece rectangle running off the canvas** |
| 244 | a PNG that is not the size the manifest says |

**Mutation:** delete the piece bounds check at lines 233–239. **Expected: green.**
That check is the one standing between a malformed or hostile `.ora` and a
`write_layer_rect` past the texture, which is a wgpu validation error and fatal.

Note also that the fixture's canvas is **64 × 64** (`docformat/history.rs:987`
patches `"canvas":[64,64]` to `[65,64]`), so the `x`/`w` and `y`/`h` halves of
both bounds checks are indistinguishable under transposition — the fixture-shape
trap again.

## A5 — a loop that is its own subject

`crates/umber-core/src/export.rs:603`:

```rust
for format in ExportFormat::ALL {
    assert_eq!(needs_matte(format, true), !format.carries_alpha(), "{format:?}");
    …
}
```

`needs_matte` is `transparent && !format.carries_alpha()` (`export.rs:217`). The
assertion is the body. It can only agree with itself; the explicit
`losses(...)` assertions above it are what does the real work, so this is a
harmless-but-vacuous half rather than a live defect. Worth deleting or replacing
with something that measures.

(The two round-trip tests either side of it — `an_alpha_carrying_format_…` and
`an_alpha_less_format_…` — *do* measure output, and they partition `ALL` by
`carries_alpha()` itself, which is the right shape: a wrong answer for an
existing format lands the format in the wrong half and the encoder disagrees.
They do not protect a **new** variant; see B4.)

## B1 — the export dialog is drawn by nothing (BLOCKING)

`crates/umber-app/src/exportdlg.rs` has two tests
(`the_form_states_exactly_what_the_encoder_is_given`,
`a_quality_that_left_the_rail_is_still_one_the_encoder_accepts`) and both drive
`ExportForm::options()` alone. **`show` is never called.** The module is one of
only three in `umber-app` with a `pub fn` taking `&mut Ui` and no `ctx.run_ui`
test (`exportdlg`, `tabs`, `tweaks`).

What that leaves unguarded, all inside `show`:

- **`losses(ui, p, form.format, transparent)` at line 161.** This is
  CLAUDE.md's "every loss is named *before* the write" — the rule the whole
  `ExportLoss` type exists for. **Mutation: delete that line. Expected: green.**
  Consequence: a transparent document exported to JPEG or BMP is matted with
  nothing said, which is the "silently onto black" failure the module docs call
  the classic version of this bug.
- **`if export::needs_matte(form.format, transparent)` at line 156.** Mutation:
  `if true`. Expected: green. Consequence: a live matte control on a PNG export,
  which is the "a control that does nothing is worse than one that is not drawn"
  rule.
- **`if form.format.has_quality()` at line 143.** Same shape.
- **`let transparent = ed.doc.background == Background::Transparent` at line
  116.** Mutation: `let transparent = true;`. Expected: green. Consequence:
  every opaque document is warned that it will be flattened — the exact "a
  warning shown every time is one nobody reads" failure.

The idiom to copy is already in the crate: `canvasdlg.rs:1087` builds an
`egui::Context::default()`, calls `ctx.run_ui(...)`, and reads the galleys out of
the returned `FullOutput.shapes`. Asserting that the string "Nothing in this
document is lost by this format." is present for `(Png, true)` and absent for
`(Jpeg, true)`, and that the matte caption "Transparency becomes" appears only
when `needs_matte`, kills all four mutations.

## B3 — `DabTarget::is_colour`

`crates/umber-core/src/dynamics.rs:215`:

```rust
pub fn is_colour(self) -> bool {
    matches!(self, Self::Hue | Self::Saturation | Self::Value)
}
```

Two failures at once:

1. **`matches!` where a `match` would fail the build.** `DabTarget::label`
   (`:198`) and `DabTarget::range` (`:222`) *are* exhaustive matches, so adding
   an eleventh target fails the build there — and the developer fills those in
   and this answers `false` in silence. That is the "the compiler appears to have
   your back and only half does" shape verbatim.
2. **Nothing calls it under `cfg(test)`.** No test in `dynamics.rs` (14 tests)
   mentions `is_colour`, and its one consumer `ModulationTable::colours_dabs`
   (`:454`) is only reached through brush-import assertions in `mypaint.rs` and
   `clipstudio.rs`, which exercise Hue and Smudge and never sweep `ALL`.

`ALL` is a hand-written `[Self; 10]` (`:184`) with no exhaustive-match guard
beside it, which CLAUDE.md names as its own defect.

**Consequence if it goes wrong:** `Brush::colours_dabs` → `StrokeStyle::per_dab_color`
(`editor.rs:1626`). CLAUDE.md: "`StrokeStyle::per_dab_color` must match what
`draw_dabs` was told, for the whole stroke: turning it on midway leaves the
earlier dabs with no colour recorded." A colour-driving target that answers
`false` puts the whole stroke on the fast path and the colour modulation
silently does nothing.

**Fix:** make it a `match` with ten arms, and add an exhaustive-match test over
`ALL` in the shape CLAUDE.md prescribes (arms indexing `ALL`, not iterating it).

## B4 — three more `matches!` over `ExportFormat`

`crates/umber-core/src/export.rs`:

```rust
pub fn carries_alpha(self) -> bool { matches!(self, ExportFormat::Png | ExportFormat::Tiff) }   // :140
pub fn has_quality(self)   -> bool { matches!(self, ExportFormat::Jpeg) }                        // :145
pub fn quantises(self)     -> bool { matches!(self, ExportFormat::Gif) }                         // :150
```

`extensions()` (`:114`) is an exhaustive `match`, so adding a sixth format is a
compile error there and *only* there. The three above then answer `false`,
`false`, `false`, and `losses()` reports the new format as lossless — a format
that cannot carry alpha exports a transparent document with no matte warning.
`losses()` also carries a fourth spelling of the same question,
`if format == ExportFormat::Jpeg` at `:206`, beside `has_quality`'s.

Not a live defect — WebP is explicitly refused and no sixth format is planned —
but it is four statements of "which format is which" where one exhaustive
`match` returning a small struct would be forced. Cheap to close; rank it
against how likely a sixth format is.

## B5 — the VRAM refusals' call sites

`crates/umber-app/src/vram.rs:45–53` states the gap itself:

> The three call sites — `install_import`, `add_layer` and `add_mask` — are
> guarded by nothing: delete the reservation from any one of them and the whole
> suite stays green while that path goes back to producing the crash box.

Collected here because it is exactly the shape of items 2 and 3 in the remit and
because it has since grown a **fourth** site the comment does not name:
`vram::effect_refused` at `app.rs:3913`. The four are `app.rs:2223` (add layer),
`:2446` (add mask), `:3913` (refused bake), `:4625` (open refused). The
reachability argument is honest — an `UmberApp` needs an `EventLoopProxy` — but
the comment should be widened to four, and `umber-render`'s
`a_bake_refused_its_page_draws_the_layer_and_reports_once`
(`gpu_pipeline.rs:7570`) is worth checking against the `effect_refused` claim
that `EffectCache::refusing` latches.

Related, and the same honesty: `canvas.rs:4502` records that passing `None`
instead of `Some(&self.layers)` to `try_reserve` "leaves every test in the
workspace green while every growth refusal understates by the whole of the
document already on the card". That one is structural rather than tested and says
so.

## B7 — `Grid::tiles_over`

`crates/umber-core/src/tile.rs:311–333`. Two things.

**The doc claim is stale.** It says "there is no allocator yet, so this has no
caller outside the tests". There are now ten: `app.rs:4589`,
`canvas.rs:6043, 6801, 10268`, and three examples. CLAUDE.md's own rule — "a doc
comment that names a call site is a claim" — applies.

**Its clip is unguarded.** `fragments` clamps *both* ends (`x0 = rect.x.min(doc_size.x)`,
`x1 = … .min(doc_size.x)`) and has `nothing_outside_the_canvas_is_a_fragment` to
drive it. `tiles_over` clips only the far end, and its guard
(`tiles_over_names_the_same_tiles_the_fragments_do`, `:505`) drives three
rectangles all wholly inside the canvas.

**Mutation:** drop `.min(self.doc_size.x)` and `.min(self.doc_size.y)` at
`tile.rs:319–320`. **Expected: green.** Add an overhanging rectangle to that
test's list — `rect(900, 700, 400, 400)` on the 1000 × 800 grid — and it dies.

## B8 / B9 — smaller

- `loading.rs`: `fraction` (`:135`) and `detail` (`:141`) are undriven; both
  tests cover `pack`/`unpack` only. `detail`'s `(done + 1).min(total)` → `done.min(total)`
  is green and produces "Layer 0 of 64". `Loading::start`'s thread-and-wake
  wiring is undriven, which is understandable (it spawns a real thread) and
  worth a note rather than a test.
- `preview.rs:100–108`: the `from_raw` fallback returns
  `Self { size: UVec2::new(w, h), rgba: Vec::new() }` — a `Preview` whose size
  disagrees with its buffer, which is the one invariant `Preview::new` exists to
  enforce. It is called unreachable and it is; but `to_bitmap`'s
  `chunks_exact_mut(4).zip(...)` would silently truncate rather than refuse.
  Returning `Err` costs nothing.

---

# What is already at the bar

Worth recording, because knowing which parts do not need looking at is part of
the answer:

- **`docformat`'s streamed save.** `BothWays` (`docformat/mod.rs:4507`) is the
  best fixture in the workspace: no two layers alike (an index bug writes a
  different archive), one layer's content off-origin and non-square (a
  transposed `Encoded::at` is caught), and a mask whose three channels differ (a
  reader that took green instead of red is caught). Each of those three
  properties carries a comment naming the mutation that proved it necessary.
- **`umber-core::tile`.** Ten tests, `fragments_cover_a_rectangle_exactly_once`
  sweeps a 700 × 600 grid and counts every pixel, `an_entry_packs_the_way_the_shader_unpacks_it`
  pins the shifts rather than a round trip, and the `MAX_TILES_PER_AXIS` sweep is
  over real adapter figures. Only `tiles_over` (B7) falls short.
- **The tile atlas's GPU guards.** `TILED_W`/`TILED_H` at 700 × 500 are
  deliberately non-square and not a whole number of tiles, and the comment above
  them records both mutations that forced each property. `every_atlas_cell_is_held_by_one_slot_or_is_free`
  is the set-level invariant CLAUDE.md describes.
- **`docimport::residency`.** `place`'s `axis` closure is applied per axis, so a
  transposition is not expressible, and `the_scanned_region_is_the_one_the_tiles_were_charged_for`
  drives an asymmetric case.
- **`docformat::mod::a_mask_patch_is_converted_or_not_by_the_revision_the_document_declares`.**
  The repair of the twin-call-site failure CLAUDE.md records, and it is a good
  one: one archive read twice with nothing between the two readings but the
  version attribute, an ordinary patch riding beside the mask patch so
  `entry.mask` is load-bearing, and a coverage of 128/188 rather than the
  transfer function's fixed points.
- **`ui.rs`'s options-strip budget** (`every_rail_on_the_strip_fits_the_budget_that_lets_it_be_drawn`)
  and **`canvasdlg`'s dialog guards** — both now read the shapes egui actually
  drew.
- **`umber-shellext`'s COM plumbing**, as distinct from its pixels:
  `the_dll_hands_out_the_class_it_is_registered_as`,
  `the_thumbnail_handler_clsid_matches_the_installer` (which reads the WiX rather
  than a copy of it), `a_null_out_parameter_is_refused` and
  `rubbish_in_a_stream_produces_no_thumbnail_and_no_panic` are all real. It is
  only the bitmap's *contents* that nothing checks.
