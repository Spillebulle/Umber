# Group compositing

A folder with an opacity and a blend mode of its own.

`docs/layer-folders.md` is the other half of this and is assumed here: what a
folder is, why it sits above its own contents, why the stack stayed a flat `Vec`,
and why the cheap half — containment, movement, visibility, locking — shipped
without touching `composite.wgsl` at all. Its §7 sketches this feature. This
document is that sketch worked through against the code, and it **corrects it in
five places**; each correction is marked where it appears.

Nothing here is built.

---

## 1. What this changes, and what it must not

The one sentence the whole design has to protect is `layer.rs`'s:

> Every folder in this build is **pass-through**: a container, whose visibility
> and lock reach its contents and whose opacity and blend mode do not exist.

A pass-through folder is exactly its contents composited in place. That is why
the shader never learned folders exist, why `umber-version` did not move, and
why an older Umber — or GIMP, or MyPaint — flattening the nesting away shows the
identical picture.

**Isolated** is the other thing, and it is what an opacity means. The group's
children composite into an accumulator of their own, starting from a transparent
backdrop, and the finished result is composited into the backdrop below with the
folder's own opacity and blend mode. A folder at 50% over two overlapping
children is not two children at 50% each; a folder set to Multiply must multiply
the group's *result* into the backdrop, not each child separately.

Both have to exist. Turning every folder isolated would silently change every
document this build has already written — not through the opacity, which is 1
and where source-over is associative, but through **a child with a blend mode**,
which would multiply against the group's transparent backdrop instead of against
what is under the group. `layer-folders.md` §7 has that argument and it stands.

So the rule this design is built on:

> A folder is **isolated** exactly when its opacity is below 1 or its blend mode
> is not Normal. Otherwise it is pass-through and reaches the renderer as
> nothing at all, precisely as it does today.

Derived, not stored. The same predicate decides isolation, whether the file
needs a new revision, and whether the folder occupies an entry in the shader's
uniform array — so the three cannot disagree, there is no new field, and there
is no migration. §3.2 is what that costs.

---

## 2. The shader

### 2.1 An accumulator stack is unavoidable, and here is why

`composite.wgsl` walks `layers: array<vec4<f32>, MAX_LAYERS>` bottom to top into
one `acc`, carrying a running `clip_alpha`. Compositing a group means: the
parent's partial result must survive untouched while the child group accumulates
from zero, and the two are combined when the child ends. The parent's value is
therefore *live across the whole of the child's run*, and so is its parent's.
The number of simultaneously live accumulators is the nesting depth. That is not
an implementation detail to be encoded away — it is what nesting is.

So there are exactly two shapes, and the second is refused in §2.5:

- a per-fragment stack of accumulators inside the one pass, or
- an intermediate texture and a pass per group.

`LayerStack::MAX_DEPTH` is 7 and exists for this: "the eventual group stack in
the fragment shader is a fixed-size array, and a document too deep for it has to
be refused where somebody can be told". That part is already done.

**How deep the array actually is: seven, not eight, and not nine.** The array
holds the *suspended* levels; the level being worked on lives in `acc` itself.
`well_formed` bounds depth at 7, so a folder at depth 7 can hold nothing — its
contents would be at depth 8 — and §2.2 emits nothing at all for an empty group.
The deepest useful folder is therefore at depth 6, with six enclosing it: seven
groups open at once, seven suspended accumulators, one live. `array<vec4<f32>,
7>` is exact and has no slack.

**There is no `clip_stack`, and `layer-folders.md` §7 is wrong to propose one.**
It reasons that `clip_alpha` must be saved and restored with the accumulator.
Trace what would read the restored value: the entry after a group close is
either clipped, in which case it wants the *group's* alpha, or unclipped, in
which case it writes `clip_alpha` itself. A close therefore sets `clip_alpha`
unconditionally — from the group's own composited alpha, by the same line a
layer uses — and the saved value is never read. Zeroing on push is the whole of
it. Seven registers and a second array saved, and one thing that would bring
them straight back: **a clipped folder**, which does not write `clip_alpha` and
so would need the enclosing value to survive. §6 defers that deliberately.

### 2.2 The markers, and why the CPU emits them

Two things travel per draw entry:

- **`open`** — how many accumulator levels to push before this entry, zeroing
  each. Nought for almost everything.
- **this entry is a group close** — pop one level, composite the popped
  accumulator into the restored one through `composite_over` with this entry's
  opacity and blend mode.

**`open` is an explicit count, not a depth**, and this is the second correction
to §7, which proposes carrying each entry's `depth` and pushing for every level
it exceeds the running one. Three reasons the count wins:

- **The depth that matters is the *isolation* depth, not `Layer::depth`.** A
  pass-through folder must not open an accumulator and must not close one, so a
  document with plain folders has to produce byte for byte the draw list it
  produces today. Deriving that from `Layer::depth` means the shader knowing
  which folders are isolated, which is exactly the thing the CPU has already
  worked out.
- **An empty isolated folder disappears.** §7 has the shader guard the pop
  against a group that was never opened. It does not need to: an empty group
  composites nothing, so the CPU emits *no draw entry for it at all* — the same
  treatment a pass-through folder gets. That removes the unbalanced case from
  the shader rather than handling it, and it is what makes the seven above
  exact.
- **"Opens equal closes" becomes a CPU invariant with a unit test.** A shader
  that tolerates a malformed uniform is a shader that hides the bug that
  produced it.

**Where they go: in the two spare fields, with no new binding.** `layers[i].z`
is the slot, and a folder holds none — writing `-1` there says "this is a group
close", and every real slot is non-negative. `extra[i].w` is documented as
unused and takes `open`. One trap: `extra[i].x` is the mask slot and is written
`src.mask.unwrap_or(src.slot)` so the array index is always in range; a close
entry has no slot, so it must be written 0 with `has_mask` clear rather than
inheriting the sentinel.

A third array was the alternative and is not needed. It would be honest and it
would cost 1 KiB: `ViewUniforms` is **2160 bytes** today (32 for the four
`vec2`s, 48 for three `vec4`s, 16 for the background, 32 for the eight scalars,
1024 each for `layers` and `extra`) against `downlevel_defaults`'
`max_uniform_buffer_binding_size` of 16 KiB. There is room for four more such
arrays. Two spare fields exist, so use them — and whatever is chosen, the Rust
`#[repr(C)]` struct and the WGSL one still have to agree byte for byte; see
CLAUDE.md's "Uniform layout".

`LayerDraw::slot` becomes `Option<u32>`, which is the shape `Layer::slot` and
`LayerMeta::slot` already have and for the same reason. It is a field, so the
four things that reuse the composite pass are untouched — see §5.

### 2.3 The loop

```wgsl
const MAX_GROUP_DEPTH: u32 = 7u;   // LayerStack::MAX_DEPTH

var suspended: array<vec4<f32>, MAX_GROUP_DEPTH>;
var acc = vec4<f32>(0.0);
var clip_alpha = 0.0;
var level = 0u;

for (var i = 0u; i < v.layer_count; i = i + 1u) {
    // Opens first, and unconditionally. `open` is a property of the entry's
    // position, not of whether it draws: the bottom entry of a group may be
    // hidden and the folder above it will still close.
    let open = u32(v.extra[i].w);
    for (var k = 0u; k < open; k = k + 1u) {
        suspended[level] = acc;
        acc = vec4<f32>(0.0);
        clip_alpha = 0.0;      // §6
        level = level + 1u;
    }

    let params = v.layers[i];
    if (params.z < 0.0) {      // a group close
        // The pop comes BEFORE the visibility test. A folder that is hidden or
        // at zero opacity still closes its group; skipping the pop leaves the
        // stack unbalanced and every level after it wrong.
        level = level - 1u;
        let group = acc;
        acc = suspended[level];
        clip_alpha = select(0.0, group.a, visible && opacity > 0.0);
        if (visible && opacity > 0.0) {
            acc = composite_over(acc, group * opacity, mode);
        }
        continue;
    }

    // ... the existing body, verbatim ...
}
```

Four things fall out of that being the shape:

- **There is no second copy of the blend maths.** A group goes through
  `composite_over` — the same function every layer already goes through, W3C
  compositing with both operands premultiplied. Scaling a premultiplied group by
  its opacity is correct as-is, exactly as the layer line beneath it is.
  Isolation *is* "start from a transparent backdrop", which is what the push
  writes, so the W3C definition and this code are the same statement.
- **`composite_over` already returns `dst` unchanged for a transparent source**,
  so a group whose contents are all hidden costs one early-out and no special
  case.
- **The pop before the visibility test** is the one ordering that can be got
  wrong silently: it is right for every document without a hidden folder, which
  is most of them.
- **The truncation at `MAX_LAYERS` must stay unreachable.** `composite` clamps
  `count` to 64; a draw list cut off mid-group would leave levels open and the
  background would be added to a group's accumulator. It cannot happen — see §7
  — and that is what `LayerStack::MAX` counting folders bought.

### 2.4 What it costs, and what would measure it

**I have not measured this and cannot from here.** What can be said without
running it:

The array is 7 × `vec4<f32>` = **28 scalar registers**, allocated statically for
every fragment of every frame whether or not a document has a folder in it. A
rough count of what the loop keeps live today — `acc`, `clip_alpha`, `uv`,
`screen`, `coverage`, `params`, `extra`, `lay`, `m` — is 25 to 35. So this is
roughly a doubling of the fragment shader's register pressure.

Occupancy is a step function of that, and GCN is the case I can state exactly: a
SIMD has 256 VGPRs per lane and at most 10 wavefronts, so occupancy is
`min(10, floor(256 / vgprs))`. A shader at 32 VGPRs gets 8 waves; at 64 it gets
4. Going from ~32 to ~62 halves it. RDNA and NVIDIA have their own step tables
of the same shape — NVIDIA allocates in blocks of 8 registers against a 64 K
register file per SM — and the step positions differ, so the honest claim is the
shape and not a number.

**Two things make the case better than it looks, and one makes it much worse.**

Better: the index into the array is `level`, which is derived only from the loop
counter and the uniform. **No per-fragment value enters it, so the nesting is
wave-uniform control flow** — the best case for this pattern. The values stored
are per-fragment and must be VGPRs, but a wave-uniform index into a VGPR array
is relative addressing or a small select chain, not a divergent scatter. And the
inner push loop is bounded by a compile-time constant, so it can be unrolled
where the outer one cannot (`v.layer_count` is uniform but not constant).

Worse: a dynamically indexed function-scope array is exactly the construct a
compiler puts in **scratch memory** when it cannot promote it — an "indexable
temp" in DXBC/DXIL terms. On a full-screen fragment shader at 4K that is not an
occupancy cost, it is a bandwidth cost, and it is the risk that decides this
design. Whether any given driver promotes it is not knowable by reading.

What would settle it, none of which needs an Umber change:

- **Radeon GPU Analyzer** on the SPIR-V `naga` already produces: VGPR and SGPR
  counts, scratch bytes, and occupancy against a named ASIC. Scratch bytes above
  zero is the answer on its own.
- **`spirv-cross` → HLSL → `dxc`**, and read the disassembly for
  `dcl_indexableTemp`. Same question, on the other driver family.
- **Mali Offline Compiler**, which is the one that matters: device limits are
  `downlevel_defaults` precisely so a desktop build cannot depend on what a
  mobile GPU refuses, and mobile is where a register spill is not survivable.
- **A frame time.** `gpu_pipeline.rs` composites into an offscreen target; a
  64-entry stack at document resolution, before and against after, is the number
  that decides it. Not on CI — see CLAUDE.md's rule about wall-clock assertions
  on a runner nobody chose.

**If it does cost, the fallback is a second pipeline, and the shader should be
written so that stays cheap.** The CPU knows whether any folder in the document
is isolated. Two entry points in the same module — one with the stack, one
without — chosen per frame, means a document with no isolated folder pays
exactly what it pays today, which is the standard the selection clip and the
mask branch are both held to ("no selection is the exact identity"). It is only
affordable if the per-layer body is first factored into a function both entry
points call, because two copies of the compositing loop is the drift this
codebase refuses everywhere. **Factor the body out in stage 2 whether or not the
second pipeline is ever built**, so that it is a small change rather than a
rewrite. Note that a uniform `if (level == 0)` fast path is *not* a substitute:
register allocation is static, so the fragments that take it still pay.

### 2.5 The alternative refused: a pass per group

An intermediate render target per nesting level, one pass per group, composited
in order. It needs no per-fragment array and no register pressure at all.

It loses on four counts, and the first two are fatal:

- **It breaks the property that four other things reuse this pass.** The
  composite is a screen-space pass, so the intermediates are target-sized —
  which is a 1×1 target for `pick_colour`, a small patch for `probe_canvas`, and
  **the whole canvas** for `export_rgba` and the autosave's capture. At 10000²
  an `Rgba16Float` intermediate is 800 MB, per level, transient, on a device
  that is already holding layer slices at 400 MB each.
- **`Rgba8` intermediates lose precision the single pass does not.** `acc` is an
  `f32` vec4 for the whole walk today; storing a group's partial result in eight
  bits per channel and reading it back quantises it, and stacked groups compound
  that. The choice is banding or the doubled memory above.
- **N+1 full-screen bandwidth round trips per frame, while painting** — exactly
  what "the entire stack composites in ONE pass. Do not simplify this into a
  pass per layer" exists to prevent, arriving by another door.
- It is more code, not less: a target pool, a level allocator, and a second
  place where the export and the screen can disagree.

Worth recording that the viewport-sized case is not absurd on its own — a 4K
`Rgba16Float` intermediate is 66 MB and one or two levels covers nearly every
real document. It is the export and capture paths, which render at document
resolution, that kill it.

---

## 3. Isolated versus pass-through

### 3.1 Deriving it

`is_isolated(folder) == folder.opacity < 1.0 || folder.blend != Normal`.

One predicate, in `umber-core`, read by `Editor::layer_draws`,
`Candidate::draws`, `Editor::active_draw_index`, `docformat::required_version`
and `folder_xml`. Because it is derived there is no flag to migrate, no way for
the file and the shader to disagree about which folders are groups, and — the
point — a document written by this build with plain folders is byte for byte the
document the current build writes.

### 3.2 The one thing it cannot say, and it is a real loss

**Umber cannot represent a pass-through group with an opacity.** Photoshop can:
a group set to Pass Through at 50% fades its contents *as composited in place*,
which is a genuinely different picture from an isolated group at 50% wherever a
child inside it has a blend mode. Krita can, through its Passthrough checkbox
sitting beside the opacity slider.

`layer-folders.md` §7 names the mirror-image case — "a folder cannot be isolated
*and* transparent-looking at opacity 1" — and calls it something nobody has
asked for. That is right, and this one is the case with a claim. It is the price
of no flag, and it is what a later `Layer::isolated: bool` would buy. If that
day comes, **the flag and the version clause have to move together**, because
the version rule in §4.3 is stated in terms of the same predicate.

It also decides what the reader does with such a file — §4.4.

---

## 4. The file

### 4.1 A bug that exists today

The OpenRaster layer-stack specification gives a non-root `<stack>` an
`isolation` attribute taking `isolate` or `auto`, **defaulting to `isolate`**.
`auto` is what every other application spells "pass-through".

Umber writes folders and does not write `isolation`. So every folder Umber has
ever written declares itself, by the specification's default, **isolated** —
when it is pass-through and the whole argument for folders not moving
`umber-version` is that it is.

Today that is mostly harmless, because `folder_xml` writes no `opacity` and no
`composite-op`, and an isolated group at opacity 1 in source-over is identical to
a pass-through one by associativity. It is *not* harmless in one case: **a child
with a blend mode.** An Umber document with a Multiply layer inside a folder,
opened in an application that honours the default, composites that layer against
the group's transparent backdrop instead of against what is under the group. The
picture is wrong, in the file, now — and it is precisely the failure the
pass-through/isolated split in §1 exists to prevent inside Umber.

**Writing `isolation="auto"` on every pass-through folder fixes it, needs no
version bump, and is worth doing on its own** — see stage 0 in §10. An older
Umber ignores an attribute it has never heard of and behaves as pass-through
already, which is what the file would then say.

Reading it is the other half, and there is a subtlety: Krita treats an *omitted*
isolation as isolate and an *unrecognised* value as `auto`. Umber should read
`auto` as pass-through and everything else — including absent — as isolated,
which is the specification's own reading and the safe one, since it is the
reading under which a group carrying an opacity is not silently reinterpreted.

### 4.2 A group opacity is baseline ORA, not an extension

Also from the specification, on a non-root `<stack>`: `name`, `opacity`
(default 1.0), `visibility` (default `visible`), `composite-op` (default
`svg:src-over`), `isolation`. All five are baseline.

So this feature adds **no new `umber-` attribute**. `folder_xml` starts writing
attributes the format already defines, which is what "there must never be a
second ORA reader" and "the `umber-` attributes are the extension mechanism"
would both want. `umber-blend` still rides along on a folder for the same reason
it rides along on a layer: Add's nearest SVG name (`svg:plus`) is only
approximate, and without it reopening Umber's own file reports a loss that did
not happen. `composite_op` and `blend_id` are already written and need no
change.

### 4.3 `required_version` gains one clause, and it is the right kind

> A folder whose opacity is below 1 or whose blend mode is not Normal takes
> `umber-version` to **3**.

That is the same predicate as §3.1, which is what stops the file and the shader
disagreeing.

It is exactly the case the version mechanism is for, and `docformat`'s own
comments say so before this feature exists: "a group opacity is the one thing a
reader that flattens the nesting away *cannot* reproduce, since a folder at 50%
over two overlapping children is not the same picture as two children at 50%
each. That is the whole reason folders did not move `VERSION`." An older Umber
folds a group's opacity into each child — `parse_stack` does exactly that
today — and a group of overlapping children then composites differently. A
document that opens showing something else is the masks-and-clipping case that
took the version to 2.

`required_version` emitting the lowest revision that describes the file does the
rest for free: **a document whose folders are all plain still declares 1 or 2 and
still opens in every Umber that came before.** No existing file is shut out, and
`a_document_of_folders_still_declares_the_revision_it_needs` becomes the guard
for both halves — it already checks the *absence* of `opacity` and
`composite-op` on a folder tag, and gains the presence case beside it.

One thing to keep: that test's current assertion must not simply be relaxed. A
plain folder must go on carrying neither attribute, not `opacity="1.0000"`. It
would be harmless and it would be a statement the file does not need to make,
and `required_version`'s whole argument is that a revision number describes what
a file contains.

### 4.4 The reader

`parse_stack` keeps a `Vec<Group>` and folds a group's opacity into the layers
inside, with `GroupOpacityFolded`. That fold does **not** go away, and the three
cases have to be separated:

- **`isolation` absent or `isolate`, with an opacity or a blend mode** — put them
  on the folder. This is what a file this build wrote looks like, and what a
  Krita or GIMP group looks like. No warning: nothing was lost.
- **`isolation="auto"` with an opacity** — the case §3.2 says Umber cannot hold.
  Fold it into the children and warn, exactly as today. Reading it as isolated
  instead would change the picture, which is worse than a warning.
- **Nested deeper than `MAX_DEPTH`** — the group is flattened away by
  `flatten_ill_formed`, so its opacity has nowhere to sit and must still be
  folded. `GroupFlattened` already says so.

The `BlendDropped` warning for a group with a non-exact `composite-op` stays,
and now fires far less: a mode Umber has keeps its place on the folder.

### 4.5 What other applications do, and what they will see

Recorded because the whole point of choosing ORA is that these files travel.

| | group opacity | pass-through | writes `isolation` |
|---|---|---|---|
| GIMP 2.8.6 | yes | no — all groups isolated | — |
| Krita 2.7.5 | yes | no — all groups isolated | — |
| MyPaint (historically) | yes | all groups non-isolated | — |
| Krita, current | yes | yes, "Passthrough" | yes; `auto` ⇔ passthrough |
| GIMP 2.10+ | yes | yes, "Pass through" mode | not verified |
| Umber, today | no | every folder | **no, and §4.1 is the bug** |

The first three rows are the survey in the OpenRaster stack-specification
proposal that introduced `isolation`, so they describe the versions of the day
rather than current builds; both applications have since gained an explicit
pass-through group mode. Krita's mapping is documented and is the one to copy:
passthrough = `isolation="auto"`, and `passthrough == false` is
`isolation="isolate"`.

The consequence for `SaveWarning`: an **isolated** folder is a real
interoperability loss for a reader that flattens the nesting, in exactly the way
it is for an older Umber, and that is where it should be said. A pass-through
folder is not, and must not start warning.

---

## 5. The four things that reuse the composite pass

`export_rgba`, `pick_colour`, `probe_canvas` and the autosave's capture reuse the
*screen* pass with an export flag, deliberately, so there is no second copy of
the blend maths. All four take a `&[LayerDraw]` and pass it straight through, so
**all four are untouched**, which is why the markers must be fields on
`LayerDraw` and not a second argument to `composite`. `render_export` is the
shared half and stays shared.

What does change is the two places a draw list is *constructed*, and they must
change together or a saved file disagrees with the screen:

- **`Editor::layer_draws`** (`editor.rs`) — five call sites: the screen frame,
  `pick_colour`, the export, the Save's `mergedimage.png`, and canvas-as-brush-tip.
- **`Candidate::draws`** (`autosave.rs`) — the flattened preview the autosaved
  file carries. Its comment already says the two rules have to be the same rule.
  `LayerMeta` happily already carries `opacity`, `blend`, `depth` and `folder`,
  so this is the filter changing and nothing else.

Two details:

- **`probe_canvas`** takes the stack with the wet stroke included, so a smudging
  brush inside a group at 50% picks up the group as faded. That is what the
  painter can see, so it is right, and nothing is needed for it.
- **The float still works, unchanged, and inside a group.**
  `CanvasRenderer::float_preview` answers `(layer slot, preview slot)` and
  `layer_draws` swaps one for the other; the preview slice holds what the layer
  will hold, so it composites at the right stack position, under the right blend
  mode, at the right opacity — and now, additionally, inside the right group's
  accumulator, without `render_float` or `transform.wgsl` learning anything. The
  one thing to watch is the swap's own test: with `slot` an `Option`, `from ==
  slot` must not fire on a folder's `None`.

---

## 6. Clipping

Today a clipped layer answers to "the alpha of the nearest *unclipped* layer
below it", through one running `clip_alpha` set after that layer's own mask and
its wet stroke, and a clipped layer at the bottom of the stack shows nothing.
`layer-folders.md` §3 says clipping reaching across a folder boundary is correct
rather than overlooked, because a pass-through folder *is* its contents in place.

**The rule: an isolated group confines it; a pass-through folder does not.**

- **Inside an isolated group**, `clip_alpha` is zeroed on the push. A clipped
  layer at the bottom of the group therefore shows nothing — not because that
  was chosen, but because isolation means the group starts from a transparent
  backdrop and there is genuinely nothing under it. Choosing otherwise would
  require reaching outside the accumulator, which is the definition of not being
  isolated.
- **Across a pass-through folder**, nothing changes at all: the folder is not in
  the draw list, so there is nothing to push and every existing document
  composites identically.
- **A group closes as an unclipped entry**, writing `clip_alpha` from its own
  composited alpha before its opacity — the same `select(0.0, ..., visible &&
  opacity > 0.0)` line a layer uses, so a layer clipped to a group and a layer
  clipped to a layer cannot behave differently.

This is the third correction to §7, which assumed clipping had to be pushed and
popped. §2.1 has the argument: nothing reads the restored value.

**Photoshop and Krita disagree here, and the rule above lands on Photoshop's
side.** In Photoshop a clipping mask is confined to its group; the documented
workaround for clipping to a base layer *outside* a group is to set the group's
blend mode to Pass Through, which is exactly the split above. Krita's equivalent
is alpha inheritance, and its manual is explicit that it is confined to the
layers below it **in the same group** — with no pass-through exception, so Krita
confines it in both cases and Umber's pass-through behaviour differs from
Krita's today, before this feature. Neither reads Umber's `umber-clip`, so
nothing in a file turns on this.

*(The Photoshop pass-through claim is from a community answer rather than
Adobe's documentation and I have not verified it against the application.)*

**A clipped *folder* is deferred, and deliberately.** `Layer::clipped` is a
public field and nothing stops it being set on a folder today —
`required_version` skips folders explicitly for that reason, so the flag cannot
push a document to a revision it does not need. Making it mean something is
cheap in the shader (multiply the group's result by `clip_alpha` and do not
write `clip_alpha`) and expensive everywhere else: it brings back the second
array §2.1 removed, it needs `umber-clip` written on a `<stack>`, and it needs a
*second* clause in `required_version` — because an older Umber dropping it shows
a group painting where it should not. Three costs for a feature nobody has asked
for. Leave the flag meaningless on a folder, and leave `required_version`'s
folder skip and its comment exactly as they are.

---

## 7. The interface, and the numbers

**`MAX_LAYERS` still works out, exactly, with no slack.** `LayerStack::MAX` is
64 and bounds stack **entries, folders included** — "stricter than the array
needs today… because a folder that composites its contents as a group *will*
occupy an entry in that array". Cash that in:

```
draws = layers + isolated non-empty folders  ≤  layers + folders  =  entries  ≤  64
```

So the draw list can never exceed the uniform array, and `composite`'s
`min(MAX_LAYERS)` truncation stays unreachable — which §2.3 requires, because a
truncated list would cut a group open. The cap was tightened in advance and is
now exactly right. It is worth a CPU test that says so, because the equality is
the kind of thing a later change to either number breaks silently.

**`Editor::active_draw_index` is the one thing here that fails silently**, and it
gets *more* dangerous, not less. Today it counts `!is_folder`; it must count
"entries that produce a draw", so an isolated folder below the active layer
shifts it. And it must go on answering `u32::MAX` for a **selected folder, even
an isolated one** — because an isolated folder now genuinely has a draw index,
so the naive repair succeeds and hands the shader a stroke to blend into a group
close. A folder is not somewhere to paint; that has not changed. This is the
fourth correction to §7, which says the mapping "has to count the isolated
folders that now do reach the draw list" and does not say the second half.

**The controls become drawn.** `panels::layers_body` has two branches already:
`!is_folder` draws the mask, clip and lock toggles and, further down, the blend
dropdown and the opacity slider; `is_folder` draws a lock and the sentence "A
group carries its layers", with a tooltip explaining that a group has no blend
mode and no opacity of its own. What is needed:

- The blend-and-opacity row stops being gated on `!is_folder`, which is the
  whole change. It writes `Layer::blend` and `Layer::opacity`, which a folder
  already has, at the values a pass-through folder means — so a folder's first
  reading of that row is Normal at 100%, which is pass-through, which is what it
  was.
- The sentence and its tooltip go. Something has to replace them, because the
  pass-through/isolated distinction is now visible and derived: a painter who
  drops a group to 99% has changed how a Multiply layer inside it composites,
  and nothing on screen says so. The honest minimum is a mark on the row — the
  same word the file uses, so it is the word they will meet in Krita — shown
  when the folder is isolated, with the tooltip carrying §1's sentence.
- The mask and clip toggles stay undrawn for a folder, per §6 and because a
  folder has no slot to mask.

Everything else in the panel is already right: a folder's row draws a folder
mark rather than a thumbnail, and the drag, the ticks and the chevrons are
untouched.

---

## 8. Undo, the autosave, thumbnails

- **The undo history is untouched, and `history::VERSION` does not move.** No
  slot changes hands: grouping, re-nesting and folding free none and reassign
  none, and changing a folder's opacity frees none either. Nor does it become an
  undoable edit — `EditKind` "has a variant only for something the engine can
  restore", and a *layer's* opacity is not undoable today, so a folder's must
  not be. Two rows that undo identically must not have two names, and a row that
  undoes nothing must not exist at all.
- **The autosave** needs `Candidate::draws` changed with `Editor::layer_draws`
  (§5) and nothing else. A folder still reads back as no pixels, so the
  snapshot's own `pixel_index` is unaffected.
- **Thumbnails** stay a folder mark. An isolated folder does now have an honest
  picture — the group's composited result — which makes the third mode for
  `thumbnail.wgsl` a slightly better idea than it was, and it is still out of
  scope: `thumbnail.wgsl` reduces one slice and a folder has none.

---

## 9. Tests

Without a GPU, in `umber-core` and `umber-app`:

- **The markers balance.** Opens equal closes over any well-formed stack, for
  every arrangement of isolated and pass-through folders, empty folders
  included. This is the invariant that lets the shader have no guard.
- **A document of pass-through folders produces today's draw list**, entry for
  entry, `open` zero throughout. The regression that matters most, and it needs
  no device.
- **An empty isolated folder produces no draw at all**, which is what makes
  `MAX_GROUP_DEPTH` seven.
- **`active_draw_index` counts isolated folders**, and still answers `u32::MAX`
  for a selected folder of either kind.
- **`draws ≤ MAX_LAYERS`** on a full stack of nested isolated folders.
- **`required_version`** — a plain folder still declares 1, and still carries
  neither attribute; a folder at 50% declares 3.
  `a_document_of_folders_still_declares_the_revision_it_needs`, extended.
- **A save and a reopen do not move a folder's opacity**, the folder-shaped
  version of `saving_and_reopening_does_not_move_a_pixel`.
- **`isolation="auto"` round-trips**, and an absent `isolation` reads as
  isolated.

On the GPU, through `composite_pixel` in `gpu_pipeline.rs`:

- **The point of the feature.** Two overlapping opaque children in a folder at
  50%, against the same two children at 50% each outside a folder. The two must
  differ, and the first must equal the flattened-then-faded result.
- **The thing that must not change.** A Multiply child inside a *pass-through*
  folder composites exactly as it does with no folder at all. This is the
  regression the whole split exists to prevent and it is invisible in any test
  built only out of Normal layers.
- **A group at opacity 1 in Normal is the exact identity**, which is the shader
  half of the derivation in §3.1 — and it is a test that must compare bytes
  only where the arithmetic has nothing to round; see CLAUDE.md on assertions
  about pixels that have been through a shader.
- **Nesting.** An isolated group inside an isolated group, against the same
  result computed a level at a time.
- **A hidden folder still closes its group** — the ordering trap in §2.3. Build
  it as a hidden isolated folder with a visible layer above it, and check that
  the layer above lands on the backdrop and not on the group's accumulator.
- **Clipping.** A clipped layer at the bottom of an isolated group shows
  nothing; the same layer at the bottom of a pass-through folder still answers to
  the layer beneath the folder; a layer clipped to a group answers to the
  group's alpha.

---

## 10. A staged plan

**Stage 0 — write and read `isolation`.** No shader, no model, no version bump,
and useful entirely on its own: it fixes §4.1, which is a picture Umber gets
wrong in other applications *today*. `folder_xml` gains `isolation="auto"`;
`parse_stack` reads it and keeps the fold for the `auto`-with-opacity case. Do
this first whatever happens to the rest.

**Stage 1 — the draw list, with nothing isolated.** `LayerDraw::slot` becomes an
`Option`, `open` and the close marker are added, both constructors emit them,
`active_draw_index` counts draws, and `is_isolated` is wired to a predicate that
is always false. The uniform packing changes; the shader does not. Every test in
§9's first list can be written here, and the shipped behaviour is byte for byte
what it is now. **This is where the risk is bought down**: everything structural
lands with nothing to see.

**Stage 2 — the shader. This is the risky piece.** The accumulator array, the
push and pop, the clip rule. Factor the per-layer body into a function first, so
the fallback in §2.4 stays cheap. Measure before and after — RGA for scratch and
occupancy, a frame time locally, and the Mali offline compiler for the target
that cannot survive a spill. The GPU tests in §9 land here, and the
pass-through-Multiply one is the gate.

**Stage 3 — the model and the file.** `is_isolated` starts answering truthfully,
`required_version` gains its clause, `folder_xml` writes `opacity`,
`composite-op` and `isolation="isolate"`, the reader puts them on the folder,
and `SaveWarning` names an isolated folder as an interoperability loss.

**Stage 4 — the interface.** Ungate the blend and opacity row, retire the "A
group carries its layers" sentence, and say somewhere that a group below 100% or
off Normal is isolated.

**Is there a useful first piece?** Stage 0, emphatically — a bug fix that stands
alone. And stages 1 and 2 together are a coherent piece that ships nothing
visible and settles the only question this design cannot answer by reading.

**A folder opacity without a folder blend mode** is *not* a useful split: it
needs the whole of stage 2 and saves only the dropdown, and it would leave the
derivation in §3.1 half-stated. **Isolation without either** is the split that
matters, and it is stages 1 and 2.

---

## 11. Not settled

- **Whether the accumulator array lands in registers or in scratch**, on any
  driver. §2.4 names the four things that would say. Nothing else here is in
  serious doubt.
- **Whether a clipped folder should mean something.** §6 defers it; it is a
  coherent feature that costs a second array, a second `required_version`
  clause, and `umber-clip` on a `<stack>`.
- **Whether "pass-through group with an opacity" should eventually be
  representable.** §3.2. It needs `Layer::isolated`, and the flag and the
  version clause have to move together.
- **What GIMP 2.10 writes for `isolation` on a pass-through group.** The table in
  §4.5 marks it unverified; it wants an actual file.
