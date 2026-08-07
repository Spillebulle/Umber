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

**Measured, it is the expensive effect — not the shadow — and that was the wrong
way round in the first draft of this document.** §3.4 has the figures. The short
version is that a jump flood at 10000² costs 19 ms at radius 64 and holds 1.6 GB
of ping-pong buffers, against 3 ms and 1 GB for a downsampled shadow of the same
radius. So the choice between jump flooding and blur-and-threshold is no longer
a question of corner quality alone; it is 6x the time and twice the memory, and
§13 now weighs it that way.

### 3.2 The blur

**A tent from two box passes per axis**, which is exactly what
`umber-core::selection`'s feather already is, and separable for the same reason.

**But not "linear in the area whatever the radius", and this document said so
before it was measured.** That property belongs to the feather's *running
sums*, and a fragment shader has none: a box pass there is `2r + 1` taps per
texel, so the cost scales with the radius after all. Measured at 10000², a
full-resolution tent goes from 8.5 ms at radius 4 to 83 ms at radius 64 — ten
times, for a claim that predicted no change at all. Borrowing an algorithm's
complexity across a change of execution model is the mistake; the kernel
carried over and the bound did not.

**So the blur is done on a 4x downsample and bilinearly upsampled**, which is
what makes it affordable and nearly radius-independent: 2.0 ms to 3.2 ms across
the same sweep, and 0.36 ms to 0.45 ms at 2048². Sixteen times fewer texels at
a quarter of the radius is ~64x less work, and the quality cost falls only on a
hard edge — which a shadow, a glow and a soft stroke do not have. An effect that
genuinely needs a hard edge is the one that must not take this path, and at a
hard edge the radius is small and the full-resolution pass is cheap. §3.4.

Two more things follow that are easy to get wrong:

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

### 3.4 Measured

`examples/measure-effects.rs`, in `umber-render` because it needs a device.
**Re-run it before quoting any of these**, which is the rule
`measure-clipboard.rs` records having learned the hard way when figures three
times too slow got written into the docs by a machine that was building six
other things at the time.

Median wall-clock around `submit` plus a blocking poll, seven runs after three
warm-ups. Not a GPU timestamp — `Features::TIMESTAMP_QUERY` is not among the
features Umber requests, and asking for it would measure a device Umber never
creates. It over-states by whatever the submit and the fence cost, which is the
safe direction.

**RTX 3080, Vulkan.** Milliseconds; `!` is over a 60 Hz frame.

| canvas | radius | shadow, full res | shadow, quarter res | stroke, jump flood |
|---|---|---|---|---|
| 2048² | 4 px | 0.64 | **0.36** | 0.66 |
| 2048² | 16 px | 1.25 | **0.38** | 0.89 |
| 2048² | 64 px | 3.77 | **0.45** | 1.13 |
| 10000² | 4 px | 8.50 | **2.05** | 9.04 |
| 10000² | 16 px | 22.46 ! | **2.26** | 14.01 |
| 10000² | 64 px | 83.05 ! | **3.23** | 19.40 ! |

Textures held at once: 44 MB at 2048² and **1,049 MB at 10000²** for the
shadow; 68 MB and **1,621 MB** for the stroke, whose two `Rg16Uint` seed buffers
are 400 MB each on the large canvas.

**The software rasteriser brackets it from below.** The same sweep on
`Choice::Fallback` — WARP, a CPU — is roughly 100x slower: a quarter-resolution
shadow at 1024² is 8.0 ms and at 2048² is 26.4 ms. That is not a mobile GPU and
must not be read as one; it is the floor, and a real integrated part lands
somewhere between. It is recorded because a number from a discrete card alone is
exactly the kind of evidence `downlevel_defaults` exists to distrust.

Four readings, and the third and fourth are the ones that changed this design:

- **At ordinary canvas sizes everything is cheap.** Every bake at 2048² is
  under 1.2 ms on the 3080 and the downsampled shadow is under half a
  millisecond at every radius. A live rebake during a stroke is affordable
  there, which §5.1 assumed it might not be.
- **At 10000² the downsampled shadow still fits and the full-resolution one does
  not**, by a factor of twenty-six at radius 64. Downsampling is not an
  optimisation to add later; it is the only version of this that works.
- **The stroke is the expensive effect, not the shadow.** The design had it
  backwards. Jump flooding is the one bake that fails a frame at 10000², and it
  is the one whose memory — 1.6 GB — is a harder wall than its time.
- **Memory bites before time does.** A gigabyte of transient texture per
  effected layer is not something to hold at canvas scale whatever the frame
  budget says, and it is what makes §6.1's byte budget and stage 3's region
  bounding load-bearing rather than tidy.

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
- **Bake every frame from layer + scratch.** Right on screen, at the cost of the
  whole pipeline at canvas resolution every frame.
- **Bake every frame, over the damaged region only.** The stroke's damage is
  already accumulated as a `damage::TileMask` on a 64-pixel grid, for the undo
  patch. Dilate that by the effect's reach — spread plus softness plus the
  offset — and the rebake is bounded by the mark being made rather than by the
  canvas.

**§3.4 measured this and the answer splits by canvas size, which the first draft
did not anticipate.** At 2048² a downsampled shadow bakes in 0.4 ms and a stroke
in about 1 ms, so the *second* answer is affordable and the live shadow can ship
without any region machinery at all. At 10000² the shadow still fits at 3.2 ms
and the stroke does not, at 19 ms — and both hold about a gigabyte while they
do it.

So the third answer is still where this ends up, and the reason has moved: it is
**memory at canvas scale and the stroke's distance field**, not the shadow and
not the frame budget. That is a narrower claim than "the cache has to be
region-aware", and it is what lets stage 1 ship a live shadow rather than a
lagging one.

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

### 6.1a Two budgets, and only one of them may refuse

There is a cap on *adding* an effect and a cap on *drawing* one, they are
different numbers, and the distinction is what stops an undo being refused.

- **Adding is gated.** `LayerStack::set_effect` consults the slice budget and
  declines, with a `can_set_effect` beside it sharing the plan, so a control
  cannot light up promising what the model will refuse. A refusal changes
  nothing at all.
- **Overflow is not gated, because an undo may not be refused.**
  `LayerStack::restore_shape` puts a deleted layer back with the effects it had,
  and there is no answer to "your undo does not fit" that is better than doing
  it. The same goes for an import, for a document opened from a file, and for a
  layer moved out of a folder. So a document *can* hold more enabled effects
  than it has slices or bytes for.

**Therefore the draw path must degrade visibly rather than truncate silently**,
and that is one rule serving both budgets. Effects are dropped in a stated order
— the ones furthest down the stack first, so the layer being worked on keeps
its own — the document is said to be over budget, and nothing is quietly
different from what the panel shows. Truncating the draw list at `MAX_DRAWS`
instead would be the silent version, and it is refused for the reason
`group-compositing.md` §2.3 gives about a list cut off mid-group.

The tempting simplification is to make `restore_shape` refuse, which is one `if`
and is wrong: an undo that declines to undo is worse than a picture that is
missing a shadow and says so.

### 6.2 Draws

`LayerStack::MAX` is 64 and `MAX_LAYERS` in `composite.wgsl` sizes the uniform
array at 64. `group-compositing.md` §7 shows the equality
`draws = layers + isolated folders ≤ 64` holds today **exactly, with no slack**.

Effects break it outright: 64 layers each with three effects is 256 draws.

The uniform can afford it, and this is the one capacity question that is not
tight. `ViewUniforms` is 2,160 bytes at 64 entries — two `array<vec4<f32>, 64>`
at 1,024 each, plus 112 of scalars and vectors — against `downlevel_defaults`'
`max_uniform_buffer_binding_size` of 16 KiB. At **191 entries** the two arrays
are 3,056 each, so **6,224** bytes, leaving room for the third array a later
feature might want. Raising the array does **not** lengthen the loop, which is
bounded by `layer_count`; the cost is uniform bytes and the upload, both of
which are noise.

**The uniform's headroom is what made this look easy, and it is the wrong limit
to have been looking at.** There is 10 KiB spare here and the real ceiling was
somewhere else entirely — §6.3.

So `LayerStack::MAX` stays 64 and a separate `MAX_DRAWS` sizes the arrays. **What
that number is, is decided in §6.3 and not here** — an effect draw reads an
effect slice, so the draw budget cannot exceed the slice budget, and the slice
budget turns out to be the binding constraint. It is **191**.

An effect past the budget is **refused with a tooltip saying so**, which is the
treatment `LayerStack::LINK_GROUPS` already gives a seventh link group, and is
far better than a cap that truncates the draw list silently. Truncation must
stay unreachable for the reason `group-compositing.md` §2.3 gives: a list cut off
mid-group leaves an accumulator open.

Three numbers now have to agree instead of two — `LayerStack::MAX`, `MAX_DRAWS`
in `canvas.rs`, `MAX_DRAWS` in `composite.wgsl` — and a CPU test must say so,
because it is exactly the kind of equality a later change to any one of them
breaks in silence. **Pin the array *declarations*, not only the constant**: leave
the constant right and write `array<vec4<f32>, 64>` and the WGSL struct is
merely smaller than the bound buffer, which validates — and the composite then
reads `extra` as `layers` past index 63, silently.

### 6.3 Slots — and the ceiling this document got wrong

**`MAX_SLOTS` may not exceed 256, and the first draft of this section proposed
257.** That is a fatal error and it is worth keeping the wreckage visible.

The limits Umber requests are `Limits::downlevel_defaults().using_resolution(…)`.
`downlevel_defaults()` names seven fields and `max_texture_array_layers` is not
among them, so it inherits `Limits::defaults()`' **256**; and `using_resolution`
raises the three texture *dimension* limits and nothing else. So every device
Umber creates guarantees exactly 256 array layers. Asking for 257 is a
`create_texture` validation error, and `crash::device_error` makes that fatal —
a painting application killed by adding a layer.

The trap is that the number reads as unbounded. `using_resolution` is right there
raising limits from the adapter, and it looks as though it raises this one.
It does not.

So the ceiling is stated as what it is and everything else is derived from it:

```
MAX_SLOTS         = 256                           // the guaranteed ceiling
MAX_EFFECT_SLICES = MAX_SLOTS − (MAX × 2 + 1)     // = 127
MAX_DRAWS         = MAX + MAX_EFFECT_SLICES       // = 191
```

One slice per layer, one per mask, one spare for the float — 129 — and the
remaining **127** are the document's effects.

**127 rather than 128 is what makes the cap reachable at all**, which is worth
recording because the first draft's 128 made it dead arithmetic. Two kinds, one
of each per layer, `LayerStack::MAX` of 64: the most a legal document can enable
is 128. Against a budget of 128 that is exactly the ceiling and never over it, so
the refusal could only be exercised by building a stack the model otherwise
forbids — a test that pins a synthetic shape and proves nothing about a document.
Against 127 the last effect on a fully doubled stack is refused for real. The
budget should be re-checked for reachability whenever a kind is added, and the
guard should say so rather than the next person rediscovering it.

Sitting exactly on the device's
figure is safe **only because it is derived from that figure and asserted against
it**, so the assertion is the load-bearing part and not a formality:

```rust
const _: () = assert!(
    LayerStack::MAX_SLOTS <= wgpu::Limits::downlevel_defaults().max_texture_array_layers
);
```

Its comment has to say *why* — that this limit is inherited rather than named,
and that `using_resolution` does not touch it — because the comment is the only
thing that stops the next person making the same reading.

Deriving rather than typing also means a later change to `MAX` re-derives the
budget instead of silently overrunning: raise `MAX` to 100 and the effect budget
goes to 55 on its own, and the assertion fails if it would go negative.

Nothing is allocated by raising `MAX_SLOTS` — `INITIAL_SLOTS` is four and growth
doubles — so a document with no effects pays nothing, which is the argument the
mask's own headroom already makes. **Except one thing, which is eager:**
`slot_revisions` is `vec![0; MAX_SLOTS]` and is paid per open document.

At `u64` each it goes from 1,032 bytes to 2,048. Its doc comment calls it "half a
kilobyte", which is **already wrong by a factor of two** at 129 slots — correct
it to be right rather than doubling a figure that was not right to begin with.

**A budget in slices is not a budget in bytes**, and §6.1's is the one that bites
first: 127 effect slices at 10000² would be 50 GB. The slice count is a hard
ceiling the device imposes; the byte budget is what a document actually spends
against. Both are needed and neither implies the other.

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

**This was put to the author as an open judgement and confirmed**, which is why
it is stated here as a decision rather than a lean. It was the one call in this
document that could reasonably have gone the other way, and it is recorded as
having been made rather than assumed — because the consequence, an older Umber
*refusing* a document rather than opening it plainly, is a heavy hammer and the
next person to read this will want to know somebody chose it on purpose.

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
- **`MAX_SLOTS` does not exceed what the device guarantees.** A `const` assertion
  against `Limits::downlevel_defaults().max_texture_array_layers`, which is the
  guard §6.3 exists for and the one whose absence would have shipped a fatal
  validation error.
- **The budget is reachable by a legal document**, so the refusal is exercised
  through an ordinary stack rather than a synthetic one — §6.3, and it was not
  true at the first draft's figure.
- **An undo is never refused for want of budget.** `restore_shape` puts a
  deleted layer back with its effects even when that goes over, and the draw
  path drops effects in a stated order and says so. §6.1a — the one rule that
  keeps two budgets from becoming two behaviours.
- **An effect past the budget is refused** and the refusal changes nothing at all, the shape
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

**Stage 1 — the bake, canvas-sized, live.** The distance field, the
**downsampled** blur, the knockout, the two effects. §3.4 says a canvas-sized
rebake is 0.4 ms to 1 ms at 2048², so this rebakes every frame from layer plus
scratch and the shadow follows the brush — which the plan before the measurement
had deferred to stage 3. Above a canvas size the bake cannot hold, it falls back
to rebaking at commit and says nothing, because a shadow one stroke late is
still the right picture. The GPU tests land here.

**Stage 2 — the interface.** The module, the layer-row mark, Apply to pixels.
Deliberately after stage 1 rather than beside it, because "do not add UI for
features that do not work" cuts both ways and a control drawn against a bake that
is still moving is a control that gets redrawn.

**Stage 3 — regions, for memory and for the stroke.** The `TileMask`-dilated
rebake and the byte budget. The measurement moved what this stage is *for*: not
liveness, which stage 1 now has at ordinary sizes, but the gigabyte of transient
texture a canvas-scale bake holds and the jump flood that costs 19 ms at 10000².
Both are large-canvas problems, and both are real.

**Is there a useful first piece?** Stages 0 and 1 together, which is a live drop
shadow and a live stroke at ordinary canvas sizes. Stage 0 alone ships nothing.
**Do not start with the interface**, and do not start with more than two effect
kinds: the second kind is what proves §3's unification and the fourth proves
nothing.

---

## 13. Not settled

**Settled by §3.4 and struck from this list:** whether a canvas-sized bake fits a
frame. It does at 2048² and, downsampled, at 10000²; the naive full-resolution
blur does not, and the claim that it would was wrong. What is left:

- **Whether jump flooding is worth it over blur-and-threshold.** This was a
  question about corner quality and is now mostly a question about cost: §3.4
  puts the flood at 19 ms and 1.6 GB at 10000²/64 px against ~3 ms and 1 GB for
  a blur, and the blur is already being built for the shadow. **The likely answer
  is now blur-and-threshold**, with the flood kept for a hard-edged stroke at a
  small radius where it is cheap. A picture of a 20 px stroke by each method
  settles it faster than the argument does, and it should be made before stage 1
  writes either one.
- **What an integrated or mobile GPU does.** §3.4 has a discrete card and a
  software rasteriser and nothing between, and they are 100x apart. The
  downsampled shadow has enough headroom that it is probably safe either way;
  the flood plainly does not. Anyone with a laptop should re-run the example
  before stage 1 fixes the algorithm.
- **Which version number**, 3 or 4, against `docs/group-compositing.md`. §8.2.
  Decided by which lands first and must be written down when it does.
- **Whether `Effect` belongs on `Layer` or beside the stack.** On `Layer` by the
  argument `picked` and `link` both make — a set beside the stack has to be kept
  in step with reordering and deletion by hand. But `Layer` is `Clone` and cheap
  today, and a parameter set per effect kind is not `Copy`; whether that matters
  to `StackShape` and the structural undo entries wants checking against
  `docs/structural-undo.md` before stage 0.
- **Reading `.psd` layer effects.** §8.3. A coherent later piece, blocked on the
  same crate limit `.psd` masks are.
