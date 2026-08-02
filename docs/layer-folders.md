# Layer folders — a design, and why they are not built yet

Umber's stack is flat. Folders were asked for alongside content thumbnails,
ticking several layers, and link groups; those three are built and this one is
not. This document is the design, so that whoever picks it up is not starting
from the beginning, and the list of invariants it collides with, so that nobody
picks it up thinking it is small.

**The short reason it was not built with the other three:** a folder that a
painter can put opacity or a blend mode on is *group compositing*, and group
compositing cannot be expressed in `composite.wgsl` as it stands. Everything
else here — the model, the file, the drag — is work; that part is a change to
the one shader four other code paths reuse. A folder that composites wrongly, or
that writes files an older Umber opens showing a different picture, is worse
than no folders at all, and CLAUDE.md says so about narrower things than this.

---

## 1. What a folder has to do

Three things, in rising order of cost:

1. **Contain layers**, so the list can be collapsed and a stack of forty is
   readable. Costs nothing but the model and the list.
2. **Move as a unit**, so dragging the folder takes its contents. Reordering is
   a `Vec` shuffle, so this is nearly free — it is what link groups already do,
   with a different rule for which layers come along.
3. **Apply its own visibility, opacity and blend mode to what is inside it.**
   This is the expensive one, and only the first of the three is free.

Visibility is the cheap member of (3) and worth separating out: hiding a folder
is exactly "hide every layer in it", because visibility is a boolean and
`hidden ∧ anything = hidden`. Opacity is not, and this is the whole difficulty:
a folder at 50% over two *overlapping* children is not the same picture as two
children each at 50%. Where they overlap, the second is composited over the
first and only then faded; halving each of them lets the lower one show through
the upper one. Blend modes are worse again — a folder set to Multiply must
multiply the group's *result* into the backdrop, not each child separately.

Photoshop and Krita spell this distinction **pass-through** versus **isolated**.
A pass-through folder is only a container: its children composite into the same
accumulator everything else does, and the folder's opacity and blend are
disabled or ignored. An isolated folder composites its children into an
accumulator of their own, then composites that result into the backdrop with the
folder's own opacity and blend.

**A pass-through folder is buildable today with no shader change at all.**
Multiply each child's `visible` by every ancestor's, flatten, and hand the
composite the same array it gets now. That is a real feature — it is what most
people reach for folders for — and it is the sensible first release.

---

## 2. The model

`LayerStack` is a `Vec<Layer>` with an `active: usize`, and every API in the
codebase indexes it by position. Two shapes were considered:

**A real tree** (`enum Node { Layer(Layer), Folder(Folder, Vec<Node>) }`) reads
best and breaks everything: `get(index)`, `active_index`, `reorder`, `remove`,
`layer_draws`, the autosave's snapshot, `SaveHistory`'s position mapping and
`layerdrag`'s rows all take a flat index today.

**A flat list with a depth** is the recommendation. Keep the `Vec`, add to
`Layer`:

```rust
/// How deeply nested this entry is. 0 is the top level.
pub depth: u8,
/// This entry is a folder: it owns the entries above it with a greater
/// depth, and holds no pixels.
pub folder: bool,
```

with the invariant that a folder's children are the contiguous run of entries
*above* it whose depth is greater — the same order the composite already walks,
and the same order ORA's nested `<stack>` writes. Then:

- `get`, `active_index`, `set_active` and every index-based caller keep working.
- `children_of(index)` is a scan of the run above `index` — no allocation
  needed for the common questions (`any_hidden_ancestor`, `subtree_len`).
- `reorder` grows one rule: moving a folder moves its whole run, and a
  destination *inside* a folder rewrites the moved entries' depths. That is the
  same shape as the link-group move already in `reorder`, and it should go
  through the same code path, not beside it.
- A folder occupies no slot. `Layer::slot` would have to become an `Option<u32>`
  or a folder would have to hold a slot nothing writes to. The first is honest
  and touches every `slot()` caller; the second is a lie that will be found by
  the autosave, which reads every slot back. **Take the `Option`.**

`LayerStack::MAX` is 64 because `MAX_LAYERS` in `composite.wgsl` sizes a uniform
array. If folders occupy entries in that array (see below), the cap has to count
folders too, and CLAUDE.md's "`LayerStack::MAX`, `MAX_LAYERS` in `canvas.rs`, and
`MAX_LAYERS` in `composite.wgsl` must agree" gains a fourth member: the number of
stack *entries*, which is layers plus folders.

### Ticking a folder

The rule asked for is "ticking a folder ticks everything in it". That belongs
beside `LayerStack::targets`, which is already the one place "what does this
operation reach" is decided, and it should be **written into the ticks** rather
than derived at read time: a painter who ticks a folder and then unticks one
child means what they did, and a rule that re-derived the set would put the
child back. So `set_picked(index, on)` cascades down the subtree, and `targets`
stays exactly as it is.

A folder that is ticked and whose children are not is then impossible, which is
the state that would otherwise have to be drawn as a third checkbox state.

---

## 3. The composite pass

This is the part that needs care.

`composite.wgsl` walks `layers: array<vec4<f32>, MAX_LAYERS>` bottom to top into
one `acc`, carrying a running `clip_alpha` for clipped layers. Two properties of
that loop are load-bearing and are what a folder disturbs:

- **`acc` is a single accumulator.** An isolated group needs its own.
- **`clip_alpha` is a single running value.** A clipped layer answers to the
  nearest unclipped layer below *within the same group* — clipping across a
  folder boundary is not a thing any application allows — so it has to be saved
  and restored with the accumulator.

### The shape that works

An explicit stack in the fragment shader, depth-bounded:

```wgsl
const MAX_GROUP_DEPTH: u32 = 8u;
var acc_stack: array<vec4<f32>, MAX_GROUP_DEPTH>;
var clip_stack: array<f32, MAX_GROUP_DEPTH>;
```

On an entry marked "open a group": push `acc` and `clip_alpha`, set both to
zero. On "close a group": pop, and composite the finished group into the
restored `acc` with the folder's own opacity and blend mode, through
`composite_over` — the same function every layer already goes through, which is
what keeps there from being a second copy of the blend maths.

Three notes on making that real:

- **The entries can be encoded in the array that exists.** `extra[i].w` is
  currently unused (`(mask slot, has mask, clipped, unused)`), so a folder's
  open/close marker fits with no new uniform and no new binding. A folder needs
  two entries — one to open and one to close — or one entry plus a subtree
  length; the length is fewer entries but makes the shader's loop non-linear,
  and the loop is the thing this design is trying to keep simple.
- **`MAX_GROUP_DEPTH` has to be enforced in `umber-core`**, not hoped for. A
  document nested deeper than the shader's stack must be refused at the point of
  nesting, because a fragment shader has nowhere to report it.
- **A pass-through folder emits no markers at all.** It is flattened away in
  `Editor::layer_draws`, so the ordinary document pays nothing — the same rule
  the mask's uniform branch and the grain's `mix(1.0, …, 0.0)` live by.

### What else reads that pass

`export_rgba`, `pick_colour`, `probe_canvas` and the autosave's capture all
reuse the composite with `export: true`. None of them would need changing —
they take a `&[LayerDraw]` and pass it through — **provided** the group markers
travel in `LayerDraw` rather than being a second parameter. Make them a field of
`LayerDraw` and the four reusers stay untouched; make them an argument to
`composite` and there are four more places for an export to stop matching the
screen.

The new GPU test to write is the one that catches the whole point: two
overlapping opaque children in a folder at 50%, against the same two children at
50% each outside a folder. The two must differ, and the first must equal the
flattened-then-faded result. `composite_pixel` in `gpu_pipeline.rs` is the
harness for it.

---

## 4. The file format

ORA has nested `<stack>` elements, so the format carries folders natively and
Umber's reader already understands them — `docimport::openraster::parse_stack`
keeps a `Vec<Group>` of open stacks and folds each group's opacity and
visibility into the layers inside it, flattening the tree. That is a **lossy**
read today, and it is one of the two things the reader's module docs say do not
survive.

Writing folders is therefore emitting the nesting that reader already parses,
and reading them is keeping the tree instead of folding it away. Neither is
difficult. The interesting question is the version.

### Does this raise `umber-version`?

The rule is that a version is raised when an older build would drop something
and show a picture that is **wrong**, not merely plainer. Applied here, the
answer is split, and the writer already has the mechanism for a split answer —
`required_version` emits the lowest revision the file actually needs:

- **A pass-through folder does not raise it.** An older Umber flattens it and
  folds the visibility in, which is precisely what a pass-through folder means.
  The picture is identical; the painter loses the grouping.
- **A folder with an opacity below 1 or a blend mode other than Normal raises
  it to 3.** An older Umber folds that opacity into each child, and a group of
  overlapping children then composites differently — a document that opens
  showing something else. That is exactly the masks-and-clipping case that took
  the version to 2.

So `required_version` gains a clause, and a document of pass-through folders
still declares 1 or 2 and still opens everywhere. **Do not raise the version for
folders as such** — raise it for the folders that need it.

Note also that other applications' ORA readers will flatten Umber's folders in
exactly the way an older Umber would, so the same argument makes an isolated
folder a real interoperability loss and `SaveWarning` is where it should be
said.

---

## 5. Undo

`PixelPatch` names a **slot**, and folders own no slot, so nothing about a
stroke's patch changes. Two things do:

- **The history file names a stack *position*.** `SaveHistory::new` maps slot to
  position at save time and `ImportedDocument::open` maps it back. With folders
  in the `Vec`, "position" has to mean position among the entries that have
  slots — i.e. the flattened layer order — or every entry in a document with a
  folder is off by the number of folders below it. Whichever is chosen, the
  manifest's fingerprint has to include enough to catch a file where the two
  disagree, because a history replayed into the wrong layer is the worst failure
  in that module.
- **`history::VERSION` does not have to move**, and this is worth stating
  because it looks as though it should. The entries themselves do not change
  shape; only the position mapping does, and it is computed at save and at load
  from the stack that is actually there. A revision-3 reader given a revision-3
  file with folders resolves positions against the stack it just built. What
  *would* force a bump is storing the folder structure inside the history, which
  nothing needs to do — folders are not undoable, for the same reason adding and
  deleting layers is not.

Deleting a folder deletes its children, which frees their slots, which **clears
the undo history** for exactly the reason deleting one layer does. No new rule;
the existing gate in `App::delete_layer` just has more to remove.

---

## 6. The list, the drag, and the thumbnail

- **`layerdrag.rs` has two hit tests today** and would need a third. A press must
  land strictly inside a row and a drop rounds to the nearest boundary; a drop
  *into* a folder is a third question — the horizontal position, or a dwell —
  and it has to be decidable in the model with no drawing in it, like the other
  two. The rule that rows are read by their carried index and never by their
  position in the slice becomes more important, not less.
- **A collapsed folder hides rows.** The list draws the stack upside down and
  reads rows by carried index, so this is a filter on what is drawn, not a
  change to the model — but `layerdrag::Row` must then carry the *stack* index
  of a row that is not at the drawn position, which is what it already does.
- **A folder's thumbnail** is the interesting one. The honest answer is the
  composite of its children, which is a third mode for `thumbnail.wgsl` — it
  reduces one slice, and a folder has none. The cheap answer is the folder icon
  and no picture. Ship the cheap answer first: a thumbnail that showed one
  arbitrary child would be a picture that lies.

---

## 7. What would be built, in what order

1. The model: `depth` and `folder` on `Layer`, `slot` as an `Option`, the
   subtree helpers, `reorder` carrying a folder's run, the tick cascade. All
   testable with no GPU and no window, and this is most of the risk.
2. Pass-through folders end to end: create, collapse, drag into and out of,
   visibility folded in `Editor::layer_draws`, written as nested `<stack>` and
   read back as a tree. **No shader change, no version bump.** This is a
   complete, honest feature and is where it should first ship.
3. Isolated folders: the accumulator stack in `composite.wgsl`, the markers in
   `LayerDraw`, the depth cap enforced in core, the GPU test above, and the
   `required_version` clause. This is the part that must not be rushed.

The controls for a folder's opacity and blend mode must not be drawn until step
3 — the design rule about disabled controls with an explanation applies
directly, and a folder whose opacity slider does nothing is exactly the control
that lies.

---

## 8. How far this got

Nothing of the above is built. What *is* built, in the same round of work, covers
part of what folders get reached for:

- ticking several layers and showing, hiding, locking, unlocking, linking or
  deleting all of them at once;
- link groups, so several independent sets of layers each travel through the
  stack together, each drawn in its own colour;
- thumbnails that show what is on a layer, which is most of why a long flat
  stack is hard to read in the first place.

What is left is nesting, collapsing, and a folder's own opacity and blend mode
applying to its contents.
