# Transforming a linked set

> **The slot ceiling moved after this was written.** `LayerStack::MAX_SLOTS` is
> **256**, not 129 — it is the device's `max_texture_array_layers` guarantee
> rather than `MAX * 2 + 1`, and 127 of it is reserved as the layer-effect
> budget. Growth is no longer plain doubling either: it doubles inside a byte
> budget and rounds to a quantum past it. Every figure below that names 129, or
> describes growth as doubling, needs re-deriving before it is acted on; the
> arguments do not change, the arithmetic does. See `docs/layer-effects.md`
> §6.3 and `grown_capacity` in `canvas.rs`.

Link groups already carry a set of layers together **through the stack**: drag
one member of a group in the layers panel and the others come with it, because
reordering is a `Vec` shuffle and shuffling six entries costs exactly what
shuffling one does. They deliberately do not carry a set through a **transform**,
and the README says so rather than letting the flag half-work.

This document is the design for the half that is missing. The obstacle is stated
in CLAUDE.md and it is the thing everything below has to answer:

> `Float` holds one layer's pixels, one base, one bind group, and
> `EditBody::Pixels` holds one patch — so moving several at once needs N of each
> *and* an entry holding several patches, or an undo would step through a
> multi-layer move one layer at a time and leave the document in states it was
> never in.

Both halves are tractable. The first is a shape change in `CanvasRenderer` with
a **hard resource ceiling behind it** that has to be named out loud, and the
second is a change to `EditBody` that another piece of work — structural undo —
needs from the other direction, so the two must be agreed rather than each
invented separately. §6 is written to be read alongside `docs/structural-undo.md`.

---

## 1. What "several at once" has to mean

Three properties, and the third is the one that makes this larger than it looks.

1. **One box, one map.** The artist grabs one handle and every member follows.
   There is one `Transform`, not N of them — a set whose members could drift
   apart is not a set.
2. **The preview is the commit.** `render_float` is one function called twice,
   with the preview slice as the target the first time and the layer's own slice
   the second, and that is what makes what the screen showed byte for byte what
   gets written. N members must be N calls to that same function, not a new one.
3. **One undo entry.** Six layers moved and then undone must pass through
   exactly two states: before and after. Six entries would step through four
   states the document was never in, and a jump that landed in the middle of
   them would leave a picture nobody made.

(3) is the reason this cannot be built as "call the existing float machinery N
times and hope". Everything else composes; the history does not.

---

## 2. The set: which layers travel

Two candidates exist in the model and they answer different questions.

- **`Layer::link`** — `Option<u8>`, six groups, bounded by
  `theme::Palette::link_colours` because a group is told from its neighbours by
  the colour of the chain on its rows. A link is a **standing statement about
  the picture**: these layers belong together and travel together.
- **`LayerStack::targets`** — "every ticked layer, or the selected one alone when
  nothing is ticked". This is the rule for what a **bulk operation** reaches, and
  it is total by construction so no caller has to special-case an empty tick
  list.

**A canvas transform follows the link group, and not the ticks.** The argument is
about which gesture each belongs to:

- A tick says *what I am about to do*. Every caller of `targets` today is a
  button on the ticked strip — an explicit command, pressed on the frame the
  ticks are visible. A press on the canvas with the transform tool in hand is not
  a command; it happens every time the pen goes down. Ticks left over from
  hiding four layers three minutes ago would then silently move those four
  layers, and the artist would have no reason to look at the layers panel to find
  out why.
- A link is a *persistent mark on the rows*, in a colour, that already means
  "these move together". Extending it from the stack to the canvas gives one mark
  one meaning. Two spellings of "these travel together" — one for the list and
  one for the canvas — is exactly the drift this project refuses elsewhere.
- It is already written. `LayerStack::moving_with` is the function the layer drag
  uses, and it composes the two rules that exist: a link group carries every
  other layer of *that* group, and a folder carries its subtree. A transform that
  asks the same function cannot disagree with a drag about what a set is.

`moving_with` is private and takes a stack index. It becomes public, or gains a
public sibling that filters its answer to the entries that hold a slot:

```rust
/// The layers a canvas transform of the entry at `index` moves, as slots.
///
/// `moving_with` expanded through folders and then filtered to entries that
/// hold pixels: a folder has no slot, so what it contributes is its contents.
pub fn transform_set(&self, index: usize) -> Vec<u32>
```

Three consequences worth stating:

- **A folder in a link group contributes its contents.** That falls out of
  `moving_with` and needs no rule of its own. It is also the only way a painter
  reaches "move this whole group of eight" without linking eight rows.
- **An unlinked layer answers with itself**, which is exactly today's behaviour.
  Nothing about a document with no links changes.
- **A paste stays single-layer.** A clip is one picture; there is no set for it
  to be pasted into. `begin_float`'s paste path is untouched, which also means
  its "the layer is locked" notice is untouched.

### The alternative, stated fairly

`targets` has one real advantage: moving three layers together needs no
preparation, where linking refuses a group of one and therefore refuses to be a
convenience for a single ad-hoc move. And a *third* option — "the link group, or
the ticked layers when the active layer is not linked" — would give both.

It is refused because it is two rules for one question, and the failure it
produces is silent: the artist cannot tell, from the canvas, which of the two
answered. One rule, with a visible mark, is worth the extra gesture of linking.

---

## 3. One map, N sets of pixels

`Float` today is one struct holding both halves — what belongs to the *gesture*
and what belongs to the *layer* — because with one layer there was no difference.
There is now, and splitting it is what stops the members drifting apart:

```rust
/// A floating transform. One map, N sets of pixels.
struct FloatSet {
    /// The map, as the shader reads it. **One buffer for the whole set**,
    /// because there is one `Transform`: writing it per member would be N
    /// copies of one fact, and `write_buffer` is staged, so N writes into one
    /// encoder would all read back as whichever was written last anyway.
    uniforms: wgpu::Buffer,
    /// The selection's coverage, uploaded once and shared by every member's
    /// bind group. The selection is per-document; a copy per member would be
    /// N uploads of one texture and N chances to disagree about the edge.
    mask: wgpu::Texture,
    /// Where the previous preview landed. Shared for the same reason the
    /// uniforms are: one map means one previous destination.
    last_dest: Option<PixelRect>,
    members: Vec<FloatLayer>,
}

struct FloatLayer {
    layer_slot: u32,
    preview_slot: u32,
    /// This layer with the lifted region taken out. Canvas-sized.
    base: wgpu::Texture,
    /// This layer's floating pixels at identity.
    source: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}
```

`render_float` keeps its body and gains a `&FloatLayer`. `draw_float` and
`commit_float` become loops over the members, and **the uniform write moves out
of `render_float` and into them** — written once, then N passes. That is not
tidying: it is what makes "there is one map" structural rather than a thing five
call sites have to keep true.

Everything downstream still holds:

- **The preview cannot disagree with the commit**, for exactly the reason it
  cannot today: the commit is the same function with a different target slice,
  now called N times instead of once.
- **`composite.wgsl` is untouched.** `float_preview` becomes a slice of
  `(layer_slot, preview_slot)` pairs instead of one, and `Editor::layer_draws`
  looks each layer's slot up in it. The shader still sees a stack of slots and
  still does not know a transform exists.
- **The interface needs nothing.** `ui.rs` reads `float.xf` and nothing else —
  the box, the eight handles, the rotation mark and the flip pair are all
  functions of the map. One box for six layers is the correct picture and is
  what the existing code already draws.

### The lift, and what it does *not* share

A lift is `min(a, m) / a` — the share of the alpha that is there which lies
inside the selection — and `a_lift_leaves_no_ghost_of_the_selection_it_was_
painted_through` guards it. Nothing about that arrangement is global, and it is
worth being explicit, because it looks as though it might be:

- `fs_mask` reads **the layer's own slice** for `a`, so each member computes its
  own share from its own pixels. A layer that is empty inside the selection
  yields a share of `0/eps = 0` and contributes nothing to either pass — which is
  correct and needs no branch.
- The keep pass and the take pass write **that member's** floating copy and base,
  which are that member's own textures, so the "a colour attachment may not be
  sampled in the pass that writes it" constraint is satisfied per member exactly
  as it is today.
- The `mask_bind_group` differs from the drawing bind group only in binding 1 —
  the layer's slice rather than the floating copy — so each member holds two bind
  groups over the **shared** uniform buffer and the **shared** mask texture.

So the lift is N independent arrangements of one shape, and the only sharing is
of things that genuinely are one thing.

### Submissions

`begin_float` submits twice today, and deliberately: `Queue::write_texture` is
flushed before the command buffers of the submission it precedes, so a paste
cleared in the same encoder is wiped. **This must stay two submissions for the
whole set, not 2N.** Clear every member's floating copy in one encoder, submit;
then do every member's copies and mask passes in a second encoder, submit. A
lift writes no texture at all, so a lift could be one submission — leave it at
two rather than branching, since it is once per gesture either way.

---

## 4. Spare slices, which is the binding constraint

This is the part that decides how large a set can be, and the answer today is
**one**.

```
LayerStack::MAX        = 64        stack entries, folders included
LayerStack::MAX_SLOTS  = 64*2 + 1  = 129 texture-array slices
```

The `+ 1` is the float's preview slice, and
`the_slot_ceiling_covers_a_fully_masked_stack_and_the_floats_spare` says so in
as many words: a full stack of 64 masked layers needs 128 slices and the ceiling
leaves exactly one over. So the ceiling as it stands supports a set of **one
layer**, and a link group is not bounded at all — `LayerStack::link` refuses
fewer than two members and imposes no maximum, so a group can be the whole stack
of 64.

### What a slice costs

A slice is a full canvas of `Rgba8UnormSrgb`:

| canvas | one slice | a float today (base + source + preview) |
|---|---|---|
| 2048² | 16.8 MB | 50.3 MB |
| 4096² | 67.1 MB | 201 MB |
| 10000² | 400 MB | 1.20 GB |

The right-hand column is what one floating transform already costs, and it is
unbudgeted: nothing checks it, and `ensure_slots` grows the array by allocating
a new `LayerStore` and copying into it. **An allocation that fails there is a
wgpu error, and `crash::device_error` makes an uncaptured device error fatal.**
So the failure mode of "too many floats" is not a refusal; it is the crash
reporter. That is true of one float today on a large enough canvas, and this
feature multiplies it by N.

### The proposal

**Cap the set, and make the cap structural.**

```rust
/// The most layers one transform may carry.
pub const TRANSFORM_SET_MAX: usize = 8;
```

and raise the slice ceiling to match:

```
LayerStack::MAX_SLOTS = 64*2 + TRANSFORM_SET_MAX   = 136
```

Raising `MAX_SLOTS` allocates nothing: `INITIAL_SLOTS` is still four and growth
still doubles, capped at the ceiling, so a document with no masks and no float
pays exactly what it pays now. What the ceiling bounds is the worst case, and the
worst case is 8 preview slices — 537 MB at 4096², 3.2 GB at 10000².

Eight, and not more:

- A hand-built rig — line, flats, shade, light, glow, background — is six. Four
  is typical. Eight has headroom for both without pretending a 64-layer link
  group is a gesture anybody makes.
- It bounds the peak at 8 × 3 canvas-sized textures = 1.61 GB at 4096², which is
  already the wrong side of comfortable and is the whole argument for §10's
  stage 6.
- It is small enough to be a **fixed-capacity array**, which is what keeps
  `Editor::Floating` `Copy` — see below.

**A set larger than the cap is refused whole**, with a notice naming the number.
Not truncated: a link group half-moved is precisely the "flag half-working" the
README refuses, and the artist would have to notice which two of their eight
layers were left behind. The precedent is the canvas flip, refused whole when any
layer is locked because "a half-mirrored picture is not a state the flip's
pixel-less undo entry can describe".

### `Floating` stays `Copy`

`Editor::Floating` is `Copy` today and four call sites read it by value
(`if let Some(float) = self.editor.float`). A `Vec<u32>` of slots kills that and
turns each of them into a borrow fight with `&mut self`. A fixed array does not:

```rust
#[derive(Clone, Copy, Debug)]
pub struct Floating {
    pub xf: Transform,
    /// The slots this transform carries, snapshotted at the lift for the same
    /// reason one slot is today: selecting another layer mid-gesture must not
    /// land the commit somewhere else.
    slots: [u32; TRANSFORM_SET_MAX],
    len: u8,
    pub lifted: bool,
    pub drag: Option<(Handle, Vec2)>,
}
```

Same argument `Brush`'s dynamics table makes for itself, and with the same
payoff: the cap stops being something a call site checks and becomes something
the type cannot exceed.

### A bug found on the way

`CanvasRenderer::begin_float` refuses when `reserved >= MAX_LAYERS` — 64 — where
the number it should be checking is `MAX_SLOTS`, 129. `reserved` is
`slot_capacity_needed()`, which counts masks and every slot ever handed out, so a
document with 33 masked layers reports 66 and **cannot float a transform at all**
today, with 63 slices free. The notice it raises ("this document's layers are
using every one Umber has") is then false. This is independent of the feature and
should be fixed in its own commit; the cap above assumes it is.

---

## 5. Damage: the same rectangle, and it need not be

`Transform::damage` is source ∪ destination, where the destination is the
bounding box of the *quad* plus a pixel of skirt — `half × sqrt(2)` for a turned
rectangle, and an edge left uncommitted redraws as a preview and is baked in by
the next edit.

**Geometrically it is the same rectangle for every member**, and this falls out
of §1's first property rather than being a coincidence:

- the destination is a function of the map, which is shared;
- the source is `Transform::source`, built from `Editor::transform_region()` —
  the selection's bounds, or the whole canvas where there is none — which is a
  property of the document, not of a layer;
- `lifted` is the same for the whole set, because a paste is single-layer.

So the commit is one rectangle and N patches. Worth a test that needs no GPU:
`a_linked_lift_damages_the_same_rectangle_on_every_layer`.

**It need not be, and the difference is expensive.** What actually changes on
member L is `(L's content ∩ source) ∪ (where L's pixels landed)`, and a layer
that is empty inside the selection changes nothing at all. Using the shared
rectangle means:

- N blocking `read_layer_rect` calls at pointer-up, each of the full rectangle.
  With no selection that rectangle is the whole canvas. Extrapolating from the
  one figure the autosave notes measure — 16 MB read in one go cost 5 ms — six
  full-canvas layers is about 126 ms at 4096² and about three quarters of a
  second at 10000². Re-measure before quoting either.
- N patches in the history. An **empty** layer's patch costs four bytes, because
  `PatchPiece::new` collapses an all-identical piece to one pixel — so the budget
  survives a rig with mostly-empty layers rather well. The *time* does not: the
  readback happened either way.

The honest fix is per-layer content bounds, and the machinery exists —
`thumbnail.wgsl`'s first pass reduces a slice to the greatest alpha per cell and
`umber_core::thumbnail::content_rect` turns that into a document rectangle. It is
driven asynchronously today, one job at a time, for a picture nobody is waiting
on; making it answer *at the lift* means a blocking reduction and readback per
member, which trades one cost for a smaller one at a moment the artist is also
waiting. **Not in the first version.** It is named here so that whoever finds the
commit slow knows where the answer is, and so that nobody concludes from §5's
first half that the rectangle is forced.

---

## 6. The undo entry — and the shape structural undo needs too

`EditBody::Pixels` holds one `PixelPatch`. A multi-layer transform needs one
entry holding N.

**`docs/structural-undo.md` is being designed at the same time and hits this from
the other side**: deleting a folder has to put several layers back in one entry,
or an undo steps through the deletion one layer at a time. The two features want
the same primitive and must not each invent one. This is the proposal to
reconcile:

```rust
pub enum EditBody {
    /// The pixels the edit replaced, one patch per layer it touched.
    ///
    /// A stroke and a single-layer transform carry one. A transform of a linked
    /// set carries one per member, applied and reversed **together**: six
    /// entries would step an undo through four states the document was never
    /// in.
    Pixels(Vec<PixelPatch>),
    Flip,
}
```

That is the whole of what a linked transform needs. What structural undo needed
*in addition* was the stack itself, and the slots — a delete used to free
slices, which is why it cleared the history. **That half is built**: a deleted
layer moves into the entry and owns its slot claim, so nothing is freed and
nothing is cleared, and `EditBody` gained a `Structure` arm rather than a
patch list. So the shared decision below was settled the other way — structural
undo stores no pixels at all, and a list of patches is this feature's alone:

> **An entry holds a list of patches, applied atomically, reversed by one call to
> `reverse`.** Whatever else a structural entry carries, it carries that list in
> the same form.

Consequences, each of which the transform half can carry alone:

- **`EditKind` gains nothing.** A multi-layer transform is a `Transform`: it
  moves pixels, and it undoes by putting rectangles of pixels back. Two rows that
  undo identically must not have two names — the same rule that keeps a paste
  filed under Transform and a cut under Erase. `panels::edit_icon` stays
  exhaustive over five variants and needs no new arm.
- **`Edit::patch()` becomes `Edit::patches()`**, and `byte_len` sums. The budget
  counts what it counted.
- **`swap_patch` becomes a loop.** One `read_layer_pieces` per slot, then the
  writes. That is N submissions and N waits where a single-layer undo is one —
  and it is worth noting that `read_layer_pieces` exists precisely because "a
  hundred and fifty calls to `read_layer_rect` would be a hundred and fifty
  fences". Giving it a slot per rectangle (`&[(u32, PixelRect)]`) would make a
  linked undo one submission again. Not required; named because it is the same
  argument that produced the function.
- **A jump costs N times as much.** `steps_to` turns a click on a row into that
  many single steps, so an eight-row jump over linked transforms is 8N blocking
  reads. That is fine on an explicit click, which is what the History module
  already says about a jump, and nothing on the drawing path reaches it.
- **The budget is the same figure and holds fewer entries.** A full-canvas
  transform of six layers on a 10000² canvas is 2.4 GB in one entry, over the
  512 MB default — `evict_to_budget` keeps it, because it stops at
  `undo.len() > 1`, and drops everything before it. That is the same behaviour a
  single 400 MB entry already produces, scaled, and it is the reason CLAUDE.md
  insists the limit be said out loud rather than looking like a regression.

### Slots are safe here, and that is the difference from structural undo

A `PixelPatch` names a slot, and slots were recycled, which is why *deleting* a
layer used to clear the history. A transform frees no slot and reassigns none —
the preview slices come from above `slot_capacity_needed()` and are given back
at `end_float` without ever entering the stack's free list. So a linked
transform costs the history nothing, exactly as reordering and grouping do.

Structural undo was the harder half precisely because it did not have that, and
its answer changes one number here: a parked slice keeps `slot_capacity_needed`
climbing, so the headroom a float competes for is smaller than this document
assumed. `LayerStack::live_slot_ceiling` is what a float should ask.

---

## 7. The file

**`umber-version` does not move.** Nothing about the picture changes: a
transform is pixels, already written as pixels, and a link is already written and
already ignorable — `umber-link-group` did not raise the version either, because
a build that ignores it shows the same picture with one link set instead of
three.

**`history::VERSION` does move, and only for a file that needs it.** A build
reading revision 3 handed an entry with a `patches` list would read the flat
`layer`/`x`/`y`/`w`/`h`/`pieces` fields, restore the first member and silently
drop the rest — a document left in a state it was never in, with the other five
layers' pre-transform pixels unrecoverable. That is exactly the revision-2 case
("a document quietly damaged by an undo") and it earns the bump to **4**.

But writing 4 unconditionally would make every history file unreadable by an
older build, including the overwhelming majority that hold nothing but
single-patch entries. So:

> **The manifest states the lowest revision it needs**, exactly as
> `docformat::required_version` does for the document: 3 when no entry carries
> more than one patch, 4 when one does.

The reader already refuses `manifest.version > fmt::VERSION` and drops the whole
history with a sentence, so both directions work with no new mechanism. A guard
test in the shape of `a_document_of_folders_still_declares_the_revision_it_needs`:
`a_history_of_single_layer_edits_still_declares_revision_3`.

The manifest change itself is a list beside the existing flat fields:

```rust
/// The other layers this entry restored, where it restored more than one.
///
/// `default` and skipped when empty, so a manifest of ordinary entries is byte
/// for byte what this module wrote before. The **first** patch stays in the
/// flat fields rather than moving into the list, so a single-patch entry is
/// unchanged on disk and revisions 1 through 3 go on reading exactly as they do.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub patches: Vec<ManifestPatch>,
```

Two rules the writer inherits and must keep:

- **An entry goes in whole or not at all.** `write` already breaks at the budget
  on an entry boundary; a multi-patch entry is one entry and its patches are one
  edit. Half of them would restore half a move, which is not a state the canvas
  was ever in.
- **`SaveHistory::new` refuses the whole history if any patch cannot be placed.**
  With N patches per entry it refuses on the first that cannot, unchanged: the
  entries are a sequence in which each restores the pixels the next expects.

---

## 8. The gates

Three, and each generalises rather than multiplying.

**A float exists only with the transform tool in hand, on the layer it came
from**, checked once per frame in `render` rather than at the rail, the
shortcuts, the layer list and the preset — an invariant enforced at five call
sites is one that will be forgotten at the sixth. With a set, "the layer it came
from" becomes membership:

```rust
if let Some(float) = self.editor.float
    && (self.editor.ui.tool != Tool::Transform
        || !float.holds(self.editor.layers.active_slot()))
{
    self.finish_transform();
}
```

Selecting *another member of the same set* must not put the picture down —
which is what `holds` buys and what a scalar comparison could not express.
Selecting a layer outside the set commits, exactly as today.

**A lock is refused at one gate per operation.** `begin_float` is that gate for
both a lift and a paste. With a set it asks whether **any** member is locked, and
refuses the whole lift. The precedent is the canvas flip, and the reason is the
same one: a set that moved except for the locked member is a picture nobody
asked for, and the artist's own statement — the chain — says these travel
together. Half of that is not a weaker version of it, it is a different thing.

The notice follows the existing split and needs no new rule: a **paste** says so,
because it is an explicit command with one obvious outcome, and a **canvas
press** stays silent, because it happens every time the pen goes down and a
dialog there is the failure the autosave's "say it once" rule is about. Since a
paste is single-layer, that half is literally unchanged.

**A folder cannot be lifted.** Already gated: `active_slot()` answers `None` for
a folder and `begin_float` refuses. Unchanged — and a folder *inside* a link
group is not this case, because `transform_set` expands it to its contents and
the folder itself never reaches the renderer.

**Everything that leaves the document commits first**, and that list is
unchanged: a tab switch, a save, an export, a resize, a close, a copy, a cut, a
paste, an undo, a redo, a canvas flip, adding a layer, adding a mask, grouping.
Each of them already calls `finish_transform`, and `finish_transform` becoming a
loop over the members changes none of them.

---

## 9. The interface

**Nothing new is drawn, and that is the right answer rather than an omission.**

`ui.rs` reads `float.xf` and nothing else: the outline, the eight handles, the
rotation mark and the flip pair are all functions of the map, and there is one
map. One box over six layers is the correct picture — it is what the artist is
aiming with, and drawing six boxes would say the six could be aimed separately.

What already says which layers are moving is the **chain in the layers panel**,
in the group's own colour, on every row of the set. That mark exists, it is
persistent, and it is the reason §2 chose links over ticks.

Two small additions are defensible and neither is required:

- The History row could read the member count. It would be honest — the entry
  really does hold six patches — and it is the kind of thing the History module's
  "a row must not promise more than the engine records" rule permits rather than
  demands.
- The refusal in §4 needs a notice, and it names the cap: "A transform carries at
  most eight layers at once; this link group has eleven."

---

## 10. A staged plan

Each stage is a commit, each leaves the tree green, and the risky one is named.

1. **Fix `begin_float`'s ceiling** (`MAX_LAYERS` → `MAX_SLOTS`). Independent of
   everything else, and it is a live bug: a document with 33 masked layers cannot
   transform at all today. One commit, one test.
2. **`LayerStack::transform_set`** — `moving_with` made public or wrapped,
   filtered to entries with slots. Pure `umber-core`, no GPU, tested against a
   link group containing a folder. Nothing calls it yet.
3. **`EditBody::Pixels(Vec<PixelPatch>)`**, `Edit::patches()`, `swap_patch` as a
   loop. Single-patch behaviour is unchanged and every existing test should still
   pass. **Agree the shape with `docs/structural-undo.md` before writing this
   one.** Add a model-based test in the shape of
   `stepping_back_over_a_flip_puts_older_patches_where_they_were_recorded` — a
   `Model` per layer — asserting that undoing a two-layer transform passes
   through exactly two states.
4. **The file.** `required_history_version`, the manifest's `patches` list, the
   reader, and `a_history_of_single_layer_edits_still_declares_revision_3`.
5. **`FloatSet` / `FloatLayer`** in `umber-render`: the split, the shared uniform
   buffer and mask, the uniform write moved out of `render_float`,
   `float_preview` answering a slice, `Editor::layer_draws` looking a slot up in
   it. **This is the risky stage.** Two reasons:
   - A wrong entry in the preview map composites one layer's preview in another
     layer's place. It is invisible to every CPU test, invisible in a still
     frame with two similar layers, and shows up as a picture that jumps at
     pointer-up. It wants a GPU test that lifts two layers, reads both preview
     slices *and* both layer slices back, and asserts the commit equals the
     preview on each.
   - `MAX_SLOTS` rises with no byte budget behind it, and the failure mode of
     running out is a device error and therefore the crash reporter, not a
     refusal. The cap in §4 is the whole of the mitigation and it is a count, not
     a size.
6. **The app**: the cap, the refusal and its notice, the lock gate over the set,
   the per-frame membership gate, `Floating`'s fixed array. Then extend
   `a_lift_leaves_no_ghost_of_the_selection_it_was_painted_through` to two
   layers, which is what pins §3's claim that nothing about the mask arrangement
   is global.
7. *(optional, later)* **Region-sized `base` and `source`.** Today each member
   holds two canvas-sized textures where only the lifted region differs from the
   layer, and outside the source rectangle the base *is* the layer — untouched
   until the commit, so the preview could restore from the layer's own slice and
   the commit could skip that part entirely. That takes a member from three
   canvas-sized textures to one, which is a 3× cut in the peak of §4 and grows
   with N. It is deferred because it changes `render_float`, the one function
   called twice, and because a region-sized `source` needs a transparent border
   texel to keep the moved edge's antialiasing that a canvas-sized one gets for
   free. Worth doing; not worth doing first.
8. *(optional, later)* **Per-layer damage** from `thumbnail::content_rect` (§5),
   and **`read_layer_pieces` across slots** (§6). Both are cost, not
   correctness.

---

## 11. What this deliberately does not settle

- **A layer's mask does not travel with its pixels**, and this design does not
  change that. `Editor::layer_draws` says so today in as many words — the mask is
  not swapped for the preview slice, because a floating transform moves the
  layer's pixels and not what hides them. Whether an artist expects a mask to
  move with the picture is a real question and a pre-existing one; it is not
  made worse by six layers and it is not answered here.
- **A clipped layer whose base is not in the set.** Moving the clipped layer
  alone changes what shows through, which is what clipping means; nothing breaks
  and nothing needs a gate. Named because it is the first thing that looks like a
  bug when somebody sees it.
- **A byte budget for float storage.** §4 caps a count, not a size, and on a
  10000² canvas eight members is 3.2 GB of preview slices. The right answer is a
  budget checked before `ensure_slots` grows the array, so the refusal is a
  notice rather than the crash reporter; that is a change to an existing,
  unbudgeted path and is worth its own piece of work.
- **Undoing a *partial* set.** There is none: the set is refused whole or moves
  whole, and the entry holds every member.
