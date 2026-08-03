# Structural undo

Umber's undo covers painting, transforms and the two canvas flips. Adding a
layer, deleting one, reordering the stack, grouping, and adding or removing a
mask are not recorded — and two of those **clear the whole history**:

```rust
// app.rs, delete_entries
// Slots are recycled — both of them, where a layer had a mask — so an
// undo entry recorded against a freed slot would later be replayed into
// whichever layer or mask inherits it. Dropping history is the blunt but
// safe fix; structural undo is the real one.
self.editor.history.clear();
```

The same three lines are in `remove_mask`. That is the worst of what is missing,
and not because it loses a feature: it loses the *afternoon*, silently, at the
moment an artist is most likely to want a step back. Deleting the wrong layer is
one of the two mistakes a layers panel makes easy — the other, `remove_many`'s
reverse loop, has already deleted a layer nobody ticked, and cleared the history
on the way out so it could not be taken back.

This document is the design. Nothing in it is built.

---

## 1. The slot problem, which is the whole difficulty

`PixelPatch::slot` is a texture-array slice, and `LayerStack::take_slot`
recycles: a slice freed by a deletion is handed to the next layer or mask that
asks. A patch recorded against slot 3 and replayed after slot 3 has changed hands
writes somebody's pixels into somebody else's layer. That is the one failure
this whole area exists to prevent, and it is why the file never writes a slot
down either.

Three candidate fixes, and the third is the one to dismiss first because it is
the one that will keep being suggested.

### Storing the deleted layer's pixels in the entry — no

Honest and simple. On a 10000² canvas a layer slice is `10000 × 10000 × 4` =
**400 MB**, against a 512 MB default budget. One delete would therefore hold the
budget open on its own, age out everything before it, and put "Earlier edits
discarded" on the panel — which is *correct behaviour* that reads exactly like a
regression, which is why the budget section of CLAUDE.md exists at all. At 2048²
it is 16 MB a delete, so thirty-two of them fill the budget.

But the size is not the argument that kills it. This is: **the pixels are
already somewhere safe.** Deleting a layer does not free the texture — the layer
array grows by doubling in `CanvasRenderer::ensure_slots` and never shrinks, and
a freed slice is only overwritten when `add`/`add_mask` hands the number out
again and `app.rs` clears it. So the pixels of a deleted layer are sitting
untouched in VRAM at the moment the delete happens. Reading them back is a
blocking readback of a whole canvas at the moment of an interactive command,
into 400 MB of RAM, to make a second copy of pixels nothing is going to
overwrite. It buys nothing and costs everything.

### A stable layer id, with patches naming it — not for this

The obvious fix: give a layer an id, have `PixelPatch` name the id, resolve to a
slot at replay time. It does not answer the question. A deleted layer's id
resolves to nothing, so the entry still needs the layer *back*, which still
needs its pixels, which brings us straight back to the paragraph above.

An id turns out to be needed, but for a different job and in a different place —
see §3. It is not what keeps patches valid.

### Deferred recycling — yes, and it is not really "deferred"

A slot is not returned to the free list while anything still names it. Then no
other layer can inherit it, every recorded patch goes on meaning the pixels it
was captured from, and the deleted layer's slice is left holding exactly the
picture an undo would want to put back.

The sharp way to say it is not "deferred recycling" at all:

> **The undo entry holds the deleted layer — everything about it except its
> pixels, which never move.**

A `Layer` is a name, some flags, a depth and two slot numbers. Holding one costs
tens of bytes. Holding it is what parks the slice, because the slice belongs to
the layer and the entry is now where the layer lives. There is no second
mechanism, no copy, no readback, and no pixel path involved in a structural undo
at all.

The cost is real and is a *slice*, not RAM: a parked slot is a canvas-sized
allocation the array already made, unavailable to the next new layer. `MAX_SLOTS`
is `MAX_LAYERS * 2 + 1` = **129**, a hard ceiling. §7 says what happens when it
is reached, and the answer is not a refusal.

---

## 2. Why this is cheap for the same reason the flip is

`EditBody::Flip` stores nothing, and the module docs are careful that this is
sound *only because undo is stepped rather than seeked*: `steps_to` turns a jump
into that many single `undo` calls, so an older patch is always reached with the
flip above it already undone, and the canvas is back in the orientation that
patch was recorded in.

A structural edit is **not** its own inverse — undoing an add is a delete — so it
cannot be free the way a flip is. What carries over is the half that matters:

- **No pixels.** A structural entry describes the stack, and the stack holds
  slots rather than pictures. `LayerStack` is the one part of a document small
  enough to write down whole, and that is the whole reason structural undo can be
  simple where pixel undo cannot.
- **The stepped guarantee, one level up.** An entry that says "the stack looked
  like this" only means anything in the stack shape that existed at that moment,
  and stepping is what guarantees you arrive in it. Where the flip's argument is
  about the canvas's orientation, this one is about the stack's shape. It is the
  same argument and it should be written down in the same words.

Two consequences fall out and both are load-bearing:

- **A patch older than a delete is only ever reached with the delete undone**, so
  the layer is back in the stack when its pixels are written. Without a recorded
  delete entry this is false, and the failure is silent: `swap_patch` writes into
  a parked slice that nothing composites, and the artist presses Ctrl+Z and sees
  nothing happen. **Parking the slots without recording the delete is worse than
  clearing the history**, and that is why the first shippable piece in §11 is
  larger than it looks.
- **A patch can only exist on a layer that was in the stack when it was made**,
  so a patch naming a deleted layer is necessarily *older* than the delete that
  removed it. §8 leans on that.

---

## 3. Two identities, doing two different jobs

A structural entry has to be able to say "the entry that was at position 3 goes
back to position 3". Which entry is that? Not a position — positions are what is
being restored. So it needs an identity, and a slot is not one, because **a
folder holds no slot**.

So there are two:

- **The slot claim** answers "which slice do these pixels live in". It is what
  `PixelPatch` names, unchanged, and what parking keeps valid.
- **`Layer::id`** answers "which entry is this". A `u32` from a per-document
  counter, never recycled, never written to the file. Folders have one; layers
  have one; masks do not need one, because a mask is a field of a layer.

They are not the same thing and conflating them is exactly what the folder case
exposes. Neither can do the other's job: an id cannot keep a patch valid (§1),
and a slot cannot name a folder.

### Making the claim exhaustive rather than remembered

The claim's whole job is "this number goes back on the free list when the last
holder lets go", and the failure mode of getting it wrong is a slot handed to two
layers — a silent corruption. So it must not be a rule stated at call sites. The
shape that is exhaustive by construction:

```rust
/// A slice of the layer array, held for as long as anything names it.
///
/// `Drop` is what returns the number to the pool, which is why this is the
/// only way to hold one: a `free_slots.push` beside each of the places a
/// layer can leave the stack is the "forgotten at the sixth" failure written
/// out in advance.
pub struct SlotClaim {
    number: u32,
    pool: Arc<Mutex<SlotPool>>,
}
```

`Layer::slot()` goes on returning `Option<u32>`, read off `number` with no lock,
so every existing call site and the drawing path are untouched — only `Drop`
takes the lock. `Layer` stays `Clone` and cloning shares the claim, which is
precisely the semantics a snapshot wants.

This is the same argument `CanvasRenderer::slot_revision` already makes for
itself, and the same one `Layer::picked` makes against a set held beside the
stack.

**The alternative was a `parked` list on `LayerStack` drained by the history's
eviction**, with `History::record` and `clear` returning what they dropped so a
caller could hand the slots back. It is more explicit and it needs a correct call
at five sites: `record`'s eviction, `clear`, the redo drain inside `record`,
`restore`, and `set_budget`. Five is the number at which this project's own rule
says it will be four.

---

## 4. The entry restores the *shape*, not the values

This is the part that would otherwise ship broken.

The tempting form of a structural entry is a snapshot of the whole
`Vec<Layer>` — four kilobytes, restored wholesale, one inverse rule for every
operation there will ever be. It is wrong, and the reason is subtle: a layer's
*properties* — its name, opacity, visibility, blend mode, lock, link, clipping —
are not undoable and are not going to be (§10). A snapshot that carried them
would make undoing a **reorder** silently revert an opacity set afterwards. That
is an undo damaging something it was never asked about, which is the class of bug
this codebase refuses everywhere.

So:

```rust
/// One position in a recorded stack shape.
enum ShapeEntry {
    /// An entry that survived the edit. Put it back here, at this depth, and
    /// leave everything else about it alone — its opacity may have been
    /// changed since, and that is not this entry's to revert.
    Kept { id: u32, depth: u8 },
    /// An entry the edit removed. Put the whole layer back: nothing could have
    /// changed it, because it has not been in the stack to be changed.
    Gone { layer: Layer },
}

/// The stack as it was before one structural edit.
pub struct StackShape {
    entries: Vec<ShapeEntry>,
    /// Which entry was selected, **by id**: the selection follows the layer,
    /// which is the rule `LayerStack::reorder_to` already keeps.
    active: u32,
}
```

and `EditBody` gains a third arm:

```rust
pub enum EditBody {
    Pixels(PixelPatch),   // see §6 — this is the arm the linked transform pluralises
    Structure(Box<StackShape>),
    Flip,
}
```

Four things follow, and each of them is a bug that does not have to be written:

- **`Gone` is what parks the slice.** The `Layer` it holds owns the slot claim.
  Parking is not a separate mechanism; it is this field.
- **A folder deletion is one entry, in one step, with no index arithmetic.** The
  entry describes the whole stack rather than a set of positions, so the reverse
  loop that once deleted a layer nobody ticked has nothing here to recur in.
- **A restored shape is well-formed because it was well-formed when recorded.**
  A per-operation inverse would have to be judged by `well_formed` all over
  again; this cannot invent a layer nested inside nothing.
- **The entry holds the shape *before* the edit**, and undoing swaps the current
  shape into it — exactly what `swap_patch` does with pixels, so redo is the same
  code path and there is no second implementation to keep exact. `App::reverse`
  gains one arm and nothing else changes.

Size: `size_of::<Layer>()` is about 56 bytes with padding, so a 64-entry stack is
under 4 kB plus the names of whatever was removed. Against a 512 MB budget that
is on the order of a hundred and thirty thousand entries — which is to say the
budget is not what bounds these, the slot ceiling is. **That figure is arithmetic
from the struct, not a measurement**; if it is ever worth pinning,
`examples/measure-undo.rs` is where it goes.

---

## 5. Which `EditKind`s earn their existence

The rules are fixed and narrow. A variant exists only for something the engine
can restore; `panels::edit_icon` is exhaustive over the enum deliberately, so a
new variant cannot be added without deciding what it looks like; and **two rows
that undo identically must not have two names** — a paste is filed under
Transform, a cut under Erase.

The last rule needs stating precisely before it is applied here, because under §4
*every* structural edit undoes identically: they all restore a shape. Read that
way the rule would collapse the lot into one row saying "Layers", which is
plainly wrong. The rule is about what the **painter did**, not how the engine
stores it. Paste and Transform fail it because both are a rectangle of pixels
arriving on a layer. Add, Delete and Move pass it: somebody scanning the list for
"where did my layer go" is looking for exactly that word.

Six new variants:

| Variant | Label | Icon |
|---|---|---|
| `AddLayer` | Add layer | `Icon::Plus` — the layers panel's own add button |
| `DeleteLayer` | Delete layer | `Icon::Trash` — the bin the artist pressed |
| `MoveLayer` | Move layer | **new**, two chevrons back to back |
| `Group` | Group | `Icon::Folder` — the thing that appeared |
| `AddMask` | Add mask | `Icon::Mask` |
| `RemoveMask` | Remove mask | **new**, `Mask` with a stroke through it |

`EditKind::ALL` goes from five to eleven and `edit_icon` gains six arms.

Two new icons, and both have a precedent in `icons.rs` rather than being
invented: `Icon::Deselect` is already "`Select`'s dashed box with a stroke
through it — not the box greyed", which is exactly the relationship `RemoveMask`
wants to `Mask`. `MoveLayer` needs its own because `ChevronUp` alone would say
"moved up" on a row that may have moved down, and `Grip` is the drag *handle*
rather than the act.

What deliberately does **not** get a variant:

- **A folder delete is not a second kind.** It restores identically to a layer
  delete — the same `Gone` rows in the same entry — and the stack calls them
  entries for a reason. Two names for one undo is the rule above.
- **An empty folder created is `AddLayer`**, for the same reason. `Group` exists
  only because grouping is a folder appearing *and* several entries moving, which
  is neither of the two kinds beside it.
- **There is no `Ungroup`**, because there is no such operation: dissolving a
  folder is a delete of the folder or a set of moves out of it, and it should
  record as whichever it actually was.
- **`RemoveMask` and `DeleteLayer` are not one kind**, even though a mask is a
  slice like a layer's. Removing a mask changes the picture — what it hid comes
  back — and it is the only edit in the table that does; a row calling that
  "Delete layer" would send somebody looking for a layer that never went.

`AddMask` is the weakest of the six and is the one to cut if the set is too
large. Adding a mask changes no pixel — a new mask is filled opaque white — so
its row is a step that looks like nothing happened. It is kept because it is
still a change to the stack, and because a `RemoveMask` with no `AddMask` beside
it would be an obviously half-drawn pair.

---

## 6. One entry, several things — and the multi-layer transform

`docs/linked-transform.md` is being designed at the same time as this, and it
hits the same wall from the other side. CLAUDE.md states its half already:

> `Float` holds one layer's pixels, one base, one bind group, and `EditBody::
> Pixels` holds one patch — so moving several at once needs N of each *and* an
> entry holding several patches, or an undo would step through a multi-layer move
> one layer at a time and leave the document in states it was never in.

**Structural undo does not need that**, and the two designs are independent: a
structural entry stores no pixels at all, so the folder-delete case is solved by
§4's shape and not by a plurality of patches.

They meet in exactly three places, and a supervisor reconciling the two should
decide all three at once:

1. **`EditBody` gains arms from both.** The shape that satisfies both is
   `Pixels(Vec<PixelPatch>)` alongside `Structure(Box<StackShape>)` and `Flip`. A
   one-element `Vec` is the overwhelmingly common case; its 24-byte header
   against patches measured in megabytes is not worth a `SmallVec` dependency,
   and `PixelPatch::byte_len` already counts per-piece bookkeeping so the budget
   accounting stays consistent.
2. **`Edit::patch() -> Option<&PixelPatch>` becomes `patches() -> &[PixelPatch]`.**
   Both want it; whichever lands first should do it, and the other should not
   introduce a second accessor.
3. **One `history::VERSION` bump, not two.** Both change the manifest — one adds
   a plural patch list to an entry, the other adds a structural body. If they ship
   in the same release they are one revision. If they ship apart they are two, and
   the second must still read the first.

The one thing that is genuinely *not* shared: a linked transform's several
patches are several slices of the **same** edit to the pixels, while a structural
entry's several restored layers are the same edit to the **stack**. Neither is a
generalisation of the other, and a design that tried to express both as "an entry
with N things in it" would end up with a body that means two different things
depending on the kind.

---

## 7. The ceiling, and what happens when it is reached

`MAX_SLOTS` is 129 slices, mirrored between `LayerStack::MAX_SLOTS` and
`canvas.rs`. A slice is canvas-sized RGBA8: **16 MB at 2048², 64 MB at 4096²,
400 MB at 10000²** — arithmetic, not measured. The array grows by doubling and
never shrinks, so a parked slot costs no *new* allocation; what it costs is the
chance to reuse one.

Today the ceiling is reachable only by a live document: 64 layers each with a
mask is 128 slices plus the transform tool's one spare, and `add` answers `None`
and `app.rs` logs "layer limit reached". Under this design a *history* can hold
slots too, so the ceiling becomes reachable by a session of adding and deleting.

**When the pool is empty, the history gives a slot back — it does not refuse the
layer.** Entries are dropped oldest first until one releases a claim, which is
`evict_to_budget`'s loop with a different stopping condition and should be that
same function generalised rather than a second one beside it. `History::dropped`
counts them, so the panel's existing "Earlier edits discarded" note already
covers the case and already says what the limit is.

Only when the history is empty *and* the live stack holds every slice is the
operation genuinely refused — which is exactly today's condition, unchanged. So
this adds no refusal a painter can reach that they could not reach before.

Two invariants worth writing at the code:

- **A parked slice's pixels are destroyed exactly when the slot is handed out
  again**, and that cannot happen while any entry names it. `app.rs` already
  clears a recycled slot on the GPU; that line becomes the *only* moment a parked
  picture dies, which is what makes the whole design safe with no new GPU work.
- **The autosave is untouched.** `begin_capture` walks the stack's slots through
  the snapshot's own `pixel_index`; a parked slice is in no stack entry, so it is
  never read and never written to a file. A parked layer must not appear in a
  saved document — it is not part of the picture.
- **`Thumbs`' cache may keep dropping a slot that leaves the stack.** Its rule
  today is "a slot that leaves the stack loses its picture, because slots are
  recycled". Under parking that reason is no longer true, but the behaviour still
  is correct — an undone delete simply re-reads the thumbnail. Say so at the
  code, or somebody will "fix" it into a cache that grows with the history.

---

## 8. The saved history

`history::VERSION` is 3. The bar for raising it is a revision an older build
would **misread**, and this clears it on the strongest possible grounds — the
same grounds the flip cleared it on, which is the sharper of the two existing
cases.

**What a revision-3 reader does with a structural entry:** `kind_from_id` answers
`None` for a name it does not know, and `docimport::history` drops the whole
history on that. So an older build is already safe by construction, exactly as it
was for the flip. But the reason it would report is wrong, it would have decoded
PNGs first, and — the part that actually earns the bump — **the entries around a
structural one are not independently valid**. A pixel entry recorded before a
delete was recorded against a stack of a different shape; a reader that dropped
the structural entry and kept the rest would replay patches into positions that
mean something else. That is the flip's argument with "orientation" replaced by
"stack shape", and it is why the degradation must be a whole-history discard
rather than a shorter history.

So: **`history::VERSION` → 4, and `umber-version` does not move.** A structural
history lives entirely under `umber/`, which every other ORA reader ignores; an
Umber build that discards it opens the picture whole, which is what every build
before saved histories existed did. That is the argument `docformat`'s module
docs already make for the history's existence and it needs no extending.

### Positions, which is where it gets hard

The file's governing rule is that **a slot is never written down** — entries name
a stack *position*, mapped from a slot by `SaveHistory::new` at save time and
back by `ImportedDocument::open` at load. Structural edits change positions, and
the manifest fingerprints the canvas and the layer names of the stack as it
*loaded*.

Both halves still work, and neither needs a new idea:

- **A `ShapeEntry::Kept` writes a position**, mapped from its id exactly as a
  patch's slot is mapped from a slot. The fingerprint is unchanged: the manifest
  goes on listing the current stack's names, positions go on counting every entry
  including folders, and a position that resolves to nothing goes on dropping the
  whole history.
- **The mapping is still made once, at load.** After that an entry names ids and
  slots, so undoing a reorder moves the layer and a later patch still finds it —
  because a slot follows the layer, which is the property `a_patch_finds_its_
  layer_again_however_the_slots_fell_out` already pins.

- **A `ShapeEntry::Gone` names a layer that is not in `stack.xml` at all**, and
  that is the one thing the file cannot express today. Its pixels are in a parked
  slice, and a parked slice is deliberately not written to the document (§7).

### Revision 4 truncates; a later revision could carry the pixels

The cheap and honest answer, and the one to build:

> **A save keeps the newest run of the timeline that contains no entry which
> resurrects pixels.** Everything older is dropped, `position` moves back by that
> much and `dropped` moves forward by it.

That machinery already exists — it is exactly what `docformat::history::write`
does when the file budget bites, with `first`, `position.saturating_sub(first)`
and `dropped + first`. And the theorem from §2 is what makes it sufficient: *a
patch on a deleted layer is necessarily older than the delete*, because you
cannot paint on a layer you have already deleted. So truncating at the newest
`DeleteLayer` or `RemoveMask` removes precisely the entries whose slots cannot be
placed, and nothing else. Adds, moves and groups save whole and need no new
archive entries at all.

The failure this leaves is real and should be said in the module docs: a session
with one deletion near the start saves almost no history. Whether that is
acceptable for good, or whether a later revision writes the removed layers'
images under `umber/history/layers/` beside the patch PNGs, is **an open
question I could not settle** — it turns on what a deleted layer costs in the
file, and that has not been measured. `examples/measure-history.rs` would need an
argument for it. What is clear is the shape it would take: a PNG at
`Compression::Fast` like every other image in the archive, against the same
`BUDGET_BYTES` of 32 MB, with the manifest carrying the layer's name and flags
beside it — no new mechanism, only more of an existing one.

Doing it *later* costs nothing, because a revision-4 file's truncated history is
an ordinary history and a revision-5 reader reads it unchanged.

---

## 9. What else clears the history, and which of it this fixes

| | Clears today | After this |
|---|---|---|
| Deleting a layer or folder | yes | no |
| Removing a mask | yes | no |
| Clearing a layer | yes | yes — see below |
| Resizing the canvas | yes | yes, and permanently |
| Reordering, grouping, folding | no | no |

**Removing a mask** is the same bug and the same fix: the slice goes on the free
list, so a patch naming it would be replayed into whatever inherits it. Under §1
the claim is held by the `Gone` row and nothing is inherited. The tooltip that
warns about it comes off in the same commit.

**Clearing a layer** (`clear_active_layer`) is not a slot problem at all — the
slot is untouched. It clears the history because the edit is not recorded, and
because an entry recorded before it would restore part of a layer the artist
asked to be empty. It is independently fixable and needs none of this design:
record it as `EditKind::Erase` with a full-canvas `PixelPatch`, which is what an
eraser stroke is and undoes as, so no new variant and no `edit_icon` arm. The
cost is stated plainly rather than hidden: on a 10000² canvas that patch is
400 MB and the budget holds one, exactly as a full-canvas wash does.

There is a cleverer version worth naming and *not* building first: park the
cleared layer's slice and give the layer a fresh slot, making Clear a zero-copy
structural edit. It works — nothing depends on a slot number beyond the layer's
lifetime once patches are parked — and it costs one slice from a ceiling of 129
per clear instead of 400 MB of RAM. It also quietly breaks "a layer's slot never
changes", which `layer.rs`'s module docs state as a fact. Build the plain patch;
keep the trick in reserve.

**Resizing the canvas cannot be fixed by any of this**, and it was considered. A
`PixelPatch` is a rectangle of a *particular* canvas — that is why the manifest
stores the canvas size and refuses a history recorded against a different one —
so every recorded rectangle stops being meaningful, not just the ones near an
edge. Nor could a resize be made structural: `CanvasRenderer::resize` reallocates
the whole layer array, so there is nothing to park, and a crop destroys pixels
outside the new canvas that only a full copy of the old document would hold. It
stays as it is, and the reason stays where `Editor::apply_canvas` already says
it.

---

## 10. What this deliberately does not make undoable

Renaming a layer, and changing its opacity, visibility, blend mode, lock, link
or clipping. Every one of them would be *free* under §4 — they are fields of a
`Layer` and a shape entry could carry them — and they are left out anyway, for a
reason that has nothing to do with cost: **an opacity is dragged**. One drag is
sixty frames and would be sixty rows, so making it undoable means a coalescing
rule — when does a run of changes become one entry, what ends it, and what
happens when the artist drags one slider then another — and a History list with
forty "Opacity" rows in it from one gesture is worse than a list that stays quiet
about the whole subject.

The `Kept`-restores-shape-not-values rule in §4 is precisely what makes deferring
them safe: an undo of a reorder cannot revert an opacity, because it never
carried one. If property changes are ever added, **that rule must not be quietly
broken to make it easier** — a `Kept` row that started carrying values would make
every existing structural entry revert things it was never asked about.

---

## 11. A staged plan

Each piece builds, tests and merges on its own.

**1. The slot pool.** `SlotClaim`, `SlotPool`, `Drop` returning the number.
`Layer::slot()` unchanged in signature. No behaviour change at all: nothing holds
a claim outside the stack yet, so freeing happens at exactly the moments it
happens today. Also `Layer::id` and the counter, unused. Pure `umber-core`, no
GPU, low risk.

**2. `EditBody::Structure`, with add and delete recorded — the risky piece.**
This is where the history stops being a list of independent rectangles and starts
being a sequence that only means anything in order, and it is where the failure
is *silent*: get it wrong and an undo writes into a parked slice and looks like
an undo that did nothing. It cannot be made smaller, for the reason §2 gives —
parking the slots without recording the delete is worse than clearing the
history. It ends with the first `history.clear()` in `delete_entries` gone.

**3. Masks.** `AddMask`, `RemoveMask`, the second `history.clear()` gone, and the
tooltip that warns about it. Same body, same parking; a small piece riding on
piece 2.

**4. Reorder and group.** The cheapest piece — no slot changes hands, so it is
pure model and pure list — and the least missed, which is why it is not first.

**5. The file.** `history::VERSION` → 4, the structural body in the manifest, and
save-time truncation at the newest pixel-resurrecting entry. Until this lands,
`SaveHistory::new` refuses a history holding a structural entry and the document
saves with none, which is what every build before saved histories did. Nothing in
pieces 1–4 touches the file.

**6. Clear layer as an `Erase` entry.** Independent of all of the above; could go
first.

**7. (Open.) Deleted layers' images in the file**, removing the truncation. Needs
the measurement §8 asks for before anyone commits to it.

---

## 12. Tests

Named in the codebase's style, and the first two are the ones that matter.

- `stepping_back_over_a_delete_puts_older_patches_in_the_layer_they_were_recorded_in`
  — the shape of `stepping_back_over_a_flip_puts_older_patches_where_they_were_
  recorded`, and for the identical reason. A CPU model of a stack and a canvas,
  no GPU, comparing the document byte for byte against the copy it started as.
- `undoing_a_reorder_does_not_put_back_an_opacity_changed_since` — §4's rule, and
  the one thing here that would otherwise ship broken and be found by a painter.
- `a_slot_returns_to_the_pool_only_when_the_last_holder_lets_go` — piece 1 alone.
- `a_folder_deletion_undoes_in_one_step` — the whole subtree back, in one entry,
  with the stack never in a state it was not in.
- `the_slot_ceiling_shortens_the_history_rather_than_refusing_a_layer` — §7, and
  it needs no GPU because it is the pool's arithmetic.
- `a_history_with_a_delete_in_it_is_truncated_rather_than_refused` — §8's save.
- `a_document_of_structural_entries_declares_the_revision_it_needs` — the twin of
  `a_document_of_folders_still_declares_the_revision_it_needs`, and it must check
  that `umber-version` did **not** move.
- One GPU test, in `gpu_pipeline.rs`: a stroke on a layer, the layer deleted, the
  delete undone, and the layer's pixels read back. That is the assertion the
  parked slice exists for and no CPU test can make it.

---

## 13. The numbers, and which of them are guesses

Quoted from where they were measured:

- A patch is the rectangle a stroke covered, so a full-canvas stroke on a 10000²
  document is **400 MB** and the 512 MB budget holds one —
  `one_broad_stroke_on_a_large_canvas_all_but_fills_the_budget`.
- A thin diagonal across that canvas is **381.5 MB as a box and 6.8 MB as
  cells** — `damage.rs`'s table, from `examples/measure-undo.rs`.
- The file's budget is **32 MB encoded**, and 120 full-canvas strokes on a 2048²
  canvas take a document from **9.68 MB to 22.13 MB** — `docformat::history`,
  from `examples/measure-history.rs`.

Arithmetic done here, not measured, and flagged as such:

- A layer slice is canvas-sized RGBA8: 16 MB at 2048², 64 MB at 4096², 400 MB at
  10000². `MAX_SLOTS` is 129.
- `size_of::<Layer>()` is about 56 bytes, so a 64-entry stack shape is under
  4 kB. If the budget's accounting for structural entries is ever worth pinning,
  `measure-undo.rs` is where the figure belongs.

**Needs measuring before it is decided:** what a deleted layer costs in a saved
document, as a PNG at `Compression::Fast` — which is the whole of whether §8's
truncation is a staging step or the permanent answer. `measure-history.rs` is the
file to extend, and no figure should be quoted for it from memory.
