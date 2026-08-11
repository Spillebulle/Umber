# Audit: silent wrongness

A read of the paths that can produce a wrong document, a wrong pixel or a wrong
file **without saying so**. Not crashes and not performance. Read-only: nothing
was built or run, so every finding is marked **confirmed by reading** or
**inferred**, and the distinction is meant literally.

The headline is that this is a hard codebase to find defects in. The recorded
refusals in `CLAUDE.md` are, as far as this pass could tell, all still true of
the code, and the two areas that looked most dangerous going in — the tile atlas
and the `umber-version` 4 mask migration — came back clean on every path this
read followed. What is below is one real defect, two known-and-recorded gaps
that are worth re-ranking now that a second site has appeared, and two notes.

| # | Rank | What | Where |
|---|---|---|---|
| 1 | **BLOCKING** | Undoing a canvas flip that was refused moves the history anyway; the next undo then writes a patch into the mirrored position | `umber-app/src/app.rs:778` |
| 2 | SUBSTANTIVE | A flip that cannot back a tile drops it with a `log::error!`, and a flip's own inverse can never bring it back | `umber-render/src/canvas.rs:6698` |
| 3 | SUBSTANTIVE | An autosave capture is interrupted by Save, flip, resize and close — and by no ordinary edit | `umber-app/src/app.rs:3095`, `umber-render/src/canvas.rs:7877` |
| 4 | MINOR | `Capture::empty` is left stale where `Capture::gaps` is cleared; two fields for one fact, one of them reset | `umber-render/src/canvas.rs:9442` |
| 5 | MINOR | `ManifestEdit::mask`'s compatibility argument is stated against `umber-version` 2 and the figure is now 4 | `umber-core/src/docformat/history.rs:428` |

---

## 1. BLOCKING — a refused flip-undo still moves the history

**Confirmed by reading.**

`App::reverse`'s flip arm:

```rust
EditBody::Flip => {
    if let Some(axis) = kind.flip_axis() {
        self.mirror_document(axis);
    }
    EditBody::Flip
}
```
`umber-app/src/app.rs:778-783`

`mirror_document` returns `bool` and answers **`false` when any layer is
locked** (`app.rs:834`), refusing the flip whole — which is right, and is
exactly the rule `CLAUDE.md` records ("a picture with some layers mirrored and
some not is not a state the flip's pixel-less undo entry can describe"). The
command path honours it: `flip_canvas` does `if !self.mirror_document(axis) {
return; }` before recording (`app.rs:881`), and `ui.rs:1577` disables both menu
items while a layer is locked so the menu does not offer what it will not do.

`reverse` discards the answer. `undo` then pushes the entry onto the redo stack
and decrements the position regardless.

### The sequence

1. Paint a stroke on a layer. A `PixelPatch` is recorded in orientation *O*.
2. Flip the canvas. Allowed, because nothing is locked. Orientation is now *O′*
   and an `EditBody::Flip` entry sits on top of the history.
3. Lock any layer — a background layer, say, which is the ordinary reason to
   lock one.
4. Ctrl+Z. `canvas_is_ready` passes, the flip entry is taken off,
   `mirror_document` returns `false` and mirrors nothing, and the entry moves to
   the redo stack. **The History list now says the flip is undone and the canvas
   is still flipped.**
5. Ctrl+Z again. `swap_patch` writes the step-1 patch's pieces at the rectangles
   they were recorded at — rectangles of orientation *O* — into a canvas in
   orientation *O′*. A rectangle of the old picture lands in the mirrored
   position.

Step 5 is silent damage to the layer. It is also **unrecoverable by redo**:
`swap_patch` captures the redo patch from what it found at those rectangles,
which is the damaged content, so redoing writes the damage back. `mark_modified`
is set either way, so an autosave or a Save will write it out.

Nothing gates undo on the lock — `any_locked` has exactly three call sites
(`app.rs:834`, `panels.rs:1804`, `ui.rs:1577`) and none of them is the undo
path — and the Undo menu row is gated on `can_undo()` alone (`ui.rs:1676`).

### The narrower sibling, same arm

`undo` and `redo` call `finish_transform()` and **not** `finish_stroke()`
(`app.rs:683`, `app.rs:704`), where `flip_canvas` calls both (`app.rs:874`).
`CanvasRenderer::flip_layers`' own docs say the caller owes it no stroke in
flight, "the scratch surface … is not mirrored, so a stroke would commit
unmirrored over the flipped picture". Ctrl+Z with the pen or the mouse button
still down, on a document whose newest entry is a flip, reaches `flip_layers`
with the scratch loaded; the stroke then commits at its pre-flip position over
the re-mirrored picture. Reachable with a mouse; a keyboard event is not
suppressed while a button is held.

### The fix and the guard

The fix belongs where `flip_canvas` already puts it — ask before spending the
entry. `undo`/`redo` already ask `canvas_is_ready()` *before* `take_undo()`
precisely so that "taking one and then finding there is nowhere to apply it
would lose it"; the lock is the same question one step further on. Either check
`any_locked` beside `canvas_is_ready`, or have `reverse` report failure and have
the caller put the entry back untouched.

Guard to write: **`an_undo_of_a_flip_the_lock_refuses_changes_nothing_at_all`** —
build a stack, record a paint patch and a flip, lock a layer, call `undo`, and
assert (a) `history.position()` is unmoved, (b) `can_redo()` is false, and (c)
a subsequent `undo` restores the paint patch at the orientation it was recorded
in. The (a)/(b) half is testable without a device against `Editor`/`History`;
(c) needs `gputest::lock()`. A guard that only asserts "nothing was mirrored"
would pass today and miss the whole defect, which is in the *history* rather
than in the pixels.

---

## 2. SUBSTANTIVE — a flip that cannot back a tile loses paint for good

**Confirmed by reading.**

```rust
match self.layers.free.pop() {
    Some(cell) => cell,
    None => {
        log::error!("the atlas is full: slot {slot} loses a tile to the flip");
        continue;
    }
}
```
`umber-render/src/canvas.rs:6695-6701`

The destination tile is then left `UNBACKED` and reads as the slot's empty
value: a 256-square hole in the layer, transparent for a layer and full reveal
for a mask. Nothing on screen says anything.

`back_tiles` has the same shape (`canvas.rs:4636`) and is honestly labelled —
"§9.5's open problem, and it is not solved here… the tiles that could not be
backed are logged and skipped, and the stroke loses them. The alternative —
refusing the whole stroke — loses more." That reasoning is sound for a stroke.
**It does not carry to the flip, and this is the part worth re-ranking.** A
stroke that lost a tile can be drawn again. A flip's undo *is another flip*
(`EditBody::Flip`, no pixels), so the tiles it dropped are not in any patch, not
in any parked slice and not reproducible by any gesture: flipping back restores
the orientation and leaves the holes. `flip_layers` also has no reporting
channel — the effect bake grew `take_effect_refusal`/`Vram` for exactly this
class of refusal, and the flip has nothing equivalent.

`write_layer_rect` closes the same loop from the other side: after `back_tiles`
it skips any fragment whose entry is still unbacked (`canvas.rs:10288`), so an
**undo** replayed while the atlas is full silently fails to restore those
pixels — and the redo patch it hands back was read from the layer before the
write, so the pair still round-trips and nothing notices.

### Guard to write

**`a_flip_that_cannot_back_a_tile_says_so`** — drive `flip_layers` on a renderer
whose free list has been exhausted (the cheapest injection is a
`set_readback_limit`-style test hook capping `growth_for`, matching how the
banded readback is driven) and assert that a public reading reports the refusal,
in the shape `take_effect_refusal` already has. Assert *reporting*, not the
pixels: the pixel loss is the behaviour being accepted, the silence is the
defect.

---

## 3. SUBSTANTIVE — nothing interrupts an autosave capture except Save, flip, resize and close

**Confirmed by reading for the mechanism; inferred for the artist-visible
consequence** (not reproduced).

`stop_autosave_of` has four callers (`app.rs:879` flip, `app.rs:3109` save,
`app.rs:4329` resize, `app.rs:4371` close). `CanvasRenderer::touch_slot` —
called by every method that writes a slice — abandons a **thumbnail** of that
slot and says nothing to a capture:

```rust
fn touch_slot(&mut self, slot: u32) {
    if let Some(rev) = self.slot_revisions.get_mut(slot as usize) { *rev += 1; }
    if let Some(job) = self.thumb.as_mut() && job.slot == slot { job.abandoned = true; }
}
```
`umber-render/src/canvas.rs:7877-7886`

This is the "disowned job with no driver" shape the brief names, one step along:
the *thumbnail* half was closed and the *capture* half was not.

A capture takes at least one frame per layer plus banding, and the rule
`CLAUDE.md` records — "nothing starts unless the pointer is up and no stroke is
live" — governs **starting** only. So between step *k* and step *k+1* the artist
can commit a stroke, undo, clear a layer, add a mask or fill one, and each of
those writes a slice the capture has not read yet or has already read. The
result is a copy assembled from more than one instant:

- The flattened preview is drawn at the **last** step (`canvas.rs:9454`), so
  `mergedimage.png` can show a stroke the layer entries beside it do not have.
  That is what `docimport::preview` reads for shell thumbnails, so a recovery
  copy's thumbnail can show work that opening it does not.
- "Clear layer" (`app.rs:2105`) wipes a slice and clears the history. Done
  during a capture, the copy carries a blank layer under the old layer's name —
  in the one file that exists so work is not lost.

The metadata snapshot (`Candidate`, `app.rs`/`autosave.rs:2144`) was written so
that names and pixels cannot come from different instants. This is the other
half of the same rule, and it is not enforced.

Worth saying plainly: nothing here damages the artist's own `.ora`. The blast
radius is the autosave copy and what a recovery offers back.

### Guard to write

**`an_edit_during_a_capture_abandons_it`** — in `autosave.rs`'s `FrameLoop`
harness (which already drives `a_capture_interrupted_by_a_save_does_not_stop_
the_next_autosave`), start a capture on a document with enough layers to span
several frames, commit a stroke between two `drive_capture` calls, and assert no
file is written for that round *and* that the next round still autosaves. The
second half is the one that matters — the fix must not re-create the stranded
capture that `8ea00d6` closed.

---

## 4. MINOR — `Capture::empty` is not reset where `Capture::gaps` is

**Confirmed by reading.**

`drive_capture` sets `job.empty` and `job.gaps` together on every band of a
layer step (`canvas.rs:9380-9386`). The flattened-preview step clears `gaps`
with a paragraph explaining exactly why —

```rust
// **The flattened preview has no gaps** … Left alone they would punch that
// layer's unbacked tiles out of the merged image …
job.gaps.clear();
```
`canvas.rs:9437-9442`

— and leaves `empty` holding the last layer's value. It is harmless *only*
because `copy_chunk` iterates `gaps`, so an empty `gaps` never reads `empty`.
That is one fact held in two fields with one of them reset, which is the shape
this codebase records as the way two readings come to disagree. Anything added
later that produces a gap on a non-layer step inherits the previous step's empty
value silently — and for a mask/layer pair those values differ maximally
(`[255;4]` against `[0;4]`).

Guard: set `job.empty = [0; 4]` beside `job.gaps.clear()`, and
**`the_merged_step_carries_no_layers_empty_value`** reading it back through a
`#[doc(hidden)]` accessor, in the shape `thumb_phase_is_picture` already uses
for a test-only reading.

---

## 5. MINOR — a stale figure in the manifest's compatibility argument

**Confirmed by reading.**

```rust
/// … a mask patch can only be written where a layer actually has a mask —
/// `SaveHistory::new` refuses the whole history otherwise — and a document
/// with a mask declares `umber-version` 2, which every build that predates
/// this refuses before it reaches the manifest at all.
```
`umber-core/src/docformat/history.rs:426-430`

`required_version` has answered **4** for a document with a mask since the
linear-coverage change (`docformat/mod.rs:328`). The argument is unharmed —
it is strictly stronger at 4 — but the figure is wrong, and this file's own
rule is that "a figure in a comment is what the next change gets argued
against". Text change only.

---

## What was checked and came back clean

Recorded so a later pass does not re-walk it, and so that "clean" is a claim
somebody can disagree with.

- **The `umber-version` 4 mask migration.** `required_version` answers 4 for any
  mask; a mask patch in a saved history can only be written where a layer holds
  that mask (`SaveHistory::new`'s `find_map(…)?`), so there is no file in which
  a mask patch is linear while the document declares 3 or a mask entry is sRGB
  while it declares 4. Both readers take the *document's* declared version and
  not `docformat::VERSION` (`openraster.rs:744`, `docimport/history.rs:263`).
  `mask_pixel`, `mask_buffer` and `decode_v3_mask_buffer` write all three colour
  bytes, and the writer's `px[0]` and `composite.wgsl`'s `.r` therefore cannot
  disagree.
- **The tile atlas against every path that assumed slot == slice.** Composite,
  commit (`commit_aims`, and the class-not-caller choice of view at
  `canvas.rs:5974`), the blocking readback (synthesises the class's empty value
  before any copy lands), `write_layer_rect`, `resize` (`class` copied across;
  `sx % TILE` is within the source tile by construction), `flip_layers`
  (`mirrored_residency`, table written only after the passes have run),
  `begin_float` (both slots promoted, refusal refuses the whole float), the
  autosave capture (`gaps`/`empty` per band). `Grid::tiles_over`'s guard is
  written differently from `fragments`' but is equivalent, and
  `tiles_over_names_the_same_tiles_the_fragments_do` pins it.
- **Undo replay.** `swap_patch` reads before it writes; the saved history's
  slot↔position mapping and its `?`-drops-the-whole-history rule; the cut at
  `resurrects_pixels`; `is_structural` and `resurrects_pixels` are exhaustive
  `match`es, and `Edit::patches` returns the patch for `EditBody::Text` so the
  `None => SaveBody::Flip` fallback is still reachable only by a bodiless kind
  that does not exist.
- **Clipboard and transform.** `Clip::take`'s single pass with `left[3] = before
  - px[3]` (no underflow, `taken + left == before` byte for byte); `axis`'s NaN
  and saturation behaviour; `Clip::place`'s crop.
- **Selections.** The three booleans' bounding rectangles (intersect starts from
  the overlap, not from either operand), `combined`'s arm ordering,
  `shared_feather`'s `max`, `flipped`'s hard-mirror fallback,
  `coverage_at` outside bounds.
- **Importers.** The Clip Studio piece contract's clipping (`block_span` clamps
  both ends; `within` clips the bitmap's own padding); Krita's mask taking the
  linear byte unconverted and the `.defaultpixel` fallback; the Photoshop
  reader's three inversions — `is_clipped(psd_flag) = !psd_flag` looks wrong and
  is not: the fixture writes Adobe's `0 = base, 1 = clipped` byte and
  `a_clipped_layer_arrives_clipped` drives the real crate over it. That one is
  named because it is precisely the shape of confident-and-wrong finding the
  brief warns about.
- **The autosave's layer↔pixel pairing.** `pixel_index`/`mask_index` over
  `Candidate::slots`, and `note_slice`'s layer/mask boundary, are consistent
  with each other and with folders holding no slice.
