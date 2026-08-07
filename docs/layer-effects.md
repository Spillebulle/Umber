# Layer effects

Non-destructive marks derived from a layer's own alpha and composited around it:
a stroke, a drop shadow, a glow. The layer's pixels are not touched, the
parameters stay editable, and the file carries the parameters rather than the
result.

Nothing here is built.

`docs/layer-folders.md` and `docs/group-compositing.md` are assumed: what a
folder is, why the stack is a flat `Vec`, and why `composite.wgsl` walks it in
one pass. §9.5 is a dependency on the second of those and is the one place this
feature cannot go alone.

---

## 0. The name, settled first

**`Stroke` is already taken, four times over.** `umber-core::stroke` is the brush
stroke; `StrokeBuilder` generates its dabs; `StrokeStyle` is what the preview and
the commit are handed; `stroke_tex`, `stroke_color`, `stroke_blend` and
`stroke_on_mask` are fields of the composite's uniform. A layer effect called
`Stroke` would collide with every one of them, in the same files.

So: **`Effect::Outline` in code, "Stroke" in the interface.** Painters know the
control by Photoshop's name and Krita's layer styles use it too, so the interface
must say Stroke; nothing in `umber-render` or `umber-core` may. This is the rule
`theme::text` versus `umber_core::text` already lives by — import the item, never
the module — written down before the collision happens rather than after.

---

## 1. What this changes, and what it must not

One sentence from `composite.wgsl` governs the whole design:

> The entire stack composites in ONE pass. Do not "simplify" this into a pass
> per layer.

and one from CLAUDE.md governs what may be added to it:

> `export_rgba`, `pick_colour`, `probe_canvas` and the autosave's capture all
> reuse the *screen* composite pass. A second copy of the blend maths would be a
> second thing to keep in step, and an export that differs from the screen is a
> classic bug.

Those four take a `&[LayerDraw]` and hand it straight through. So the test every
choice below is held to is: **can an effect be expressed as a `LayerDraw`?** If
it can, the compositing half of this feature is already written and all four
reuse paths are untouched. If it cannot, this feature grows a second compositing
path and the argument for the single pass has to be reopened.

It can. §4.

---

## 2. Where the pixels come from

Three shapes. The first two are refused.

### 2.1 Inline in the composite loop — refused

The loop takes one sample of one slice per layer per fragment. A stroke needs the
distance from this fragment to the nearest covered texel; a shadow needs a
weighted sum over a neighbourhood. Both are many-tap, and they would run inside a
loop that already iterates over the whole stack, at screen resolution, every
frame.

A 40 px gaussian is ~80 taps separated, and it cannot be separated inside a loop
that is not itself two passes. Unseparated it is ~5,000. That is not a constant
factor away from affordable.

Worth recording that **one effect really is pointwise**: a colour overlay is
`layer.a × colour`, one multiply, and would be free here. It is not worth a
mechanism of its own, and §3 folds it into the general shape at the cost of one
slice it does not need. If overlay ever turns out to be the effect people use
most, that is the moment to reconsider — not now.

### 2.2 Baked into the layer's pixels — refused

Destructive, and it throws the parameters away. That is the "apply and flatten"
menu command, which is a fine thing to have (§10.4) and is not this.

### 2.3 Rendered once into a cached slice, composited as its own draw

This is the design, and it lands on machinery that already exists:

- **The invalidation signal is already built and already exhaustive.**
  `CanvasRenderer::slot_revision` is bumped inside every method that writes a
  slice — commit, float commit, `write_layer_rect`, clear, mask fill, flip,
  resize — and CLAUDE.md calls that exhaustive *by construction*, because a
  layer's pixels cannot change without going through one of them. `Thumbs`
  already keys a cache off it. An effect cache is a second consumer of the same
  signal, plus a hash of the effect's own parameters. No `touch` call beside
  eight call sites, which is the failure that rule was written to prevent.
- **The composite already walks an array of draws** carrying a slot, an opacity,
  a blend mode, a visibility, a mask and a clip flag. An effect has every one of
  those and nothing else. §4.

So the work is producing the slice, not compositing it.

---

## 3. One derived quantity, three effects

The tempting shape is an implementation per effect. The better one is that a
stroke, a drop shadow and a glow are the **same pipeline with different
parameters**, and saying so is what keeps this from becoming four shaders that
round their edges differently.

Every one of them is built out of the layer's coverage — its alpha, after its
mask — by at most four steps:

| step | what it does | stroke | drop shadow | outer glow | inner shadow |
|---|---|---|---|---|---|
| **grow** | signed distance, thresholded at ±`spread` | the width | Photoshop's *Spread* | *Spread* | *Choke* |
| **soften** | separable blur of radius `softness` | the join | *Size* | *Size* | *Size* |
| **offset** | translate by (angle, distance) | — | the offset | — | the offset |
| **confine** | keep the part outside, or inside, the coverage | outside/centre/inside | outside | outside | inside |

That is the unification and it is worth stating plainly: **one signed distance
field and one blur serve all of them.** Build the two well and the effects are
parameter sets.

### 3.1 The distance field

A stroke of width *w* is the set of texels whose distance to the coverage edge is
under *w*. A shadow's spread is a dilate, which is the same field thresholded
elsewhere. So the field is computed once per layer per bake and read by every
effect on it.

**Jump flooding** is the method to use: `ceil(log2(r)) + 1` passes, each one full
screen, independent of the radius after the log. The alternatives lose:

- **A separated `max`** (horizontal then vertical) dilates to a *square*, not a
  disc. On a diagonal edge the corner is out by `r(√2 − 1)` — 41% of the radius.
  Visible on any stroke wide enough to want.
- **An exact disc** is O(r²) taps. At r = 20 that is 1,257 per fragment.
- **Blur and threshold** is cheap, reuses the blur, and rounds corners by an
  amount that depends on the radius — so a 2 px stroke is sharp and a 20 px
  stroke has visibly rounded corners the artist did not ask for. It is what
  several real implementations do and it is the fallback if jump flooding
  measures badly, but it should not be the first attempt.

Jump flooding's known weakness is that it is approximate for a small number of
seeds; against a dense coverage field it is essentially exact, which is this
case.

### 3.2 The blur

**A tent from two box passes per axis**, which is exactly what
`umber-core::selection`'s feather already is, for exactly the reasons stated
there: linear in the area whatever the radius, and separable.

Two things follow that are easy to get wrong:

- **The intermediate must be linear, not sRGB.** The layer array is
  `Rgba8UnormSrgb`. A separable blur that lands its horizontal pass in an sRGB
  target and reads it back for the vertical pass has quantised through a gamma
  curve in the middle, and the result is not the blur of anything. The mean of
  two gamma-encoded values is not the encoding of their mean — the rule the
  colour-pickup path already states. `LAYER_FORMAT_LINEAR` exists in the layer
  array's `view_formats` for the flip pass and is the view the intermediate takes.
- **A radius of zero must be the exact identity**, same bytes, no pass recorded.
  The rule the feather and the grain both keep.

**Whether the GPU blur and the CPU feather should agree numerically** is a real
question and the answer is that they should agree in *shape* and need not agree
in bytes. They act on different data through different code and no picture ever
shows both; requiring byte agreement would be a promise about rounding across two
implementations, which is what this codebase refuses to make elsewhere. Requiring
the same kernel is free and stops a shadow of radius 8 and a feather of radius 8
having visibly different falloffs.

### 3.3 Knockout, and the one asymmetry

An **outer** effect must not paint under the layer's own opaque pixels — with the
layer at 100% it makes no difference, and at 50% a drop shadow showing through
its own object is wrong. Photoshop spells this "Layer knocks out drop shadow" and
defaults it on.

The knockout is **baked**, not composited: the bake has the layer's coverage in
hand already, so multiplying the effect by `1 − coverage` costs nothing there.
Doing it at composite time would need an *inverse* clip, which the shader has no
notion of and would be a new mechanism for one case.

An **inner** effect is the opposite — it is confined *to* the layer's alpha — and
that is `LayerDraw::clipped`, which already means "bounded by the alpha of the
nearest unclipped layer below". An inner effect drawn immediately above its own
layer, with `clipped: true`, reads exactly the right value: `clip_alpha` is set
from the layer after its mask and after its wet stroke. **No new mechanism at
all.**

That asymmetry — outer effects bake their confinement, inner effects use the clip
flag — is the kind of thing that gets forgotten and reintroduced as a uniform.
It is written here so it is not.

---

## 4. An effect is a `LayerDraw`

Per layer, bottom to top:

1. drop shadow
2. outer glow
3. stroke, where it is outside or centred
4. **the layer**
5. stroke, where it is inside
6. inner shadow
7. inner glow
8. colour overlay

Each of 1–3 and 5–8 is one `LayerDraw`, carrying its own slot, its own opacity
and its own blend mode. That is what makes a shadow at Multiply multiply against
*the backdrop* — what is under the layer — which is what Photoshop's default
does and what baking the shadow into the layer's own slice could never
reproduce.

Only entries 1–3 and 5–8 that are enabled produce a draw. A layer with no effects
produces exactly the draw list it produces today, entry for entry, which is the
regression test that matters most and needs no device.

### 4.1 The effect slice is an ordinary layer slice, and the shader is untouched

The effect's pixels are premultiplied RGBA in a slice of the same layer texture
array a layer's pixels live in. Consequences:

- **`composite.wgsl` is not modified. At all.** No new binding, no new uniform
  array, no tint colour, no branch. This is worth a great deal: it is what keeps
  the four reuse paths untouched, it is what keeps the register pressure
  argument in `group-compositing.md` §2.4 from having to be reopened, and it is
  what makes stage 1 (§12) something that can be tested without a shader.
- **A gradient stroke or a gradient overlay costs nothing extra later.** The
  colour is in the pixels, so an effect that is not one flat colour is a
  different bake and the same draw.

**The alternative was an `R8Unorm` coverage slice tinted at composite time**, one
quarter of the memory. It loses. It needs a third `array<vec4<f32>>` in the
uniform for the colour — `group-compositing.md` §2.2 has the byte budget and it
is not free — a `select` in the hot loop, and it forecloses gradients. The saving
is real and is on the resource §6 says is bounded by region rather than by
format. Take the simple thing.

One detail the sRGB format forces: an effect slice is written through an
**sRGB** view, like a layer, so the bake's final pass encodes and the composite
decodes — correct, and the same round trip a layer takes. Only the blur's
*intermediate* is linear (§3.2).

### 4.2 The one slot that may be freed rather than parked

CLAUDE.md: deleting a layer **parks** its slice rather than recycling it, because
a `PixelPatch` names a slot and a patch replayed into a reissued slot would write
into another layer's pixels.

**An effect slot is the exception, and it is a real one.** No patch can ever name
it: effect pixels are derived, are never captured into the undo history (§7), and
are never read back. So turning an effect off puts its slice straight back on the
free list, with no undo-budget charge and no parked-slice arithmetic. That is the
difference between an effect slice and a mask slice — a mask is authored, is read
back, is saved, is patched and is therefore parked — and it is why the two are
not the same kind of thing despite living in the same array.

---

## 5. When the bake runs

`Thumbs`' policy is the model: a cache keyed by slot and validated against
`slot_revision`, plus — new here — a hash of the effect's parameters.

An effect is stale when the layer's slice revision has moved, when the mask's
has, or when a parameter changed. A stale effect is rebaked before the frame that
needs it.

### 5.1 The wet stroke is the hard case, and it decides §6

Painting on a layer that has a drop shadow: the shadow should follow the brush.
It cannot, straightforwardly. The stroke in flight lives in the scratch texture
and does not reach the layer slice until pointer-up, so an effect baked from the
layer slice is one whole stroke out of date, and the shadow snaps into place when
the pen lifts.

Three answers:

- **Bake at commit only.** Correct, cheap, and the shadow lags a whole stroke.
  Honest but poor: drawing an outlined shape and not seeing the outline until you
  lift is the kind of thing that makes a feature unusable rather than merely
  limited.
- **Bake every frame from layer + scratch.** Right on screen, and the cost is the
  whole effect pipeline at canvas resolution per frame. At 2048² that is a
  handful of full-target passes over 4 M texels and is plausibly ~1 ms; at
  10000², over 100 M texels, it is not.
- **Bake every frame, over the damaged region only.** The stroke's damage is
  already accumulated as a `damage::TileMask` on a 64-pixel grid, for the undo
  patch. Dilate that by the effect's reach — spread plus softness plus the
  offset — and the rebake is bounded by the mark being made rather than by the
  canvas.

The third is the answer, and **this is the reason the effect cache has to be
region-aware; memory is only the second reason.** It is also why §6 is a stage of
its own rather than an optimisation.

### 5.2 The float is the same problem wearing a different hat

`Editor::layer_draws` swaps the active layer's slot for the preview slice during
a transform. An effect baked from the layer's own slot would sit where the
picture *was* — a shadow left behind by a dragged object, every frame of the
drag.

Same three answers and the same one wins: bake from whichever slice the draw
actually uses, over `Transform::damage`'s rectangle, which is source ∪
destination and is already computed.

Note the ordering trap: the effect must be rebaked from the *preview* slice
during the drag and from the *layer* slice after the commit, and the two are
different slots. Keying the cache on the slot the draw carries — rather than on
the layer — makes that fall out rather than needing a rule.

---

## 6. The cache, and the three numbers

### 6.1 Slices

A canvas-sized `Rgba8UnormSrgb` slice is 16 MB at 2048² and **400 MB at
10000²** — the same as a layer, which is the arithmetic CLAUDE.md's parked-slice
bullet already warns about with "51.6 GB at 10000², with the budget reporting
kilobytes".

So the cache is **budgeted in bytes and not in count**, the way the undo history
is, and for the same reason: a count is right on one canvas size and absurd on
another. A default in the same order as undo's 512 MB, set once, shared by every
document.

An effect that will not fit is **not drawn**, and this needs care. A control that
lights up and does nothing is what this project refuses everywhere. The honest
shape is that the effect stays enabled and the panel says the document is over
its effects budget, naming the figure and where to change it — exactly the
treatment the undo panel gives "Earlier edits discarded", which exists because
the silent version was read as a bug.

### 6.2 Draws

`LayerStack::MAX` is 64 and `MAX_LAYERS` in `composite.wgsl` sizes the uniform
array at 64. `group-compositing.md` §7 shows the equality
`draws = layers + isolated folders ≤ 64` holds today **exactly, with no slack**.

Effects break it outright: 64 layers each with three effects is 256 draws.

The uniform can afford it. `ViewUniforms` is 2160 bytes at 64 entries — two
`array<vec4<f32>, 64>` at 1024 each, plus 112 of scalars and vectors — against
`downlevel_defaults`' `max_uniform_buffer_binding_size` of 16 KiB. At **192
entries** the two arrays are 3072 each, so 6256 bytes, leaving room for the third
array a later feature might want. Raising the array does **not** lengthen the
loop, which is bounded by `layer_count`; the cost is uniform bytes and the upload,
both of which are noise.

So: `MAX_DRAWS = 192`, `LayerStack::MAX` stays 64, and the difference — 128 — is
the document's effect-draw budget. A 129th enabled effect is **refused with a
tooltip saying so**, which is the treatment `LayerStack::LINK_GROUPS` already
gives a seventh link group, and is far better than a cap that truncates the draw
list silently. Truncation must stay unreachable for the reason
`group-compositing.md` §2.3 gives: a list cut off mid-group leaves an accumulator
open.

Three numbers now have to agree instead of two — `LayerStack::MAX`, `MAX_DRAWS`
in `canvas.rs`, `MAX_DRAWS` in `composite.wgsl` — and a CPU test should say so,
because it is exactly the kind of equality a later change to any one of them
breaks in silence.

### 6.3 Slots

`LayerStack::MAX_SLOTS` is `MAX × 2 + 1` = 129: one per layer, one per mask, one
spare for the float. Effects add up to `MAX_DRAWS − MAX` = 128 more, so
`MAX_SLOTS` becomes 257. Nothing is allocated by raising it — `INITIAL_SLOTS` is
four and growth doubles — so a document with no effects pays nothing, which is
the argument the mask's own headroom already makes.

`slot_revisions` is `vec![0; MAX_SLOTS]` of `u64`, so it goes from 1,032 bytes to
2,056. Its doc comment calls it "half a kilobyte", which is **already wrong by a
factor of two** at 129 slots — correct it in passing rather than doubling a
figure that was not right to begin with.

---

## 7. Undo

**Changing an effect is not an undoable edit.**

`EditKind` "has a variant only for something the engine can restore", and a
*layer's opacity* is not undoable today. An effect's parameters are the same kind
of thing: a value on a layer, with no pixels behind it. Giving effects an undo row
while layer opacity has none would be two rows that undo differently for no
reason a painter could see, and `group-compositing.md` §8 already declined the
same thing for a folder's opacity on the same grounds.

`docs/layer-rename.md` is the standing design for the `EditBody` arm that would
make a layer's *values* undoable. If it is ever built, effects join it. Until
then: no `EditKind` variant, no `panels::edit_icon` arm, no `history::VERSION`
bump.

**Toggling an effect on or off frees or claims a slot** (§4.2), and that is the
one thing here that touches the undo machinery — it must not park, and it must
not clear the history. Nothing else about the history moves.

An effect *applied to pixels* (§10.4) is an ordinary pixel edit with an ordinary
patch, filed under `EditKind::Paint`. Not a new variant: two rows that undo
identically must not have two names, which is the rule that keeps a paste under
Transform and a cut under Erase.

---

## 8. The file

### 8.1 Where the parameters go

A mask goes outside the ORA layer stack, under `umber/masks/`, pointed at by
`umber-mask`. **Effects take the same shape**: `umber/effects/<n>.ron`, pointed
at by an `umber-effects` attribute on the `<layer>` element.

The attribute is what makes it unambiguous. A single document-wide table would
have to be keyed by something, and every candidate is wrong: a stack position
shifts, a name is not unique, and an `id` is explicitly a within-session identity
that is never written down. An attribute on the element travels with the element
and has no key at all.

Serialising a blob into the attribute itself was the alternative. It works and it
puts an escaped RON string in the middle of `stack.xml`, which every other
reader has to skip past and every human has to read around. The mask's precedent
is better and costs one zip entry per effected layer.

Nothing here is a new extension mechanism: the `umber-` prefix is the mechanism,
and every other ORA reader ignores both the attribute and the directory. What
they see is the layer without its effects, which is the point of §8.2.

### 8.2 The version bump, and the argument against it

The rule: `umber-version` is bumped only where an older build would open the file
showing something **wrong**, not merely plainer. Masks and clipping took it to 2
because ignoring either shows a picture that is wrong — what the mask hid comes
back, the clipped layer paints everywhere. Folders did *not* move it, because a
flattened pass-through folder is the identical picture.

Which side is a dropped drop shadow on?

**The case for "plainer".** Every pixel that appears is correct. The layer is
where it should be, at the right opacity, under the right blend mode; a
decoration is absent. That is the folder case, and by that reading effects should
not move the version at all.

**The case for "wrong", which is the one that wins.** The failure is not what the
older build *shows*, it is what it *writes*. Effects are non-destructive: the
whole feature is that the parameters live in the file and nothing else does. An
older build opens the document, ignores an attribute it has never heard of, and
the next save drops `umber/effects/` on the floor. The artist gets their picture
back, without the shadow, permanently, having done nothing but open and save.
Masks and clipping have exactly that property too, and they are why the version
mechanism exists.

So: **effects raise `umber-version`.** And because a stroke can carry a picture —
outlined text against a busy background is illegible without it — the "plainer"
reading is weaker here than the folder case it borrows from.

**Which number, and the collision with group compositing.**
`docs/group-compositing.md` §4.3 also proposes 3. Only one of them can have it.
Whichever lands first takes 3 and the other takes 4; this must be decided once,
in whichever lands second, and not left for `required_version` to reconcile.
`required_version` emitting the lowest revision that describes the file does the
rest: a document with effects and no isolated group declares only what effects
need, and every document without either still declares 1 or 2 and still opens in
every Umber ever shipped.

`required_version`'s existing folder skip stays exactly as it is. An effect on a
folder is refused (§9.5), so a folder can never carry one to read.

### 8.3 What other applications see

The layer, unadorned, and no warning — because there is nothing they could do
with the parameters. `SaveWarning` should name it, in the same breath it names
an isolated folder: an effected layer is a real interoperability loss for any
other reader, and the artist should be told once at the save rather than
discovering it in GIMP.

Photoshop's `.psd` carries layer effects and Umber's importer does **not** read
them — `docs/document-import.md` already lists layer effects among what is
dropped. Reading them is a coherent later piece and is not part of this. Whether
`psd` 0.3.5 can reach them **has not been checked**, and the prior is poor: it
skips the layer-mask block, hands over no route to a mask's bytes, and
`photoshop.rs` documents three separate settings it cannot see for that reason.
If effects sit in the same skipped blocks it is the same second-parser fork that
`.psd` masks are refused for. Check before promising it.

---

## 9. What it touches

### 9.1 The mask

Effects derive from the layer's coverage **after** its mask. That is what a
painter means by hiding part of a layer, and it means the effect cache is
invalidated by the mask's slice revision as well as the layer's.

### 9.2 The wet stroke and the float

§5.1 and §5.2. These are the two cases where correctness and cost pull against
each other, and they are what stage 3 is for.

### 9.3 Clipping

A layer clipped to the one below, with effects of its own, gets its effects
clipped with it — they are draws sitting between the same neighbours and carrying
the same flag, so this falls out.

**Photoshop's rule for the other direction is deliberately not copied.** There, a
base layer's effects apply across its whole clipped group. Reproducing that means
an effect baked from a composite of several layers rather than from one slice,
which is the group-compositing problem again and is out of scope. Umber's rule:
an effect is a function of one layer's coverage. Say it in the module docs, so
somebody comparing against Photoshop finds the answer rather than a bug.

### 9.4 The canvas: resize and flip

Effect pixels are derived, so both are the same answer: **drop the slices and
let the next frame rebake.** A resize already clears the undo history and drops
the selection; a flip already commits the float and cancels the autosave capture.
Neither needs the effect slices mirrored — `flip.wgsl`'s exact-texel-permutation
guarantee exists because undoing a flip is another flip and loss would compound,
and a derived slice has nothing to compound. Rebuilding is exact by definition.

`resumed` — Android — rebuilds storage for every open document and pixels do not
survive it. Effects are rebuilt from parameters, which do. Nothing to add.

### 9.5 Folders — the one hard dependency

**An effect on a folder is refused until group compositing lands.** A folder
holds no slot and its contents are composited in place, so there is no coverage
to derive from: the honest input is the group's composited result, and that does
not exist until `docs/group-compositing.md` is built and an isolated folder has
an accumulator of its own.

Refused, not hidden: the effects control is drawn for a folder and disabled, with
a tooltip saying a group has no pixels of its own yet. That is the treatment the
blend and opacity controls get on a folder today.

When group compositing lands, a folder's effects become natural — the group's
accumulator is exactly the coverage to derive from — and they will need the
effect draws to sit *outside* the group's close marker. Worth noting now so the
marker's ordering is not fixed in a way that forecloses it.

### 9.6 Thumbnails

A row's thumbnail stays the layer's own content, without effects. `thumbnail.wgsl`
reduces one slice, an effect is other slices, and compositing several for a
64-pixel square is a third mode of that shader for very little. Consistent with a
folder's row drawing a folder mark rather than the composite of its contents.

---

## 10. The interface

### 10.1 Where it lives

A **new dockable module, `PanelKind::Effects`**, added to `PanelKind::ALL` and
deliberately **not** to `DEFAULT_DOCK`. That is the documented rule and the reason
matters: a layout file written before the module existed does not name it, an
absent panel is a closed one, and a default that included it would make a fresh
install and an upgraded one disagree about what the workspace holds. Adding to
`ALL` needs no version bump; adding to `DEFAULT_DOCK` would.

A modal was the alternative — Photoshop's Layer Style dialog. It loses: a painter
adjusts a shadow against the picture, and a modal covers the picture. Umber's
brush editor made the same call.

### 10.2 What it draws

The panel shows the **selected layer's** effects: a row per effect kind with its
own eye, and the parameters of whichever is expanded. Only the effects that work
are drawn — "do not add UI for features that do not work", and a whole row of
controls that are simply not there beats a row of disabled ones.

So the first version draws **Stroke** and **Drop shadow** and nothing else. Glow,
inner shadow and overlay arrive with their bakes.

Everything is `widgets.rs`': `number_row` for the figures that get typed (width,
distance, radius), `inline_slider` for opacity, `dropdown` for the blend mode and
the stroke's position, and the colour comes off the Colour panel's own picker
rather than a second one. The angle is `number_row` in degrees, which is the
control the interface scale and the wheel angle already share.

### 10.3 The layer row

A layer with effects needs a mark on its row, or the panel is the only place the
information exists and a painter wonders why a layer looks like that. It goes in
the per-layer flags row beside the mask, clip and lock marks — those are the
"this layer" controls, which is what this is. Clicking it opens the Effects
module, which is also how a removed module comes back.

**Not** in the ticked strip: that strip is for statements about several layers at
once, which is why the chain lives there and nothing else does.

### 10.4 Apply to pixels

A command that bakes the effects into the layer and clears them. One patch, one
`EditKind::Paint` entry, undoable like a stroke. It is what makes effects safe to
depend on before the importer, the exporters and other applications understand
them — and it is the escape hatch for anything the effect set cannot express.

Cheap to build once the bake exists, and worth having in the first release for
exactly the reason §8.3 gives.

---

## 11. Tests

Without a GPU, in `umber-core` and `umber-app`:

- **A document with no effects produces today's draw list**, entry for entry.
  The regression that matters most and it needs no device.
- **The three numbers agree** — `LayerStack::MAX`, `MAX_DRAWS` in `canvas.rs`,
  `MAX_DRAWS` in `composite.wgsl` — and `draws ≤ MAX_DRAWS` on a full stack with
  every effect enabled.
- **A 129th effect is refused** and the refusal changes nothing at all, the shape
  `can_reorder`/`plan_reorder` already keeps.
- **Effect order is stable**: outer effects below the layer, inner above, in the
  order §4 lists, for every subset of them enabled.
- **`required_version`** — a document with no effects still declares 1 or 2 and
  still carries no `umber-effects`; one with a shadow declares its number.
- **A save and a reopen do not move an effect's parameters**, the effect-shaped
  `saving_and_reopening_does_not_move_a_pixel`.
- **A layer whose effects are dropped by an older reader** — that is, the file
  parsed with the attribute ignored — still yields the same pixels for the layer
  itself.
- **The cache invalidates** on the layer's revision, on the mask's, and on every
  parameter, and on nothing else. Property-driven, because a missed field is
  silent and looks like a driver bug.

On the GPU, through `gpu_pipeline.rs`:

- **A stroke of width 0 and a shadow of radius 0 are the exact identity** — same
  pixels as no effect at all. The rule the feather, the grain and the selection
  clip all keep, and the one that says the fast path is really a fast path.
- **A drop shadow at Multiply multiplies against the backdrop**, not against its
  own layer. This is the point of an effect being its own draw entry and it is
  invisible in any test built only out of Normal.
- **The knockout**: a layer at 50% opacity over a shadow shows no shadow inside
  its own shape.
- **An inner effect is confined to the layer's alpha**, and a layer *clipped* to
  the one below with an inner effect is confined to both.
- **The blur is symmetric**: a shadow of a centred rectangle is mirrored about
  both axes, which is the property the feather's exact-integer running sums buy
  and the one that catches an off-by-one in a separable pass.
- **The distance field is a disc, not a square** — a stroke on a diagonal edge,
  compared against the separated `max` §3.1 refuses. Alphas compared with one
  level of slack, because the edge is antialiased; never bytes.
- **A rebake over a damaged region equals a rebake over the whole canvas** (stage
  3). The tiling's whole correctness claim in one test.

---

## 12. A staged plan

**Stage 0 — the model, with no pixels.** `Effect`, the parameter sets, the
ordering rule, the refusals, serialisation, `required_version`, and the reader
and writer. `LayerDraw`s are emitted for enabled effects and point at slots
holding nothing, so the shipped behaviour is a layer with a blank effect draw
over it — which means stage 0 ships *disabled*, behind the effect set being
empty. Every CPU test in §11 lands here. This is where the risk is bought down:
everything structural, nothing to see.

**Stage 1 — the bake, canvas-sized, at commit.** The distance field, the blur,
the knockout, the two effects. Effects refresh when the layer's slice revision
moves — so at pointer-up, not during the stroke. This is a usable feature at
ordinary canvas sizes and an honest one: the limitation is nameable and the
picture is never wrong, only late. The GPU tests land here.

**Stage 2 — the interface.** The module, the layer-row mark, Apply to pixels.
Deliberately after stage 1 rather than beside it, because "do not add UI for
features that do not work" cuts both ways and a control drawn against a bake that
is still moving is a control that gets redrawn.

**Stage 3 — regions, and liveness.** The `TileMask`-dilated rebake, the live
shadow during a stroke and during a float, and the byte budget that stops a large
canvas from being the case that breaks it. This is the stage with a measurement
in it and the only one whose cost cannot be settled by reading. Measure the
canvas-sized bake first — if 2048² is comfortably inside a frame, stage 1 already
covers most documents and stage 3 becomes about large canvases rather than about
liveness.

**Is there a useful first piece?** Stages 0 and 1 together, which is a working
drop shadow and a working stroke that refresh at pointer-up. Stage 0 alone ships
nothing. **Do not start with the interface**, and do not start with more than two
effect kinds: the second kind is what proves §3's unification and the fourth
proves nothing.

---

## 13. Not settled

- **Whether the canvas-sized bake fits a frame at ordinary sizes**, which decides
  whether stage 3 is about liveness or only about large canvases. Nothing here
  can answer it by reading. `gpu_pipeline.rs` composites into an offscreen target
  and is where the number comes from; not on CI, per the rule about wall-clock
  assertions on a runner nobody chose.
- **Whether jump flooding is worth it over blur-and-threshold** at the widths
  people actually use. §3.1 argues for it on corners; a picture of a 20 px stroke
  by each method settles it faster than the argument does.
- **Which version number**, 3 or 4, against `docs/group-compositing.md`. §8.2.
  Decided by which lands first and must be written down when it does.
- **Whether effects should raise the version at all.** §8.2 argues yes on the
  round-trip loss and states the counter-argument fairly. It is the one judgement
  in this document that a reasonable person could take the other way, and it is
  worth a second opinion before stage 0 writes it into `required_version`.
- **Whether `Effect` belongs on `Layer` or beside the stack.** On `Layer` by the
  argument `picked` and `link` both make — a set beside the stack has to be kept
  in step with reordering and deletion by hand. But `Layer` is `Clone` and cheap
  today, and a parameter set per effect kind is not `Copy`; whether that matters
  to `StackShape` and the structural undo entries wants checking against
  `docs/structural-undo.md` before stage 0.
- **Reading `.psd` layer effects.** §8.3. A coherent later piece, blocked on the
  same crate limit `.psd` masks are.
