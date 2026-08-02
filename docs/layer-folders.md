# Layer folders

Umber's stack nests. A folder is a container: it holds layers, it travels with
them, its eye and its lock reach everything inside it, and ticking one ticks its
contents. What a folder does **not** have is an opacity or a blend mode of its
own, and that half is not built.

This document is what exists and why it is shaped as it is, followed by the
design for the half that is not — so that whoever picks that up is not starting
from the beginning, and so that nobody picks it up thinking it is small.

**The short reason the second half is separate:** a folder a painter can put
opacity or a blend mode on is *group compositing*, and group compositing cannot
be expressed in `composite.wgsl` as it stands. Everything in the first half —
the model, the file, the drag — was work; that part is a change to the one
shader four other code paths reuse, plus a file-format revision. A folder that
composites wrongly, or that writes files an older Umber opens showing a
different picture, is worse than no folders at all, and CLAUDE.md says so about
narrower things than this.

---

## 1. What a folder has to do

Three things, in rising order of cost:

1. **Contain layers**, so the list can be collapsed and a stack of forty is
   readable. Costs nothing but the model and the list.
2. **Move as a unit**, so dragging the folder takes its contents. Reordering is
   a `Vec` shuffle, so this is nearly free — it is what link groups already do,
   with a different rule for which entries come along.
3. **Apply its own visibility, opacity and blend mode to what is inside it.**
   Only the first of the three is free.

Visibility is the cheap member of (3) and is worth separating out: hiding a
folder is exactly "hide every layer in it", because visibility is a boolean and
`hidden ∧ anything = hidden`. Locking is the same shape. Opacity is not, and
this is the whole difficulty: a folder at 50% over two *overlapping* children is
not the same picture as two children each at 50%. Where they overlap, the second
is composited over the first and only then faded; halving each of them lets the
lower one show through the upper one. Blend modes are worse again — a folder set
to Multiply must multiply the group's *result* into the backdrop, not each child
separately.

Photoshop and Krita spell this distinction **pass-through** versus **isolated**.
A pass-through folder is only a container: its children composite into the same
accumulator everything else does, and the folder's opacity and blend are
disabled or ignored. An isolated folder composites its children into an
accumulator of their own, then composites that result into the backdrop with the
folder's own opacity and blend.

**Every folder in Umber is pass-through**, which is why none of the rest of the
engine changed. Points 1 and 2 and the visibility half of 3 are what shipped.

---

## 2. The model, as built

`umber-core::layer`. `LayerStack` is still a `Vec<Layer>` with an
`active: usize`, and every API in the codebase still indexes it by position. A
real tree (`enum Node { Layer(Layer), Folder(Folder, Vec<Node>) }`) reads better
and would have broken everything: `get`, `active_index`, `reorder`, `remove`,
`layer_draws`, the autosave's snapshot, `SaveHistory`'s position mapping and
`layerdrag`'s rows all take a flat index.

So `Layer` gained:

```rust
/// How deeply nested this entry is. 0 is the top level.
pub depth: u8,
/// A folder folded shut, so the list draws its row and not its contents.
pub collapsed: bool,
/// This entry is a folder: it holds no pixels, and owns the run of entries
/// immediately below it whose depth is greater than its own.
folder: bool,
```

and `slot` became an `Option<u32>`.

### A folder is above its own contents

This is the one thing here that is easy to get backwards, and it took a
correction: an earlier draft of this document said a folder owned the entries
*above* it. It does not. A folder's subtree ends **at** the folder and begins at
the lowest entry of the run beneath it. Three things agree on that and none of
them would tolerate the opposite:

- **The panel.** A layers list is drawn top-first and a group's row sits above
  the layers in it, so the folder has the *higher* stack position.
- **The file.** ORA's first stack element is the uppermost, so writing the stack
  top-first emits `<stack>` and then the layers inside it — which is exactly a
  folder whose entry comes before its contents in that direction.
- **The composite.** Walking bottom to top, a folder's contents arrive first and
  the folder last, so the folder entry is the natural place for a "close the
  group" marker to sit when there is one to write.

`LayerStack::subtree` is the single place containment is computed. Everything
that has to move, delete, hide, lock, tick or draw a folder's contents asks it,
and `ancestors_of` is the walk in the other direction.

### Well-formedness

Not every sequence of depths describes a tree, and the one that does not is a
layer nested inside no folder. Rather than reason about each mutation
separately, **every structural change builds the depth sequence it would produce
and runs `well_formed` over it before committing.** The stack is at most
`LayerStack::MAX` entries, so that costs nothing, and it means `reorder_to`
cannot be made to invent a state the drawing code has no way to draw. A refusal
changes nothing, because nothing has been written when the judgement is made.

`can_reorder` is that same plan without the writes, which is what the drag asks
before it lights a row up — a mark promising a move the model will then refuse
is the lying control the drop rules already exist to prevent.

`MAX_DEPTH` is 7, eight levels. It is not a limit the model needs; it is what a
bounded group stack in a fragment shader will need, and a document nested deeper
has to be refused where somebody can be told rather than in a shader with
nowhere to report it. An *import* that is too deep is straightened rather than
refused (`flatten_ill_formed`, called once, in `ImportedDocument::open`): the
pixels are all there either way, and only the grouping changes.

### One move, two ways of saying "these travel together"

`reorder_to(from, to, depth)` is the whole of moving. A link group carries every
other layer of *that group*, a folder carries what is inside it, and they
compose: `moving_with` seeds from the link group and expands every seed to its
subtree. `depths_at` then shifts each member by the delta of *its own root*, so
a folder dragged into another folder keeps its shape rather than flattening.

Two refusals are worth naming. A folder cannot be dropped inside its own
subtree — checked against `subtree(from)` and deliberately **not** against the
whole moving set, because a link group's members are not inside one another and
dropping one on another of them is an ordinary move to that end of the stack.
And a drop that reproduces the order and the depths the stack already has is not
an edit, however it was expressed.

### Slots, and why folders cost the undo history nothing

A folder holds no slot. `Layer::slot` is therefore an `Option`, and that was the
honest choice rather than a slot a folder held and nothing wrote to: the second
is a lie the autosave would find, since it reads every slot back and would write
the file a blank layer nobody made — and on a 10000² canvas it would cost 400 MB
of texture per folder.

Because no slot changes hands, **grouping, re-nesting and folding do not clear
the undo history**, for exactly the reason reordering does not. Deleting a
folder deletes its contents, which frees their slices, so *that* clears the
history for exactly the reason deleting one layer does. The lock gate on a
delete is over the whole subtree: half a deletion is not a state to leave a
stack in.

---

## 3. The composite pass, which did not change

`composite.wgsl` was not touched, and the reason is the whole argument for
pass-through folders being a coherent thing to ship on their own: a folder is
exactly its contents composited in place. `Editor::layer_draws` flattens folders
out and folds each entry's `effective_visible` in; the shader never learns they
exist.

The consequence to hold on to is that **a draw's position is not a stack
position** once a document has folders in it. `Editor::active_draw_index` is the
mapping, and it answers `u32::MAX` for a selected folder — deliberately not "the
layer below it", which is a real draw and would preview the stroke on a layer
nobody chose.

`export_rgba`, `pick_colour`, `probe_canvas` and the autosave's capture all
reuse that pass and all needed nothing, because they take a `&[LayerDraw]` and
pass it through.

Clipping still reaches across a folder boundary, and that is correct rather than
overlooked: a pass-through folder *is* its contents in place, so a clipped layer
at the bottom of a group answers to whatever unclipped layer is beneath the
group. Isolated folders would have to change that — see §7.

---

## 4. The file, which did not move a version

ORA has nested `<stack>` elements, so the format carries folders natively and
Umber's reader already understood them — `docimport::openraster::parse_stack`
kept a `Vec<Group>` of open stacks and folded each group's opacity and
visibility into the layers inside, flattening the tree. Writing folders is
emitting the nesting that reader already parsed; reading them is keeping the
tree instead of folding it away.

**`umber-version` did not move**, and `required_version` says nothing about
folders. The rule is that a version is raised when an older build would drop
something and show a picture that is *wrong*, not merely plainer. An older Umber
— or GIMP, or MyPaint — flattens a pass-through folder and folds its visibility
in, which is precisely what a pass-through folder means. The picture is
identical; the painter loses the grouping.

That is only true because the folder's tag carries a name, a `visibility` and
`umber-lock` and **no `opacity` and no `composite-op`**. A group opacity is the
one thing a flattening reader cannot reproduce, and writing one is exactly what
would earn the bump. `a_document_of_folders_still_declares_the_revision_it_needs`
is the guard, and it checks the absence of both attributes as well as the
version.

What is still folded in on the way *in* is a group's opacity, where some other
application wrote one — with `GroupOpacityFolded`, as before. Its visibility is
not folded any more: it lives on the folder now, and folding it in as well would
hide the layers twice, so a painter who opened the folder again would find every
layer in it still individually shut for a reason nothing in the file said.

---

## 5. Undo

`PixelPatch` names a **slot**, and folders own no slot, so nothing about a
stroke's patch changed. The history file names a stack *position*, and the
decision there was:

**Positions count every entry, folders included.** `SaveHistory::new` maps a
slot to a position over the whole stack, the manifest's layer-name fingerprint
is built the same way, and `ImportedDocument::open` maps back over the whole
stack. A folder holds no slot so it can never be what a patch resolves to; what
it does is occupy a position, which is exactly why it has to be counted. A
position that resolves to a folder drops the whole history, for the same reason
one out of range does.

The alternative — positions among the layers that *have* slots — would have
worked too and was rejected for being two rules: the fingerprint would then have
had to exclude folder names, and a file whose folders were dropped by some other
application would have passed the fingerprint while meaning something else.

**`history::VERSION` did not have to move**, and this is worth stating because
it looks as though it should. The entries do not change shape; only the stack
they are resolved against does, and that is computed at save and at load from
the stack that is actually there. A build reading revision 3 given a file with
folders builds a flat stack, so the *name* fingerprint no longer matches and the
history is discarded — the picture opens whole, which is honest degradation.

`a_history_survives_a_document_that_has_folders_in_it` is the guard, and it is
deliberately built with a folder *below* the patched layer so that a mapping
which ignored folders would be off by one.

---

## 6. The list, the drag, and the thumbnail

- **`layerdrag` gained a depth.** A drop now says two things — where in the
  order, and inside what — and `Aim` carries both, so the mark on the list and
  the move that happens stay one answer. The nesting is the pointer's `x` read
  against `metrics::LAYER_INDENT`, one level per step, capped by what the target
  row can hold: one level *into* a folder, and no deeper than its own level for
  anything else. Dragging to the far left of the list is how something comes out
  of a folder.
- **Legality is asked, not restated.** `Drag::aim` takes a predicate, which
  `panels.rs` fills with `LayerStack::can_reorder`. A refused drop lights nothing
  up rather than falling back to a depth nobody asked for: the way to say
  "beside the folder instead" already exists and is one the painter is holding.
- **A collapsed folder hides rows**, and that is a filter on what is drawn and
  nothing else. The model is untouched, so a fold can never change the picture.
  It is not written to the file, for the reason a tick is not plus one of its
  own: a fold that survived a save would be a state somebody had to undo before
  they could see their own painting.
- **A folder's row draws a folder mark and a chevron**, where a layer draws its
  picture. The honest thumbnail is the composite of its contents, which is a
  third mode for `thumbnail.wgsl` — it reduces one slice, and a folder has none.
  One arbitrary child would be a picture that lies about what the group holds.
- **The autosave snapshot has its own `pixel_index`.** A folder is read back as
  nothing, so the capture is shorter than the stack; a positional zip would pair
  every layer above a folder with the pixels of the one below it and truncate
  the top of the stack away.

---

## 7. What is not built: a folder's own opacity and blend mode

This is the part that needs care, and none of it exists.

### Pass-through and isolated have to coexist

**This is the correction the rest of this section is built on, and the first
draft of this design missed it.** It is not enough to give every folder an
accumulator of its own and set the opacity to 1 where nobody asked for one.

Source-over is associative, so a group at opacity 1 in Normal mode composites
identically whether its children go into their own accumulator or straight into
the stack's. That much is safe. What is *not* safe is a child with a blend mode:
a Multiply layer inside a group would multiply against the group's own
accumulator — which starts transparent — instead of against the backdrop below
the group. That is precisely the difference Photoshop and Krita call
pass-through versus isolated, and turning every folder isolated would silently
change every document this build has already written.

So both have to exist:

- **Pass-through** folders keep doing exactly what they do now: flattened away
  in `Editor::layer_draws`, absent from the shader, absent from
  `required_version`, and still opening plainly in every older reader.
- **Isolated** folders become entries in the shader's array and get the
  accumulator treatment below.

A folder is isolated **iff its opacity is below 1 or its blend mode is not
Normal**. Deriving it rather than storing an explicit flag is what keeps this in
step with the version rule — the same test decides whether the file needs
revision 3 — and it means no new field, no migration, and no way for the two to
disagree. The cost is that a folder cannot be isolated *and* transparent-looking
at opacity 1, which Photoshop offers and nobody has asked for; if it is ever
wanted, the flag and the version clause have to move together.

Note the consequence for clipping. Today a clipped layer at the bottom of a
group answers to whatever unclipped layer is beneath the group, and that is
correct for a pass-through folder. Inside an *isolated* one it would have to
answer to nothing, which is a behaviour change confined to folders that did not
previously exist — but it does mean `clip_alpha` has to be pushed and popped
with the accumulator rather than left running.

`composite.wgsl` walks `layers: array<vec4<f32>, MAX_LAYERS>` bottom to top into
one `acc`, carrying a running `clip_alpha` for clipped layers. Two properties of
that loop are load-bearing and are what an isolated folder disturbs:

- **`acc` is a single accumulator.** An isolated group needs its own.
- **`clip_alpha` is a single running value.** A clipped layer would then have to
  answer to the nearest unclipped layer below it *within the same group* —
  clipping across a folder boundary is not a thing any application allows — so
  it has to be saved and restored with the accumulator. Note this is a
  *behaviour change* from what ships today, where clipping does reach across a
  boundary and is correct to.

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

**Where the open and the close come from.** A folder sits *above* its contents,
so walking bottom to top the contents arrive first and the folder last. The
folder entry is therefore the **close**, and it is what carries the opacity and
the blend mode to composite the finished group with. The **open** is not an
entry at all: it is a depth increase. Carry each entry's depth in the array and
push an accumulator for every level `depth[i]` exceeds the running depth. An
*empty* isolated folder then closes a group that was never opened, so the pop
must be guarded on the running depth actually being deeper — and an empty group
composites nothing, which `composite_over` already handles for a transparent
source.

Four notes on making that real:

- **The entries can be encoded in the array that exists.** `extra[i].w` is
  unused (`(mask slot, has mask, clipped, unused)`), so the depth fits with no
  new uniform and no new binding — and a folder needs no flag of its own beside
  it, because it is the entry with no slot: writing `-1` into `layers[i].z`
  says so, and every real slot is non-negative. Whatever is chosen, the Rust
  `#[repr(C)]` struct and the WGSL one still have to agree byte for byte; see
  CLAUDE.md's "Uniform layout".
- **The markers must travel in `LayerDraw`.** `export_rgba`, `pick_colour`,
  `probe_canvas` and the autosave's capture take a `&[LayerDraw]` and pass it
  through, so as a field they stay untouched; as a second argument to
  `composite` there are four more places for an export to stop matching the
  screen.
- **`MAX_GROUP_DEPTH` is already enforced**, as `LayerStack::MAX_DEPTH`. That
  part is done.
- **`LayerStack::MAX` already counts folders**, so the uniform array is already
  sized for them. That part is done too.

Two GPU tests to write, and the second is the one that would otherwise be
found by a painter:

- **The point of the feature.** Two overlapping opaque children in a folder at
  50%, against the same two children at 50% each outside a folder. The two must
  differ, and the first must equal the flattened-then-faded result.
- **The thing that must not change.** A Multiply child inside a *pass-through*
  folder must composite exactly as it does with no folder at all. That is the
  regression the pass-through/isolated split above exists to prevent, and it is
  invisible in any test built only out of Normal layers.

`composite_pixel` in `gpu_pipeline.rs` is the harness for both.

Also worth pinning without a GPU: `Editor::active_draw_index` has to count the
isolated folders that now *do* reach the draw list, where today it counts only
layers. That mapping is already the one thing in this area that fails silently.

### The file

`required_version` gains a clause: **a folder with an opacity below 1 or a blend
mode other than Normal raises `umber-version` to 3.** An older Umber folds that
opacity into each child, and a group of overlapping children then composites
differently — a document that opens showing something else. That is exactly the
masks-and-clipping case that took the version to 2. A document of pass-through
folders must go on declaring 1 or 2 and go on opening everywhere.

The writer then emits `opacity` and `composite-op` on the `<stack>` — which it
deliberately does not today — and the reader stops folding a group's opacity
into the children and puts it on the folder instead. That fold and its
`GroupOpacityFolded` warning stay for the case they were written for: a `.kra`
or an ORA from another application, read by a build whose folders can carry an
opacity, still needs no fold; but one nested deeper than `MAX_DEPTH`, whose
group is flattened away, does.

Other applications' ORA readers flatten Umber's folders in exactly the way an
older Umber would, so the same argument makes an isolated folder a real
interoperability loss and `SaveWarning` is where it should be said.

### The interface

The controls must not be drawn until the engine is there — the design rule about
disabled controls with an explanation applies directly, and a folder whose
opacity slider does nothing is exactly the control that lies. The place they
would go already exists: `panels::layers_body` draws a folder's row of controls
separately from a layer's, and today that row is one lock and a sentence saying
a group has no blend mode.
