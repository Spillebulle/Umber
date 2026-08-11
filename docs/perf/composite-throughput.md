# Composite throughput

How long a frame takes to draw, for a given set of resident layers, and where
that time goes.

This is the *speed* half of the large-document problem. Memory layout is
`docs/perf/layer-tiling.md`'s, residency and paging are
`docs/perf/layer-residency.md`'s, and allocation accounting is a third
document's. What is here is the frame: one composite pass, its fragment shader,
the passes recorded beside it, and the CPU work that runs whether or not
anything changed.

The case in front of us is a real Clip Studio document: **54 layers on a
20000 x 5000 canvas**, drawn into a 4K window. Every figure below is worked out
for that document unless it says otherwise, and **none of them has been
measured** — §8 is the experiment that would settle each one, and it is a
required part of this document rather than an appendix. CLAUDE.md records three
separate occasions on which a number reasoned about turned out to be wrong by an
order of magnitude, twice in this same crate. Read §1 and §2 as arithmetic to be
checked, not as findings.

**Revised after review.** `docs/perf/critique-residency-composite.md` reviewed
this document against `docs/perf/layer-residency.md` and against the code. What
changed: R5's cut was in the wrong place and is now correct (§5.1); the mip
argument had a wrong constant in it and the sampler's `mipmap_filter` is
load-bearing in a way §4 missed (§4.0); the proxy array of §4.5 is now one
object shared with residency, sized by a rule rather than a constant; §5.3 gained
the argument that actually settles the two cache designs, which the critique
found and neither author had made; and §4.5a names the unfiltered band both
documents had left silent. Where the critique and this document still differ, the
disagreement is argued in place rather than removed — §4.0 and §5.5 are the two.

---

## 0. The recommendation, and the ranking

**Two changes are exact, cheap and should be built first: stop submitting draws
that contribute nothing, and stop shading the part of the window the interface
covers.** Together they are arithmetic on the CPU and one `set_scissor_rect`,
they cannot move a pixel, and on a document full of hidden folders they may take
a third to a half off every frame. After that the answer splits by what the
painter is doing: an **exact screen-space cache of the stack below the active
layer** is what makes painting cheap, a **dirty-region composite** is what makes
everything else free, and **mip levels** are the only thing that fixes zooming
out. Mips are also the only item here that changes what is on screen, and §4 is
the argument about exactly how.

Ranked by benefit divided by risk. "Exact" means the bytes written to the
surface are unchanged, and a test can assert that.

| # | Change | Benefit | Risk | Exact? |
|---|---|---|---|---|
| R1 | Elide draws that contribute nothing (§3.1) | **conditional on a number nobody has**; see below | very low, one subtle rule | yes |
| R2 | Scissor the composite to the canvas region (§3.2) | 20–35% of fragments | very low | yes |
| R3 | Housekeeping: per-frame device objects, list culling, ants (§7) | small each, free | very low | yes |
| R4 | Interleave the two uniform arrays (§6) | micro | nil | yes |
| R5 | Screen-space under-cache, `Rgba32Float` (§5) | halves the loop while painting and while idle | moderate | yes, by construction |
| R6 | Dirty-region composite into a persistent target (§3.3) | a stroke frame becomes a few hundred pixels | high | yes, if the damage rect is right |
| R7 | One proxy array, shared with residency (§4) | ~9x at fit-to-view; the only fix for zoom-out | high | **no** |
| R8 | A hand-written Normal path in the layer loop (§2.4) | small, and only on integrated graphics | low | no (last bit) |

**R1's rank is the one thing in this table that could move, and it is not a
judgement — it is a missing measurement.** Its whole benefit is the fraction of a
real stack that is hidden or at zero opacity, and nobody has read that off a
file. At a third it is the cheapest large win in the programme and should ship
first; at a tenth it is a couple of gigabytes against a twelve-gigabyte overage
and R7 should go first instead. `survey-documents.rs` already walks real
documents and could report hidden-layer **count and bytes** in one run. Get that
before committing to the order. `docs/perf/layer-residency.md` rests its own
headline on the same unread number.

R5 and R6 want the same persistent screen-sized target, so R5 should be built in
a shape R6 can extend — and R5's seeding mechanism turns out to be the same
shader change `docs/perf/layer-residency.md` §6's export fold needs, which is
§5.5. R7 should not be built until R1, R2 and R5 have been measured, because it
is the one that can make the picture worse and it may turn out that the others
put fit-to-view inside a frame without it.

**R7 is no longer this document's own object.** §4.5 used to propose a
quarter-resolution proxy array of its own and flag it as probably the same thing
residency wanted. It is. There is one proxy array, sized by a rule, and §4.5 is
now the joint statement of what that rule has to satisfy.

---

## 1. What one fragment does today

`composite.wgsl`'s `fs` runs once per pixel of the **whole window** — there is no
scissor and no viewport call in `CanvasRenderer::composite`, and the vertex
shader emits an oversized triangle covering the attachment. A pixel outside the
document's uv range returns the surround colour immediately; a pixel inside runs
the loop.

For a document of `N` draws of which `M` carry masks, one inside fragment does:

| | count | notes |
|---|---|---|
| `textureSampleLevel` of the stroke scratch | 1 | `R8Unorm`, always |
| `textureSampleLevel` of a layer slice | `N` | **unconditional**, before the visibility test |
| `textureSampleLevel` of a mask slice | `M` | behind a wave-uniform `if` |
| `textureSampleLevel` of the per-dab colour | 0 or 1 | only on the active draw of a smudging stroke |
| uniform reads | `2N` x `vec4` | two arrays, 3056 bytes apart |
| `composite_over` | up to `N` | early-outs at `src.a <= 0.0` |
| `blend_rgb` switch | up to `N` | falls through to Normal in one arm |

Three things about that list are worth stating plainly because they are easy to
get backwards.

**The layer fetch is unconditional and that is deliberate.** The comment beside
it says so: a hidden layer still has to bound whatever is clipped to it, to
nothing, so `lay` is sampled before `visible` is tested. Two fetches for a layer
that contributes no pixels is the stated price of that being one rule rather
than two. It is also the single largest avoidable cost in the pass, and §3.1 is
how to get it back without touching the shader.

**`composite_over` early-outs on an empty source.** `if (src.a <= 0.0) return
dst;` fires wherever the layer is transparent at that pixel, which on a real
stack is most layers at most pixels. So the ALU cost of the loop is *far* below
its worst case, and the fetch cost is not: the tap happens whether or not the
result is used. This is why §2 concludes what it concludes.

**The `blend_rgb` switch is wave-uniform.** `mode` comes out of the uniform
indexed by the loop counter, so every fragment in a wave takes the same arm. It
is not a divergence cost; it is a code-size and register cost, and the two
non-separable arms (`set_lum`/`set_sat`/`clip_color`) are what set the shader's
register high-water mark for every document, including one that uses none of
them.

At the ceiling — `MAX_DRAWS` is 191, from 64 layers plus a 127-slice effect
budget — the same fragment takes 191 layer fetches. Nobody has reached that; the
54-layer document is 28% of it. An effects-heavy document at 4K would be 3.5x
every figure below.

---

## 2. Where the time goes

### 2.1 At 1:1

3840 x 2160 is 8.29 M fragments. With `N = 54` and `M = 10`, that is **539 M
texture taps per frame**.

Bandwidth is the better estimate than tap count, because a bilinear tap reads
four texels but neighbouring fragments overlap. At 1:1 the footprint per layer
over the whole screen is about `(W+1) x (H+1)` texels, so 33 MB per layer per
frame at four bytes a texel. Fifty-four layers plus ten masks is **about 2.1 GB
of texture read per frame**, which at 60 Hz is **127 GB/s**.

That is a fraction of a discrete card's bandwidth (an RTX 3080 is 760 GB/s) and
it is *two to three times* what an Intel integrated GPU has, sharing it with the
CPU. On integrated graphics the composite at 54 layers cannot reach 60 Hz at 4K
by this arithmetic alone.

Tap rate is the second wall. 539 M taps at 60 Hz is 32 Gtap/s. A 3080 is around
460 Gtexel/s and an Iris Xe around 30–50, so the same split: comfortable on a
card, marginal on an iGPU.

ALU is third. A rough count is 10 instructions for a draw that early-outs and 30
to 40 for one that composites; at a plausible mix that is 1000–2000 per
fragment, so 8–17 GFLOP per frame and 0.5–1 TFLOP/s at 60 Hz. A 3080 is 30
TFLOP/s; an Iris Xe is about 2. **So on a discrete card the composite is fetch
and bandwidth bound, and on integrated graphics it is bound by all three at
once.**

### 2.2 At fit-to-view, which is the case that is actually broken

Fitting 20000 x 5000 into a 3000 x 1900 canvas region is a zoom of about 0.15.
Only 3000 x 750 = 2.25 M fragments enter the loop, so *fewer* fragments run —
and the frame gets much more expensive, not less.

The sampler is reading level 0 at a stride of 6.7 texels. A 64-byte cache line
holds 16 `Rgba8` texels, so consecutive fragments along x share a line only
every 2.4 fragments, and consecutive rows land in unrelated lines entirely. A
bilinear tap touches two rows. So the traffic per fragment per layer is roughly
two cache lines with 2.4x reuse, call it **53 bytes** where at 1:1 it was 4.

2.25 M fragments x 54 layers x 53 bytes is **6.5 GB of texture read per frame**.
At 60 Hz that is 390 GB/s; at the 16 Hz the marching ants ask for (§7.5) it is
still 104 GB/s, for a document nobody is touching.

There is no cache to help. Fifty-four layers each streaming a different 100 Mpx
texture is a pure streaming workload against a 4–6 MB L2, with no reuse between
layers and almost none within one.

**This is the number that says zooming out is broken, and it is a cache-line
effect rather than a texel-count effect.** That matters, because it means the
fix has to change the *footprint*, not the tap count. §4.

**It has been arrived at twice, independently, and the two agree.**
`docs/perf/layer-residency.md` §2.5 builds the same figure from a different
window size and a different starting assumption and lands on 5.7 GB once its own
arithmetic is corrected for the fragments that return before the loop — the
critique's finding 6. Two constructions agreeing within 15% is a better position
than either had alone, and it is worth saying because the *conclusion* is what
survives a 2x error here: the proxy is still the answer at 3 GB.

### 2.3 What that ranks

- Anything that removes a whole draw removes its full share of both figures, at
  every zoom. That is R1 and R5.
- Anything that removes fragments removes its share too. That is R2 and R6.
- Only a mip changes the bytes-per-tap, and only at zoom below 1. That is R7.
- Nothing here suggests the ALU is worth attacking on a discrete card, and R8 is
  ranked last for that reason.

### 2.4 The Normal path the loop does not have

`composite.wgsl` writes the wet stroke's Normal case out by hand rather than
routing it through `composite_over`, and the comment explains why: the general
form divides the source by its own alpha and multiplies it back, so the two
agree in exact arithmetic and differ in the last bit of floating point, and the
hand-written line is the one that matches what the fixed-function blender does
at commit.

The **layer loop** does not have that fast path. Every Normal layer pays two
reciprocals and a multiply-back that cancel exactly. Deriving it: with
`blended = cs = src.rgb / src.a`, the compositing formula's middle term is
`src.a * dst.a * cs = dst.a * src.rgb`, so `co` collapses to
`src.rgb + (1 - src.a) * dst.rgb`. Identical in exact arithmetic; not identical
in f32.

So a Normal fast path in the loop would be the same decision already taken one
line above, applied consistently, and it would be marginally *more* accurate
than what is there. It is still **a pixel change**: the layer loop's output
reaches `export_rgba`, `pick_colour`, `pick_patch`, `probe_canvas` and the
autosave capture, so a last-bit difference reaches a PNG and an eyedropper
reading. Given §2.1 says ALU is not the binding constraint anywhere but
integrated graphics, this is ranked last and should not be built without a
measurement showing it buys something on the machine that needs it.

---

## 3. What is bounded today, and what is not

### 3.1 R1: draws that contribute nothing are still submitted

`Editor::effected_draws` walks `LayerStack::layers()` and emits a `LayerDraw` for
every entry with a slot, `visible` folded down from the folder's eye by
`effective_visible`. A hidden folder holding twenty layers therefore produces
twenty draws, each of which the shader fetches and then skips at
`if (!visible || opacity <= 0.0) { continue; }`.

Real Clip Studio documents are full of hidden folders — sketch layers, colour
roughs, alternate versions, reference photos. **How much of *this* document is
hidden is not known and is the single most load-bearing unmeasured number in
either performance document.** A third would make this the cheapest large win in
the programme; a tenth — entirely plausible for a finished commission whose
roughs were deleted — would make it a footnote. It costs exactly its share of
§2's figures either way, so the *mechanism* is right whatever the fraction; what
the fraction decides is the build order. See §0 and §10.

The elision rule, stated precisely, because getting it wrong is silent:

- A draw with `!visible || opacity <= 0` contributes nothing to `acc`. That is
  the shader's own `continue`.
- Such a draw *does* affect the composite if it is **unclipped**, because
  `clip_alpha = select(0.0, lay.a, visible && opacity > 0.0)` writes zero, which
  is what makes a run of clipped layers above a hidden layer show nothing.
- So: an invisible **clipped** draw may always be dropped. An invisible
  **unclipped** draw may be dropped only if the run of draws immediately above
  it contains no clipped draw before the next unclipped one.
- **The active draw is never dropped**, whatever its flags. A painter may select
  a hidden layer, and the stroke preview reads `i == v.active_index`.

Two traps come with it, and both are the shape CLAUDE.md already names:

- **`active_draw_index` has to be remapped after elision, not before.** It is
  already a mapping from stack position to draw position, and it already answers
  `u32::MAX` for a folder; eliding draws shifts it again. Compute the elision and
  the active index from one pass over one list, or the stroke previews on the
  wrong layer and jumps at pointer-up.
- **`bake_effects` takes the same list.** An effect on a hidden layer is a bake
  and a draw for a mark nobody can see. Eliding the layer's draws must elide its
  effects with them, and `BakedStack::active_index` comes back out of that
  function, so the two have to agree.

This is exact: the composite's output is bit-identical, because the shader
already produces the identical `acc` and `clip_alpha` sequence. The guard is a
GPU test that composites a stack with hidden layers and folders, elided and not,
and compares bytes — including a case with a clipped layer directly above a
hidden one, which is the only case the rule can get wrong.

**The rule covers `opacity <= 0` on exactly the same proof**, and that is worth
saying because a painter dragging a layer to zero rather than clicking the eye is
common. Line 259's `select(0.0, lay.a, visible && opacity > 0.0)` and line 262's
`if (!visible || opacity <= 0.0)` treat the two conditions identically, so the
elision rule above is stated over both and needs no second argument.

**Where the rule refuses, residency has a second answer, and the two compose.**
`docs/perf/layer-residency.md` §2.2 keeps the draw and gives its slot no
resident pixels, on the proof that the sampled value is provably ignored. Read
side by side the two look like a contradiction; they are not, because they act
on different things. So the merged rule is:

- **Elide the draw where the rule above permits.** That removes the draw, the
  fetch and the pixels.
- **Keep the draw where it does not, and let residency free its pixels.** That
  removes the pixels only.

For *this* document's purposes the second half buys nothing: a kept draw still
costs its texture tap, which is the whole of §2's cost model. It is a memory win
and not a throughput win, and it is worth stating plainly so that nobody reads
the merged rule as making R1 unnecessary. Both halves come out of the same single
pass that already has to compute `active_draw_index`.

### 3.2 R2: the pass shades the whole window

`CanvasRenderer::composite` records no `set_scissor_rect` and no
`set_viewport`. The oversized triangle covers the attachment, which is the
swapchain image, which is the whole window. egui then draws its panels over the
top.

`ui::draw` already computes exactly the region that is not covered:
`canvas_rect`, the `CentralPanel`'s rect, whose comment says "the canvas is
drawn by the GPU beneath egui, so this panel only reports its rect and stays
transparent". It is already in `Editor::canvas_size` and `canvas_pivot`, in
physical pixels.

At `metrics::PANEL`'s 264 points a side plus the tool rail, at a 4K display's
usual scale, the interface covers **20–35% of the window**. Every document pixel
under a panel runs the full 54-layer loop and is then overdrawn.

The change is:

- `LoadOp::Clear` the target to the surround colour instead of black. The
  surface is deliberately non-sRGB and the shader writes `v.backdrop.rgb`
  straight out with no encode, so a clear to the same value writes the same
  bytes.
- `pass.set_scissor_rect(canvas_rect_in_physical_pixels)` before the draw.

Every pixel that is visible is unchanged. The only pixels whose bytes move are
document pixels behind opaque panels. Two things to check rather than assume:
whether any part of the interface outside `canvas_rect` is translucent (if so,
clearing to the backdrop is what makes this safe, and it is strictly safer than
today), and the window's own edges under a compositor.

The checkerboard is `floor(screen.x / v.checker)` in **attachment** coordinates.
A scissor does not move it. A *viewport* would, and so would rendering into an
offscreen target whose origin is the canvas rect (§3.3) — that one needs the
offset threading through, and forgetting it shifts the checker phase, which is
visible.

### 3.3 R6: dirty-region compositing, and how far the existing machinery reaches

While a stroke is in flight, the only thing on the canvas that changes between
frames is the wet scratch, over the cells this frame's dabs reached. While
nothing at all is happening — a tooltip, a hover, a marching-ants frame — nothing
on the canvas changes and the whole stack is recomposited anyway.

**What exists.** `umber_core::damage::TileMask` accumulates a 64-pixel grid
beside `StrokeBuilder::bounds`; `pieces` merges neighbours along each row and
clips them to the bounding box; `commit_stroke` sets a scissor per piece and
draws one quad under each. So the *shape* of the machinery is right and is
already trusted with the undo patch.

**What is missing, and it is three things.**

1. **The damage is cumulative over the stroke, not per frame.** `bounds` and the
   `TileMask` describe everything the stroke has reached, because that is what
   the commit and the patch need. A dirty-region composite needs the union of
   *this frame's* dabs, computed by the same rule — the axis-aligned box of the
   rotated quad of the scattered dab, with the short semi-axis taken from the
   **dab's** `aspect` and not from `Brush::dab_ratio`. CLAUDE.md records that
   under-tight rect being got wrong four times, and the failure here is the same
   one wearing different clothes: an uncomposited streak left on screen behind
   the brush. Feed it from the same numbers `bounds` is fed from, bound to a name
   once, or do not build this.
2. **A document rect has to become a screen rect.** That is a camera transform
   and a rounding rule (expand outwards to whole pixels, plus one for the
   bilinear tap's neighbour). Cheap, and testable in `umber-core` beside
   `ScrollSpan`.
3. **The target cannot be the swapchain image.** Its contents are undefined at
   acquisition and it is a different image each frame. A dirty-region composite
   needs a persistent, canvas-region-sized target and then a blit into the
   swapchain. The blit is a fullscreen triangle with a `textureLoad`, exact, and
   at 8.29 M pixels it is 66 MB a frame — about 4 GB/s at 60 Hz, negligible
   against §2's figures. It is also where R2's scissor naturally lives, since the
   target is canvas-region-sized to begin with.

**The invalidation predicate is the risk.** The cached output is valid only while
the camera, the whole draw list (every slot, opacity, blend, visible, mask,
clipped flag), the background, the surround colour, the document size, every
slot revision and the effect bake count are unchanged, and the only delta is new
dabs. That is a long list and it is the "enforced at five call sites, forgotten
at the sixth" failure waiting to happen. The way to make it structural is to
**hash the uniform block that was uploaded** — `ViewUniforms` is `Pod`, so the
predicate is "the bytes I am about to write equal the bytes I wrote last frame",
which cannot be forgotten because it is derived from the thing itself. Slot
revisions are not in that block, so they need adding to the comparison
separately; `CanvasRenderer::slot_revision` already exists and is already bumped
inside every method that writes a slice, which is exactly the property
`docs/thumbnails.md` relies on.

The reward is large. A 30 px brush at 0.15 zoom damages about five screen pixels
of radius per frame; a stroke frame goes from 2.25 M fragments x 54 layers to a
few hundred fragments plus a blit. An idle frame goes to a blit alone.

### 3.4 None of this breaks "the entire stack composites in ONE pass"

Worth writing down, because R5, R6, the proxy and residency's bake all add
passes, and a reader who stops at the pass count will re-litigate an invariant
CLAUDE.md states in strong terms:

> The entire stack composites in ONE pass. Do not "simplify" this into a pass
> per layer.

**The rule is about a per-layer cost that scales with the stack on a steady-state
frame** — a pass and a full-screen bandwidth round trip each, N of them, every
frame, for ever. That is what makes a fifty-layer document fifty times the
bandwidth of a one-layer document at rest.

A memoisation pass is a different shape. It runs **on change** and is read with
one tap, so on a steady-state frame it is O(1) in the stack, not O(N). R5's fill
is O(N) on the frames it runs and O(0) on the frames it does not; R6's blit is
O(1) always; the proxy's refresh is O(damage) at commit. None of them is a pass
whose *count* grows with the layer count on a frame where nothing changed, which
is the quantity the rule protects.

The one thing in either document that genuinely is O(N) passes is residency §6's
export fold — and that is on the export path, once, where the alternative is not
exporting at all. That is why it is defensible there and would not be on the
screen path, and saying so is what lets a reviewer accept three new passes
without reopening the argument. (It should also fold as many layers per pass as
there are resident lines rather than one at a time; the pass already takes up to
191 draws.)

---

## 4. Zoom-out, mips, and exactly what they cost in quality

This is R7 and it is the only recommendation here that changes what is on
screen. §2.2 is why it is worth considering at all: at fit-to-view the composite
reads roughly 6.5 GB a frame to produce 2.25 M pixels, and no amount of eliding
draws or scissoring fragments changes the bytes-per-tap.

A correct mip at LOD ~2.7 collapses the per-layer footprint to about the screen
area: 2.25 M fragments x 4 bytes x ~1.25 for trilinear x 54 layers is **0.61 GB a
frame**, roughly **11x** less. It also removes the aliasing, which today makes a
zoomed-out view of a detailed document shimmer as the camera moves.

The prefiltered read is a **better** filter than what happens today and it is not
*the* correct filter; §4.4 is the whole of why, and it is the formulation both
performance documents should use.

### 4.0 The sampler filters no mips at all, and that is load-bearing

Verified: `Shared::new` sets `mipmap_filter: wgpu::MipmapFilterMode::Nearest`
(`canvas.rs:2347`). §4's first draft costed the mipped read at "x1.5 for
trilinear" — a figure too high on either setting, for a filter the sampler is not
configured to do. The mistake was not the constant, it was assuming the sampler
already did the thing the constant was pricing; the sampler was read and the word
was written anyway. Two things follow, and they point opposite ways.

**With `Nearest`, the read is x1.0 and the LOD pops at every octave.** Not only
at a proxy boundary: a continuous zoom gesture snaps at every power of two, all
the way down. That is a far more visible artefact than one seam, and it is the
thing that would get reported.

**With `MipmapFilterMode::Linear` there is no popping and the read is about
x1.25** — two bilinear taps from two levels, the coarser of which has a quarter
of the footprint, so the second level is nearly free in bandwidth terms even
though it doubles the tap count.

**Here this document disagrees with the critique's framing.** The critique
records both halves and reports the x1.0 as R7's bandwidth win being "slightly
better than stated". It cannot be banked: its own next sentence says `Linear`
"must be made in the same commit as the first mip chain", and the two cannot both
be true of the shipped design. The honest choice is `Linear` and x1.25, and the
figure above is stated at that. A 20% bandwidth difference is not worth an
artefact this project would refuse in any other control.

**The sampler is shared, and that decides how the change is made.**
`Shared::sampler` is bound by the composite, the dab pass's tip, the paper grain
and the transform resampler. Changing `mipmap_filter` on it is inert for every
one of those *today* — none of their textures has a second level, and
`mipmap_filter` only ever selects between levels that exist — so the global
change is safe as long as nothing else acquires a chain in the same breath. The
tip and the grain are the ones that plausibly would. **So: change it globally,
say in the comment that it is inert for every texture with one level, and give
the proxy its own sampler only if a tip or a paper mask ever gains a chain.** The
alternative — a second sampler now — adds a binding to the composite's layout for
a difference nobody can see yet.

### 4.1 The sRGB trap, which is already set

`LAYER_FORMAT` is `Rgba8UnormSrgb`. A mip generated by sampling level `n-1`
through an **sRGB view** and rendering into level `n` through an **sRGB view**
is correct: the texture unit decodes to linear on read, the shader averages four
linear values, the ROP encodes on write.

A mip generated through `LAYER_FORMAT_LINEAR` views averages the *encoded*
bytes. Black and white average to 127.5 encoded, which is linear 0.214 where the
right answer is linear 0.5, encoded 188 — a **60-level error**, showing up as
every reduced view of the document being far too dark.

**The wrong views already exist in this file.** `LayerStore::raw_slot_views` is a
per-slice `Rgba8Unorm` view at `canvas.rs:2106`, built for `flip.wgsl` because a
flip must be an exact texel permutation with no transfer function on either side,
with a comment saying so. They are sitting there, they are per-slice, and they
are exactly what somebody writing a mip generator would reach for. Say it at the
generator.

**Moving to a separate proxy array does not escape this**, and it is worth saying
because it looks as though it might. The proxy holds the same `LAYER_FORMAT`, so
its downsample has the identical trap — and it has one more, because a proxy is
built by reducing the full-resolution array, so the *first* reduction crosses the
same transfer function as every level after it. One generator, written once,
through sRGB views, used for the proxy's level 0 and for every level of its
chain. `docs/perf/layer-residency.md` does not mention this hazard at all; it
belongs in whichever document describes the generator.

Two things that are *not* problems and are worth saying so nobody re-derives
them wrongly:

- **Alpha is unaffected.** An sRGB format encodes RGB only; its alpha channel is
  linear either way. This is the same fact `STROKE_FORMAT`'s docs rely on.
- **Premultiplied storage is what makes a plain average correct.** The layer
  holds premultiplied linear colour, so an unweighted mean of four texels *is*
  the box filter of the compositing algebra. Averaging straight colour would need
  alpha weighting and a divide, and would be wrong at every soft edge. Umber's
  storage choice happens to be the right one here.

Requantising to eight bits at each level compounds: each step is at most half a
level, accumulating as roughly `0.5 * sqrt(n)`. Generating every level from level
0 instead would avoid it at `4^n` taps and is not worth it. The bound belongs in
a comment.

### 4.2 What must not read a mip, and why those zeroes become load-bearing

Every path that decides pixels already names level 0 — `read_layer_rect`,
`read_layer_pieces`, the autosave capture, `write_layer_rect`,
`commit_stroke`'s backdrop copy, `flip_layers`, `thumbnail.wgsl`,
`effect.wgsl`'s `fs_extract`, and every `copy_texture_to_buffer` in the file.
They are correct today **because there is only one level**. Adding levels turns
each of those zeroes from a formality into the thing standing between the
document and silently wrong pixels. That is the same shape as
`.min(MAX_SLOTS)` quietly changing meaning when `MAX_SLOTS` moved from 129 to
256, and it should be recorded the same way: at the constant, in advance.

The sharpest one is the composite's own reuse. **The reduced read must be a
`CompositeParams` field set by the caller, never derived inside `composite()`
from the camera.** `probe_canvas` composites at
`zoom = PROBE_SIZE / (radius * 2)` (`canvas.rs:5260`, `PROBE_SIZE = 8`), which is
a *minification* for any brush over four pixels of radius — a 30-pixel brush
probes at **0.133**, which is past this document's fit-to-view zoom. A LOD
derived from zoom would have a smudging brush picking up colour from a reduction,
which is a change to the document's pixels rather than to the screen's.
`export_rgba` and `pick_patch` are at zoom 1, and `pick_patch` additionally
relies on the sampler landing on texel centres. All three must be told LOD 0
explicitly. This is exactly the failure mode CLAUDE.md records for `Background` —
a per-frame parameter that three internal callers build their own params for is
three more places for an export to stop matching the screen — so the safer shape
is a field with no default.

**A separate proxy array makes this hazard smaller, and that is a real argument
for the merged object.** With a chain on the layer array the discriminator is a
*number*, and a number has a plausible-looking wrong default (zero, derived,
inherited). With a proxy the discriminator is a *binding*: the composite either
has the proxy array bound or it does not, and every non-screen caller simply does
not bind it. `docs/perf/layer-residency.md` §6 already states that rule and it is
structurally stronger than a float. It is not free, though — `probe_canvas`,
`pick_patch`, `pick_colour` and `render_export` all call `self.composite(...)` on
the same renderer, so "not bound" has to travel through `CompositeParams` rather
than being renderer state, or those four inherit whatever the last screen frame
left. A field with no default, whichever form it takes.

**And the probe is already undersampling, which must not be quietly fixed by
this work.** At 0.133 the probe takes a four-tap bilinear of a 60-pixel footprint
to decide what a smudging brush picks up. An area average is arguably what a
smudge *should* pick up, so this looks like a bug the proxy would fix for free —
and it must not be, because changing it changes every existing document's
behaviour and is nobody's brief. Recorded in §10 so that the day somebody notices
it, the answer is not "the proxy work did that".

### 4.3 The pointer-up jump, which is the real objection

A mipped layer array and an unmipped stroke scratch do not agree.

At 0.15 zoom today, both the layer and the wet scratch are point-ish sampled at
level 0, so the preview and the committed result alias the same way and the mark
does not visibly change at pointer-up. With layer mips and no scratch mips, a
thin wet stroke draws aliased over a smoothly minified stack, and then at
pointer-up it moves into the layer, the layer's mips regenerate, and the mark
becomes smooth. **That is the stroke visibly jumping at pointer-up**, which is
the one thing the whole preview/commit discipline exists to prevent, arriving
from a direction that discipline does not cover — `composite.wgsl` and
`commit.wgsl` would still be implementing identical blending maths, and the jump
would happen anyway.

The fixes, in order of how good they are:

1. **Mip the scratch too, at the same LOD.** It is `R8Unorm`, one channel, and
   the region that changed each frame is exactly the damage §3.3 already needs.
   Regenerating a few levels over a small region is cheap. This is the honest
   answer.
2. Hold the mip path off while a stroke is live. **Worse**, because switching LOD
   at pointer-down is a jump at the other end of the gesture, which is the same
   defect with better timing.
3. Ship the jump. Refused.

So: **layer mips require scratch mips.** They are one change, not two, and
costing them separately would understate R7.

### 4.4 Per-layer mip is not the mip of the composite

Compositing is not a linear operation, so downsampling each layer and then
compositing is not the same as compositing and then downsampling. For Normal
source-over on premultiplied colour it is close; for Multiply, the two dodges and
burns and the four non-separable modes it is meaningfully different, because
`B(Cb, Cs)` is evaluated on averaged operands rather than averaged over
evaluations.

It is nevertheless a **much** better approximation than what happens today, which
is one bilinear tap of one texel neighbourhood per layer — that is not an
approximation of the average at all, it is a sample of it. So the honest claim
is: mips make the zoomed-out view *more* faithful to what an export at that
scale would look like, not exactly equal to it, and nothing on screen or in a
comment may say otherwise. `export_rgba` runs at zoom 1 and LOD 0 and is
unaffected.

### 4.5 One proxy array, and how it should be sized

**Agreed with the critique and with `docs/perf/layer-residency.md`: there is one
proxy array, not two, and this document's fixed quarter-resolution was a constant
where a rule belongs.**

The full chain on the layer array, which §4 opened by costing, is refused on
memory: +33% takes 21.6 GB to 28.8 GB, and wgpu has no partial chains and no
per-slice mip residency (`docs/perf/layer-residency.md` §3.1 checked the second).
So the object is what residency describes: **a second, smaller texture array
holding every layer at a fixed power-of-two reduction `k`, with its own full
chain**, chosen by a per-layer byte budget so that it sizes itself to nothing on
a small canvas rather than being switched off by a flag. That derivation is
better than this document's `k = 2` and it is adopted.

What this document adds to it is that a byte budget **alone** is not a sufficient
rule, because the thing it does not look at is the band.

### 4.5a The unfiltered band, which both documents had left silent

The proxy serves zoom below `1/2^k`. Level 0 of the layer array serves zoom at
or above 1. **Between them is a band, `[1/2^k, 1)`, where the composite reads an
unmipped texture under minification — exactly what it does today.** Neither this
document's first draft nor `docs/perf/layer-residency.md` §2.5 names it.

It matters immediately. Residency derives `k = 3` from a 16 MB per-layer budget
and says the proxy then covers "everything from fit-to-window down". That is true
for the 1600-pixel canvas region it assumes earlier in the same section
(1600/20000 = 0.08) and false for the 3000-pixel one §2.2 of this document
assumes (0.15). **The threshold falls between the two, so on a 4K display the
proxy switches off at exactly the zoom the document opens at**, and the artist
gets today's path plus 450 MB of resident, unused proxy. A threshold derived from
a byte budget has no reason to land where a screen footprint wants it; §2.5's
observation that "the two thresholds meet" is an arithmetic coincidence of one
canvas and one window, not a structural result.

**What actually happens in the band, stated plainly, because there are only two
options and neither is free.** At a zoom inside it the screen can be fed from:

- **the full-resolution array, unfiltered** — which aliases, at a worst-case
  minification of `2^k : 1` at the bottom of the band; or
- **the proxy, magnified** — which does not alias and is blurred, because the
  proxy's level 0 is coarser than the screen.

There is no third answer without a finer proxy. **The default should be the
first**, and the reason is regression rather than theory: aliasing at 40% zoom is
what Umber does today and what every other application does, while a canvas that
went soft at 40% would be reported as a bug on the first day. So the band is a
*bounded* defect rather than an unaddressed hole — every point in it is at least
as good as today, and the worst point in it is `2^k : 1` where today's worst is
the whole minification range.

That turns `k` into a direct, statable trade, and it is the rule that should
replace both the fixed quarter and the bare byte budget:

| `k` | proxy slice | 54 slices, this canvas | worst unfiltered minification |
|---|---|---|---|
| 1 | 1/4 + chain | ~7.2 GB | 2:1 |
| 2 | 1/16 + chain | ~1.8 GB | 4:1 |
| 3 | 1/64 + chain | ~450 MB | 8:1 |
| 4 | 1/256 + chain | ~113 MB | 16:1 |

**So: choose `k` from the byte budget, then state the band it leaves and check it
against the widest fit-to-window this canvas can produce.** Where the budget's
`k` leaves a band the design will not accept, the answer is a smaller `k` and
more bytes — there is no clever third option — and the choice should be made
where somebody can read both columns, which is why the table is here rather than
a constant in a header.

At `k = 3` the band's worst point is 8:1, against today's 12:1 at fit-to-window on
this canvas. That is a real improvement and it is a much smaller one than the
"everything from fit-to-window down" the derivation currently claims, and the
difference is the whole of finding 4.

### 4.5b What the merged object changes in the risk analysis above

Four of §4's hazards move, and it is worth saying which way, because the merge is
not neutral:

- **The sRGB trap is unchanged and applies to the proxy identically.** §4.1.
  Neither the reduction that builds the proxy's level 0 nor its chain escapes it.
- **The `mip_level: 0` audit narrows and improves.** With a separate array, the
  thirty sites in `canvas.rs` naming level 0 go on meaning what they meant,
  because the array they name still has one level. What replaces the audit is
  residency §6's rule — nothing that decides pixels may *bind* the proxy — which
  is a binding rather than a number and is easier to hold. §4.2.
- **The pointer-up jump is unchanged and is still the objection.** §4.3. A proxy
  of the layers with an unreduced stroke scratch has exactly the mismatch a mip
  chain would; residency §2.5's last bullet sees it and files it as "one wrinkle
  to name rather than hide", and it is not a wrinkle — it is the stroke changing
  appearance under the artist's hand, arriving past the "identical blending maths"
  rule because both passes still agree with each other. The scratch needs its own
  reduction and it is part of R7's cost, not a footnote to it.
- **The seam gains a second cause.** The transition at `1/2^k` is between two
  different textures rather than two levels of one, so it will only be invisible
  if the proxy's level 0 is generated as a plain 2x2 box run `k` times from level
  0 of the layer array — which is what makes it agree with the reduction the
  sampler would have produced. Worth the test that composites at
  `1/2^k ± epsilon` and compares. And per §4.0, `MipmapFilterMode::Linear` is
  what stops the *other* seams, at every octave inside the proxy's own chain.

Finally, the independent reason the merge is right: a low-resolution proxy is
what a paging scheme shows while tiles arrive. Two arrays serving that one
purpose would be two things to invalidate at commit, and the commit-time
refresh — one downsample of the damaged rectangle, bounded by `TileMask` — is
work neither document wants to write twice.

---

## 5. R5: the under-cache, from the throughput side

`docs/perf/layer-residency.md` §5 memoises runs of layers for *memory*. This
section is the same idea judged on *speed*, and it reaches a different object: a
screen-space cache rather than a canvas-sized bake. **§5.4 is why, and it settles
the reconciliation the two documents were each asking for** — on an argument
about resampling that neither author made and the review found.

### 5.1 What is exactly cacheable

`acc` after the loop's first `k` iterations is a pure function of draws `0..k`.
The background is added *after* the loop, so it is not in `acc`; the surround and
the checker are screen-space and come later still. So the partial accumulator at
any cut is cacheable with nothing else in it.

**The cut goes below the lowest draw belonging to the active layer, and this
document's first draft got it wrong.** It said "immediately below the active
draw", which is wrong the moment the active layer carries an effect, and wrong
*silently* — which is why the critique filed it as blocking and why it is worth
stating the mechanism rather than just the corrected sentence.

`Effect::rank` (`crates/umber-core/src/effect.rs:402`) puts a drop shadow at 1
and an outside or centred outline at 3, against the layer's own rank of 4. So
those effects composite **below** their own layer, and `CanvasRenderer::baked`
(`canvas.rs:6132-6151`) splices them into the draw list before the layer's own
draw, setting `active_index` to the cursor at the layer, not at the start of its
run. A cut "immediately below the active draw" therefore puts the active layer's
own drop shadow and outline *inside* the cache.

Those are derived from the layer's coverage, and `fs_extract` folds the wet
stroke into that coverage — which is the whole point of it, since it is what
makes a shadow follow the brush. So they change on every frame of a stroke,
which is precisely what the cache exists to hold still. The cache would be
refilled every frame and **R5 would buy nothing at all on any layer carrying an
effect, with nothing on screen to say so.**

The number wanted is the cursor *before* `baked`'s `mine(true)` loop for the
active position — `BakedStack` should carry it beside `active_index`, since that
function is the one place both are known. `docs/perf/layer-residency.md` §5.2's
"the active layer may not be inside a bake" needs the same correction: no *draw
belonging to* the active layer may be.

The one thing that also crosses the cut is `clip_alpha`. If the active draw is
clipped, its bound comes from the nearest unclipped draw below the cut. So the
cache is `vec4` plus one float, and the float has to be stored somewhere — the
alpha channel of a second texel, or a separate `R32Float`, or simply recorded on
the CPU since `clip_alpha` at the cut is a per-*fragment* value and therefore
cannot be. It is per-fragment; store it. A five-channel cache is awkward, so
either a second single-channel target or an `Rgba32Float` plus an `R32Float`.
**Forgetting it silently unclips a clipped active layer**, which looks like the
layer painting everywhere.

### 5.2 Why it can be bit-exact, which is the whole argument

`docs/group-compositing.md` §2.5 refuses a pass-per-group on four counts, and two
of them apply here and are answerable:

- *"It breaks the property that four other things reuse this pass."* It does not,
  if the cache is **screen-space and only ever used by the screen path**.
  `export_rgba`, `pick_colour`, `pick_patch`, `probe_canvas` and the autosave
  capture each build their own camera and their own target; the cache is keyed on
  the screen camera and those five never match it. §2.5 itself says the
  viewport-sized case "is not absurd on its own — a 4K `Rgba16Float`
  intermediate is 66 MB"; it is the document-resolution intermediates that kill
  the general version, and this one has none.
- *"`Rgba8` intermediates lose precision the single pass does not."* True, and the
  answer is to not use `Rgba8`. **Store `acc` as `Rgba32Float` and read it back
  with `textureLoad` at integer coordinates.** `acc` is an f32 `vec4`; writing
  32-bit floats and loading them at the same fragment returns the identical bits.
  The cached path is then **byte-for-byte identical to the uncached one**, and a
  test can assert exactly that rather than a tolerance.

That exactness is worth a lot. `Rgba16Float` would be half the memory and would
be *nearly* right, and "nearly" is a bad word here: a Colour Dodge or Divide
layer sitting directly above the cut divides by `1 - cs` or by `cs`, which
amplifies a backdrop error of 2^-11 by up to a thousand. `Rgba32Float` removes
the argument instead of bounding it.

Cost: 3840 x 2160 x 16 bytes = **133 MB**, plus 33 MB for the `clip_alpha`
channel. Screen-sized, not canvas-sized, and it does not grow with the document.

**`Rgba32Float` is a guaranteed render attachment on `Features::empty()`, and
that has now been read rather than assumed.** `wgpu-types-29.0.4`'s
`texture/format.rs:990` gives it `(s_ro_wo, all_flags)`, and `all_flags` includes
`attachment`. `measure-limits.rs` should still print
`adapter.get_texture_format_features` for it (§8.3) — the spec guarantee is what
the specification promises, not what a driver reports — but the design is not
resting on a mistake.

**The bind group layout has to declare the texture non-filterable, and getting
this wrong is fatal rather than wrong.** `Rgba32Float`'s sample type is
float32-filterable only with `Features::FLOAT32_FILTERABLE`, which Umber does not
request, so the layout entry must say `sample_type: Float { filterable: false }`.
Saying "filtering is not used" is not enough: the *layout* has to agree, and the
composite's existing layout already carries a `Filtering` sampler, which is legal
beside a non-filterable texture only because naga checks the pairing statically
and nothing pairs them. A layout that claims filterable fails device validation
at pipeline creation, which `crash::device_error` correctly makes fatal.

### 5.3 What it buys, and what it does not

Filling the cache costs one pass over `k` draws; using it costs one `textureLoad`
in place of `k` fetches. So it pays whenever the cache survives more than one
frame, which is every frame of a stroke and every idle frame.

On this document with the active layer somewhere in the middle, the loop goes
from 54 fetches to about 28. That is roughly **1.9x** on every figure in §2, at
every zoom, stacking multiplicatively with R1 and R2.

What it does not do:

- It does not help the draws **above** the active layer, and it mostly cannot: a
  layer at Multiply above the cut reads the true backdrop, so a pre-composited
  "over" is only valid for a contiguous run of Normal draws at the very top with
  nothing clipped in it. That is worth having as a second cache only if the
  measurement says the upper half of real stacks is mostly Normal. It is
  strictly an extra, and it should not be built in the same change.
- It does not reduce residency at all. The layer slices are still there. §5.4.

**And the miss path is where it is weakest, which the first draft passed over in
half a sentence.** The cache refills whenever anything below the cut changes,
whenever the active layer changes, and — this is the one that matters —
**whenever the camera moves**. A pan or a zoom is a *continuous* miss: every
frame of the gesture pays the fill pass on top of the ordinary loop and gets
nothing back, so R5 and R6 both cost a little and buy nothing for exactly as long
as somebody is dragging the canvas. That is one of the two interactions where
frame time is most felt, the other being a stroke.

It is survivable — a fill pass is one pass over `k` draws, which is what the
frame was doing anyway plus one write — so the worst case is roughly today's cost
plus a full-screen store. But it is not free, it is not modelled anywhere above,
and it means the cache must never be *invalidated* more eagerly than it is
missed: rebuilding on a frame that would have hit is pure loss. §8's sweep gains a
camera-moving axis for this, and it is the one axis the first draft had no reason
to include and now does.

### 5.4 Why this beats a canvas-sized bake, on an argument this document did not make

`docs/perf/layer-residency.md` §5 proposes memoising the same prefix into a
**canvas-sized** slice, and §5.4 there prices the storage format carefully:
`Rgba8UnormSrgb` quantises, `Rgba16Float` does not meaningfully, so the bake "can
be made quality-neutral by writing it to `Rgba16Float`". The format analysis is
right and **the format is not the only error term**, which the critique found and
neither author had.

A canvas-sized bake is sampled by the composite at `uv = doc / doc_size` through
the `Linear` sampler. So at any zoom but 1 the screen reads a **bilinear resample
of the fold**, where the unbaked path computes a **fold of bilinear resamples**.
Those are the same two quantities §4.4 separates for the proxy, and they differ
for the same reason: `composite_over` is not affine in its layer arguments, so
resampling and folding do not commute. Not even for Normal —
`src + dst(1 - src.a)` is bilinear in the pair and the cross term does not survive
averaging.

Three consequences:

- At zoom 1 (`export_rgba`, `pick_patch`) a canvas-sized bake **is** exact, and
  residency §6's export fold is sound on this point.
- At every screen zoom it carries a resampling error that is **independent of the
  format** and is not reduced by going from eight bits to sixteen. Paying eight
  bytes a pixel to remove an error term that is not the dominant one is the wrong
  trade.
- The error is largest under Colour Dodge, Colour Burn and Divide — exactly where
  §5.4 there already says the format error is largest, which is what made the two
  easy to conflate.

**The screen-space cache has no resampling error at all**, because it is stored
at screen resolution and read with `textureLoad` at the same integer coordinate
it was written at. There is no filter between the write and the read, and §5.2's
bit-exactness is a claim about that and not about the format alone.

So the reconciliation the two documents were each asking for, settled: **build
the screen-space cache; keep the canvas-sized bake designed and unbuilt**, which
is what `docs/perf/layer-residency.md` §5.6 independently concludes on its own
grounds, and record there that a bake is a screen-fidelity trade rather than a
quality-neutral one. The bake wins only on memory, and only in the combination
§5.6 names — a large window at working zoom on a stack near `LayerStack::MAX`.

### 5.5 R5's mechanism and residency's export fold are the same shader change

Neither document noticed this and it removes work from both.

`docs/perf/layer-residency.md` §6 has to export a document whose layers do not
all fit, and its answer is to fold: page a layer in, run `composite` with the
loop starting partway and `acc` seeded from a canvas-sized `Rgba32Float`
accumulator, ping-pong, repeat. R5 needs: run `composite` with the loop starting
partway and `acc` seeded from a screen-sized `Rgba32Float` cache.

**On the export path those are literally the same read.** `render_export` sets
`center` to the document centre, `zoom` to 1 and `pivot` to `center`, so `scale`
is 1 and `offset` is zero and `doc == screen` exactly — which is already what
`saving_and_reopening_does_not_move_a_pixel` leans on. A seed read as
`textureLoad(seed, vec2<i32>(frag.xy))` is therefore the screen-space cache on
the screen path and the canvas-space accumulator on the export path, with no
branch and no second coordinate convention.

Both also need the same **third output mode**: `composite.wgsl` today writes
either straight-alpha sRGB or sRGB over the checkerboard, and neither is the
premultiplied linear `acc` that a seed has to be. So both want one new output
path, one new binding, one `first_draw` uniform, and one `Rgba32Float` target
type. One change, two callers, and the "one statement of the blend maths"
property is preserved for both rather than argued for twice.

Two things to get right if they are built together. The seed binding must be
absent-able, not merely zeroed, so the ordinary screen path and the four reuse
paths are unchanged rather than reading a placeholder; and the loop bound
convention has to be stated once, because residency §6's worked example reads
`layer_count = 1` with `baked_below = 1`, which by its own definition composites
an empty range.

---

## 6. The uniform arrays

`composite.wgsl` holds two `array<vec4<f32>, 191>`. `ViewUniforms` is 112 bytes
of head plus `2 x 191 x 16`, so **6,224 bytes**, against
`downlevel_defaults`' 16 KiB minimum binding size.

**The upload is not a cost.** One `write_buffer` of 6 KB per frame goes through
wgpu's staging belt and is somewhere around a microsecond. It is uploaded in
full even when 54 of 191 entries are live, and trimming it is not worth doing:
the two arrays are separate, so a partial upload would have to write two ranges
or the struct would have to be restructured, and the saving is 4 KB.

**A storage buffer would be worse, not better, at this size.** The index into
`v.layers[i]` is the loop counter, which is wave-uniform, so on hardware with a
scalar/constant cache (all of AMD's, and the constant path on NVIDIA and Intel)
a uniform block is read through it and broadcast, which is the cheapest possible
form of this access. A storage buffer goes through the ordinary vector memory
path. Uniform is right. Leave it.

**R4, the one thing worth changing:** the two arrays are separate, so draw `i`'s
eight floats live 3056 bytes apart, which is two constant-cache lines per
iteration instead of one. Interleaving them into a single
`array<vec4<f32>, 2 * MAX_DRAWS>` with `layers[2i]` and `layers[2i+1]` adjacent
halves the constant-cache footprint of the loop. It costs the readability the
current comment defends and is worth doing anyway, because it is exact, it is
about ten lines, and the `the_three_draw_capacities_agree` test already parses
the shader's array declarations and would catch a mismatch. Do it alongside R2.

Note that raising `MAX_DRAWS` still lengthens no loop — the loop is bounded by
`layer_count` — so none of this changes with the ceiling.

---

## 7. Everything else on the frame path

CLAUDE.md's claim that "nothing on the drawing path allocates per frame" is
recorded about the brush library, and it does not hold generally. None of what
follows is a millisecond on its own; together they are the difference between a
frame path that can be reasoned about and one that cannot.

### 7.1 Device objects created per frame

- **`CanvasRenderer::probe_canvas` creates a `wgpu::Texture` and a view every
  frame** a smudging stroke is live. It is 8 x 8, but a texture creation is a
  driver allocation on the drawing path. `ensure_stroke_color` is the pattern to
  copy: allocate once, keep it, rebuild the bind group when it changes.
- **`drive_thumb` creates a uniform buffer (`create_buffer_init`) and a bind
  group every pass.** One persistent buffer written with `write_buffer` and a
  bind group cached until the layer array is regrown.

Both are ten-line changes and both are exact.

### 7.2 Heap allocations per frame

`Editor::effected_draws` collects a `Vec`; `bake_effects` builds `wanted`,
`keys`, `slots` and `steps`; `BakedStack::draws` collects another; `app.rs`
collects the pending dabs into a `Vec`. At 54 layers these are small, and they
are on the path that runs sixty times a second. `Thumbs::request` is the model —
fixed arrays bounded by `LayerStack::MAX`, about a kilobyte of stack, with a
comment saying the rule is only worth anything if it is kept where it is
inconvenient. The composite's `[[f32; 4]; 191] x 2` on the stack is 6,112 bytes
zeroed per frame for 54 live entries; that one is fine and is named only so
nobody spends time on it.

### 7.3 The thumbnail bounds pass has an occupancy problem

`thumbnail.wgsl`'s first pass reduces a **whole slice** to 64 x 64 in one draw.
That is 4,096 fragments, each looping over its own slab. At 20000 x 5000 a slab
is 312 x 78 texels, and `MAX_TAPS` clamps the stride to 2 in x, so each fragment
takes about 6,000 `textureLoad`s: **25 M loads spread over 4,096 threads**, with
essentially no latency hiding.

The pass's own comment says it "reads every texel of the region exactly once,
which is the same bandwidth the composite pass spends on that layer every frame
anyway" — true about bandwidth and wrong about *time*, for the same reason
`docs/layer-effects.md` had to be corrected about a running-sum blur: the
complexity claim survived the move to a fragment shader and the execution model
did not. It is recorded in the frame's own encoder, so it lands on the frame's
GPU timeline; and it happens twice per layer, one layer per frame, for 54 layers
after a document opens.

Two fixes, and the second is free if R7 happens:

- A multi-pass reduction (or a compute shader with a proper workgroup reduction),
  which is the standard shape.
- **Read a mip.** A bounds pass over level 4 is 1/256 of the loads and the answer
  is very nearly the same. Not exactly the same, which matters: the maximum is a
  maximum precisely so that a one-pixel line is not averaged into nothing, and a
  mip *is* an average. So the bounds pass over a mip has to take the maximum of
  mip **alpha**, which is a mean, and would report a sparse layer as smaller than
  it is. Use it for the picture pass, not the bounds pass, or accept a slightly
  loose frame.

A correctness note found while reading, outside this remit but nowhere else to
put it, and **since confirmed independently**: at a canvas wider than 16,384 the
`MAX_TAPS` clamp makes the bounds pass **step over texels**, so a one-pixel line
at an odd x on a 20000-wide canvas is missed entirely, `peak` stays zero,
`content_rect` reports the layer empty, and the panel draws — and *caches* — the
"nothing on this layer" checker for a layer with something on it. `MAX_TAPS`' own
comment says the clamp is "reached only by a canvas over 16384 wide, which is
past `max_texture_dimension_2d` on the limits Umber requests"; it is not, because
`using_resolution` raises that limit from the adapter and `Document::MAX_EDGE` is
32768 (`crates/umber-core/src/document.rs:171`). That is `using_resolution`
causing the same class of bug a third time. It is a live bug on the exact
document this programme is about, it depends on nothing either design proposes,
and it is being fixed as a commit of its own rather than left as a line here.

### 7.4 The layer list does not cull

`history_row` tests `ui.is_rect_visible(rect)` and returns early, with a comment
explaining that a `format!` per visible row per frame shows up in a frame time.
`layer_row` has no such test. A 54-layer stack tessellates 54 rows — each with a
thumbnail image, several icon marks and a text galley — every frame, when perhaps
ten are on screen. One line, precedented twice in the same file.

### 7.5 The marching ants ask for a frame six times a second, for ever

`ui::selection_outline` calls `request_repaint_after(60 ms)` whenever a selection
or a draft exists. That is deliberate and its comment argues the rate down from
the display's. What the comment does not price is what a frame *costs on this
document*: §2.2 puts a fit-to-view frame at 6.5 GB of texture read, so a
selection sitting on screen with nobody touching anything is **104 GB/s of
sustained memory traffic to slide a dash**.

This is not an argument against the animation. It is an argument that R6 is worth
more than it looks: with a cached output, an ants frame is a blit and the
animation costs what its comment says it costs.

CLAUDE.md still says "The outline is a dashed line, not marching ants. Animating
it means requesting a frame for ever, which is the cost `render`'s `repaint_at`
exists to avoid." That is now the opposite of what the code does. Reported here
rather than edited, per the brief.

### 7.6 Things checked and found fine

`Autosave::next_due` runs every frame and allocates nothing beyond a `retain` and
an `entry` over the tab list. `crash::note_documents` reduces the tab strip to
one `u64` and returns without allocating unless it moved. `InputLog::note` writes
into a fixed-capacity ring. The dab pass draws instanced quads bounded by the
brush's own footprint. `bake_effects` on a document with no effects is a
comparison and a `Vec` and touches no GPU. None of these is worth changing.

---

## 8. What would measure it

Everything above §7 is arithmetic. This section is the experiment, and it is
required rather than optional: `docs/layer-effects.md` §3.2 asserted a blur was
"linear in the area whatever the radius", was wrong by a factor of ten, and was
only found out by `measure-effects.rs`. The same shape of mistake is available in
every figure in §2.

### 8.1 `crates/umber-render/examples/measure-composite.rs`

**What it times.** Wall clock around `queue.submit` plus a blocking `poll`,
median of several runs after a warm-up — exactly `measure-effects.rs`'s method
and for its stated reason: `Features::TIMESTAMP_QUERY` is not among the features
Umber requests, so asking for it would measure a device Umber never creates. It
over-estimates by the submit and the fence, which is the safe direction.

**What it composites.** Real `CanvasRenderer::composite` into an offscreen
`OFFSCREEN_FORMAT` target, driven through the real `CompositeParams`, so the
shader under test is the shipped one. Any prototype variant (§8.2's ALU knob)
lives in the example and **not** in `shaders/`, for the reason
`measure-effects.rs` gives — keeping them out is what stops one being adopted by
accident.

**The sweep.** Each axis is there because a conclusion in this document depends
on it:

| axis | values | settles |
|---|---|---|
| draws | 1, 8, 16, 32, **54**, 64, 128, **191** | is cost linear in draws (§2.1); the ceiling |
| canvas | 2048², 4096², 10000², **20000 x 5000** | whether cost depends on the canvas at all at fixed zoom |
| output | 1920x1080, 2560x1440, **3840x2160** | fragment scaling |
| zoom | fit, 0.25, 0.5, **1.0**, 2.0, 4.0 | the §2.2 knee, and where a proxy should cut in |
| fill | empty, 5% covered, fully covered | how much the `src.a <= 0` early-out is doing |
| masks | 0%, 25%, 100% of draws | the second fetch |
| hidden | 0%, 25%, **50%** of draws | prices R1 directly |
| modes | all Normal, a realistic mix, all Colour Dodge | the `blend_rgb` arms, worst case |
| scissor | full window, canvas region only | prices R2 directly |
| camera | still, panning, zooming | prices R5's and R6's **miss** path, §5.3 |
| effects on active | none, a drop shadow | that R5's cut is below the layer's run, §5.1 |

The last two axes exist because the review found what they cover. A still camera
is the only case R5 and R6 were originally costed against, and it is the
favourable one; a pan is a continuous cache miss and is where the design is
weakest, so a sweep without it would be a measurement agreeing with its own
assumption. And a drop shadow on the active layer is the case where a cut in the
wrong place makes R5 buy exactly nothing, silently — so it belongs in the
performance sweep as well as in the guard, because the symptom is a number that
does not move rather than a picture that is wrong.

**What it prints, per cell.** Milliseconds; taps per frame; **effective bandwidth
in GB/s**, as `taps x 4 / time`, beside the adapter's own reported figure; and
**taps per second** beside a rough TMU rate. Those three ratios are what say
whether a cell is bandwidth bound, fetch bound or neither, and §2's whole
conclusion is a claim about which.

**The one extra knob that separates ALU from fetch**, and it is the most
informative thing in the whole example: a variant of the fragment shader in which
`composite_over` is replaced by a plain add. Same fetches, almost no ALU. The
difference between the two columns is the ALU cost, measured rather than counted.
A second variant that keeps the maths and drops the fetches (reading a 1x1
texture) gives the other half.

### 8.2 The mip experiment

Separate, because it is the only one that asks a quality question as well as a
speed one, and `measure-effects.rs` is the precedent for a measurement that
writes pictures out because a picture settles an argument faster than the
argument does.

- Build the proxy array, generate it through **sRGB views**, and run the
  fit-to-view column reading level 0 of the layer array and reading the proxy at
  the LOD the zoom implies. That is R7's speed number.
- Run the same column with `mipmap_filter` at `Nearest` and at `Linear`. §4.0
  claims x1.0 against x1.25 and the difference is the entire content of this
  document's disagreement with the critique on that point; one column settles it,
  and the popping half is settled by the sweep below rather than by a number.
- Generate a second proxy through the **linear** views and write both out as
  PNGs. The 60-level error of §4.1 should be visible at a glance; if it is not,
  the reasoning is wrong and this document needs correcting.
- Composite the same stack at 0.15 zoom (a) from the proxy and (b) at 1:1 into
  a canvas-sized target and then box-reduced on the CPU — the second being what
  the answer "should" be. Write both out and the difference. That prices §4.4's
  approximation honestly, for Normal and for Multiply separately.
- **Sweep the zoom continuously across an octave and across the proxy
  threshold**, writing a frame every few percent. That is the only thing that
  settles §4.0's popping claim and §4.5b's seam, and it is the captured frame
  pair `docs/perf/layer-residency.md` §9 asks for, generalised to a strip. Do it
  at both `mipmap_filter` settings.
- **Sweep the band.** Composite at 0.9, 0.5, 0.3 and just above `1/2^k`, reading
  level 0 unfiltered, and write the frames out. §4.5a argues the band is a
  bounded defect that is never worse than today; that is a claim about what it
  looks like, and it should be looked at before `k` is chosen from a table.
- Memory: print the layer array's bytes and the proxy's at `k = 1, 2, 3, 4` for
  each canvas size, which is §4.5a's table produced by a run rather than by hand.
  That is the number `docs/perf/layer-residency.md` needs to reconcile against.

### 8.3 Extend `measure-limits.rs`

It already exists and already reports the device's dimension limits. Add
`adapter.get_texture_format_features` for `Rgba32Float` and `Rgba16Float` — R5
is designed around `Rgba32Float` being a legal render attachment on
`Features::empty()`, and that should be settled by a run on several machines
rather than by a spec table.

### 8.4 The guards, which are a different thing from the measurements

In `umber-render/tests/gpu_pipeline.rs`, and none of them asserts a time:

- **R1 and R2 are byte-equality tests.** Composite a stack with hidden layers,
  hidden folders and a clipped layer directly above a hidden one, elided and not;
  compare every byte. Then the same with and without the canvas-region scissor,
  comparing only the region inside it, and separately asserting the surround
  outside it is the backdrop.
- **R5 is a byte-equality test too**, and it is the reason to pay for
  `Rgba32Float`: seed the cache at every cut from 0 to `N`, and compare against
  the uncached composite. Include a Colour Dodge and a Divide directly above the
  cut, which is where a narrower format would fail.
- **R5 needs a second guard that is not about exactness**, because its blocking
  defect was a cache that was *correct* and never hit. Put a drop shadow on the
  active layer, run a stroke, and assert the fill pass ran **once**. There is
  already a counter of exactly this shape — `CanvasRenderer::effect_bakes` exists
  because "that frame rebaked nothing" is otherwise invisible, and its docs say
  so — so the cache should carry a `fills` counter for the same reason and the
  test should read it. A byte-equality test passes happily on a cache that
  refills every frame.
- **R6's guard is the damage rect.** Drive a stroke frame by frame with a
  scattering, angle-jittering brush; after each frame compare the dirty-region
  result against a full recomposite. An under-tight rect fails on the frame it is
  under-tight, which is the failure that has shipped four times in the cumulative
  version of this rule.
- **R7 cannot be a byte-equality test**, so its guard is the other half: with the
  proxy unbound the composite must still be byte-identical to today, and the five
  reuse paths must be shown never to bind it — by a test that fills the proxy with
  solid magenta and asserts `export_rgba`, `pick_colour`, `pick_patch`,
  `probe_canvas` and the autosave capture never see it. That is the only way to
  catch a caller that inherits a binding it did not set, and it is worth writing
  before the proxy is.
- **The proxy's generator gets a guard of its own, and it is a CPU one.**
  Reduce a two-texel image of linear 0 and linear 1 and assert the result is
  encoded 188 and not 128. That is §4.1's whole claim in one assertion, it needs
  no stack and no camera, and it is the test that fails the day somebody reaches
  for `raw_slot_views`.

**No wall-clock assertion on CI**, per CLAUDE.md, and `UMBER_TEST_SOFTWARE=1`
over the whole of `umber-render` before any of this is believed — hardware and
lavapipe do not round identically and a byte-equality test is exactly the shape
that finds out the hard way.

### 8.5 Where the numbers go

**§11 now holds them**, and this paragraph's instruction was not followed
exactly: they went into a section of their own rather than back into §1 and §2,
because the sweep that has been run answers a different question from the one §2
asks. §2 prices the composite in taps and bandwidth; §11 prices the *atlas
against the dense slice it replaced*, which is the question the tile atlas
landing made urgent and which §2 predates. Overwriting §2's arithmetic with it
would have replaced an unverified claim with an answer to something else.

The rest stands. Re-run before quoting any of them, on a machine that is not
building six other things — which is `measure-clipboard.rs`'s recorded lesson,
why the first figures written into these docs were three times too slow, and
what the first pass of this very sweep reproduced.

---

## 9. The quality contract

Everything in this document is held to one rule: **stroke quality, layer
fidelity and the rasterised image stay pristine.** Restated as what each
recommendation may and may not do:

- **R1, R2, R3, R4, R5, R6 must be byte-exact**, and each has a test in §8.4 that
  says so. A change in this set that cannot be shown byte-exact should be
  abandoned rather than argued down to a tolerance.
- **R7 changes the screen and only the screen.** It changes nothing that reaches
  a file, a readback, an undo patch, a smudge pickup or an eyedropper, and §4.2
  is the enumeration that makes that true. If any of those can reach the proxy,
  R7 is not ready. It is also the one item whose honest description is "better,
  not exact" (§4.4), and neither a comment nor anything on screen may round that
  up.
- **`mipmap_filter` moving from `Nearest` to `Linear` (§4.0) is a change to a
  shared sampler**, and it is inert only for as long as every texture bound
  through it has one level. The tip mask and the paper grain are the two that
  might plausibly gain a chain later. Whoever changes it owes the comment saying
  which textures the guarantee covers, so the day one of them gains a chain the
  reasoning is there to be re-checked rather than re-derived.
- **R8 changes a pixel in the last bit and reaches an export.** Ranked last, and
  it should not be built on reasoning alone.
- `composite.wgsl` and `commit.wgsl` must go on implementing identical blending
  maths, and none of this touches `blend.wgsl` or `composite_over` — R8 is the
  only item that goes near the arithmetic, and it would be a hand-written case in
  the *layer* loop, which `commit.wgsl` does not have and does not need.
- **The pointer-up jump is a rule about more than the blend maths.** §4.3 is the
  case that shows it: two shaders can implement identical arithmetic and the
  stroke can still jump, because the preview and the commit sample the layer
  differently. `commit.wgsl` already says so in its own comment about
  magnification, and mips take that from sub-visual to obvious. Anything that
  changes how the *screen* samples a layer has to be checked against what
  pointer-up will look like, not only against what the maths says.

---

## 10. Not settled

- **Whether §2's arithmetic is right at all.** It is the load-bearing claim and
  it has not been run. §8 exists because of that. Its zoomed-out half now has
  independent corroboration from `docs/perf/layer-residency.md` (§2.2), which
  raises confidence and is not a measurement.
- **How much of a real 54-layer Clip Studio document is hidden or at zero
  opacity.** This is the one figure where a 2x error changes the *plan* rather
  than a number — R1 first at a third, R7 first at a tenth — and both performance
  documents rest a headline on it. `survey-documents.rs` already walks real files;
  it should report hidden-layer **count and bytes** per document, and be run
  before either staging plan is committed to.
- **Whether `Rgba32Float` is a render attachment on the machines Umber ships
  to.** The spec guarantee is now verified in `wgpu-types` (§5.2); the adapters
  are not. §8.3.
- **Whether the top of a real stack is mostly Normal**, which decides whether an
  "over" cache is worth building beside the under-cache of §5.
- **Whether R1, R2 and R5 together put fit-to-view inside a frame without the
  proxy.** If they do, R7 stops being urgent and becomes a quality feature (the
  aliasing) rather than a performance one, which changes how it should be judged.
- **What `k` should be, and therefore how wide the unfiltered band is.** §4.5a
  turns it into a two-column trade and does not decide it, because deciding it
  needs the pictures §8.2 asks for. It also needs the widest fit-to-window Umber
  will meet, which is a question about displays rather than about code.
- **Whether the proxy pops, and whether the seam at `1/2^k` is visible.** §4.0
  argues `MipmapFilterMode::Linear` is required and §4.5b argues the seam is
  avoidable by construction; both are reasoning, and nothing short of the zoom
  strip of §8.2 settles either.
- **Whether the smudge probe should keep undersampling.** §4.2: at 0.133 it takes
  a four-tap bilinear of a 60-pixel footprint to decide a pickup colour. An area
  average is arguably what a smudge should get. It is a real open question about
  *document* pixels and it must not be answered as a side effect of the proxy
  landing, because changing it changes every existing document's behaviour.
- **The reconciliation with `docs/perf/layer-residency.md`** is now settled in
  both places it was open, and recorded here so that a later reader does not
  reopen it as though it were still live:
  - **The proxy is one object**, sized by residency's per-layer byte budget
    rather than this document's fixed quarter, with §4.5a's band stated as part
    of choosing `k`. Agreed.
  - **The screen-space cache is preferred to the canvas-sized bake**, on §5.4's
    resampling argument rather than on the format argument either document
    started from. The bake stays designed and unbuilt, which is what
    `docs/perf/layer-residency.md` §5.6 already concludes on its own grounds.
  - What is *not* settled is whether either is needed once R1, R2 and the proxy
    are measured. Both documents say last, and this one agrees.
- **`docs/group-compositing.md` will make all of this worse**, and by how much is
  unknown. A group stack in the fragment shader roughly doubles register
  pressure, and §2.4 of that document is honest that the scratch-memory risk is
  not knowable by reading. The `measure-composite.rs` of §8 is the harness that
  would price it, and it should be written in a shape that can take a second
  pipeline as another column.

---

## 11. What the measurement said

§8 was written before `measure-composite.rs` existed. It exists now, and this
section is its output. **Everything in §1 and §2 above is still arithmetic** —
this section does not rewrite it, because the two ask different questions: §2
counts taps and bandwidth, and what follows is wall clock on one machine. Where
they disagree, the run wins.

Re-run before quoting any of it. `measure-clipboard.rs`'s recorded lesson is
that the first figures written into these documents were three times too slow
because the machine was building six other things at the time, and the first
pass of this sweep reproduced exactly that: spreads of ±100% to ±289% on cells
that read ±2% on a quiet machine.

```sh
cargo run --release -p umber-render --example measure-composite -- \
    --sizes 1920x1080,2048x2048,4096x4096 --layers 1,8,16,32,54 \
    --zooms fit,0.25,1 --budget 8 --repeat 2
```

**The machine.** RTX 3080, Vulkan, output 1920x1080, 32 passes per submit,
median of 25 samples after 5. The noise floor — an empty submit and fence — is
0.080 ms, which over 32 passes is 0.0025 ms per pass, so every figure below is
two to three orders of magnitude above what the instrument can resolve. Two
rounds; the figures quoted are round 2, and round 1 agrees to within 0.03 ms
wherever its own spread was under 10%.

### 11.1 The answer depends on zoom, and the crossover is about 0.75

Ratios are **tiled ÷ sampled**, so below 1.0 the atlas is faster. 54 layers,
which is the artist's document. "Realistic" is 13.5% of tiles backed, the
corpus figure, in a genuinely packed atlas.

| canvas | zoom | dense | realistic (blob) | realistic (scatter) |
|---|---|---|---|---|
| 1920x1080 | 1.0 | **1.98x** | 1.25x | 1.21x |
| 1920x1080 | 0.25 | 1.04x | **0.30x** | 0.29x |
| 2048² | 1.0 | 2.11x | 1.29x | 1.27x |
| 2048² | fit (0.527) | 1.06x | 0.53x | 0.53x |
| 2048² | 0.25 | 1.02x | 0.25x | 0.22x |
| 4096² | 1.0 | 1.98x | 1.36x | 1.23x |
| 4096² | fit (0.264) | 1.03x | **0.24x** | 0.24x |
| 4096² | 0.25 | 1.02x | 0.19x | 0.17x |

Swept continuously at 2048² and 54 layers, the realistic ratio is 0.25x at
zoom 0.125, 0.37x at 0.25, 0.60x at 0.5, **0.96x at 0.75**, 1.29x at 1.0, and
flat above that. So **the atlas is a loss at working zoom and a large win
zoomed out, and the two swap at about 0.75.**

Note the one confound, so nobody re-derives it: at a 1920x1080 canvas in a
1920x1080 view "fit" *is* 1.0, so that row's fit and 1.0 entries are the same
measurement and neither contains any minification.

### 11.2 In milliseconds, which is what a frame is spent in

54 layers, 1920x1080 output. A 60 Hz frame is 16.7 ms and a 144 Hz frame 6.9 ms.

| case | sampled | tiled | the atlas costs |
|---|---|---|---|
| 1920x1080 at 1:1, dense | 1.26 ms | 2.49 ms | **+1.23 ms** (7.4% of 60 Hz) |
| 1920x1080 at 1:1, realistic | 1.27 ms | 1.60 ms | +0.33 ms (2.0%) |
| 4096² at fit, dense | 4.54 ms | 4.65 ms | +0.11 ms |
| 4096² at fit, realistic | 4.54 ms | 1.08 ms | **−3.46 ms** |

**A mask on every layer doubles the taps and moves the loss the wrong way**: at
1:1 the dense ratio goes to 2.16x (1920x1080) and 2.38x (4096²), the realistic
to 1.34–1.38x. At 4096² fit it goes the other way, to 0.16x, because the mask's
tiles are unbacked too.

The ratio also **grows with layer count**, because the per-fragment work the
loop does not repeat — the checkerboard, the sRGB encode, the backdrop — dilutes
it: at 1920x1080 and 1:1 it is 1.23x at one layer, 1.33x at 8, 1.77x at 16,
1.88x at 32 and 1.98x at 54.

### 11.3 It is not the page table. It is the four loads.

This is the finding that should decide what happens next, and the `table`
variant is what isolates it: it does the page-table read and the unbacked branch
and returns without touching the atlas. At 1920x1080, 1:1, 54 layers, dense:

| | ms | what it adds |
|---|---|---|
| `table` — loop, ALU, page-table read | 1.18 | — |
| `sampled` — the above plus one hardware bilinear tap | 1.26 | **+0.08 ms** |
| `tiled` — the above plus four `textureLoad`s and the lerp | 2.49 | **+1.31 ms** |

**The dependent page-table read is nearly free** — a slot's table slice is
`tiles.x × tiles.y × 4` bytes, 6.4 KB for this canvas, so the whole table for 54
slots is cache-resident and the latency the design worried about never
materialises. What costs is the hand-reconstructed tap: four scalar
`textureLoad`s and a lerp against one TMU instruction, and the ratio between
them is about **16x**.

That is indicative rather than exact — `table` returns a tile-uniform value and
so is not a perfect "everything but the fetch" control — but the gap is an order
of magnitude and no plausible correction closes it.

**So `tiles.wgsl`'s refusal of the apron is what this costs**, and that refusal
is now priced rather than argued. The apron was rejected because a *stale* one
is "the real risk in the whole design" — a one-texel seam appearing only at some
zooms on some layers because one writer forgot to refresh it — and because
dropping it makes a tile's pitch equal its size, which is what lets a page be the
canvas rounded up and never larger than a limit the canvas was already inside.
Both arguments stand. What has changed is that the bill is a number: **+1.31 ms
per frame at 54 layers on a fully painted 1920x1080 document, and +0.33 ms on a
realistic one.**

`textureGather` is the obvious cheaper middle and is **unmeasured**: it fetches
the four texels of a bilinear footprint in one instruction per channel, and
inside `tile_bilinear`'s existing single-tile fast path — 99.2% of samples — the
addressing could be clamped by hand first, so no apron is needed. Whether four
gathers beat sixteen scalar loads on this hardware is a question for the next run
of this example, not a recommendation.

### 11.4 Why the zoomed-out win is a residency win, not a tiling win

At 4096² and fit the *dense* tiled column is 4.65 ms against the sampled
4.54 ms — parity. The realistic one is 1.08 ms. So nothing about tiling is making
the zoomed-out case fast; what makes it fast is that 86% of tiles are unbacked
and issue **no fetch at all**, where a dense slice is sampled whatever it holds.
The composite loop has no alpha early-out — that is R1's territory, §3.1 — so the
sampled path cannot have this win at any residency.

Two consequences. A dense store could never have been made fast here by elision
at the *layer* level, because these layers are all visible and all contribute.
And the win is proportional to sparsity, which `survey-residency` measures as
**6.4% to 25% for every document over 1 GB dense** — so it is the large
documents, the ones that motivated the whole programme, that get it.

### 11.5 What this says about Stage 6

`docs/perf/roadmap.md` Stage 6 parks R7 (the proxy), R5 (the screen cache), R6
(dirty regions) and `layer-residency.md` behind this measurement.

- **R7, the proxy array: do not build it.** It exists for the zoomed-out case,
  and the zoomed-out case is now the one the atlas is 4–6x *faster* in on a
  realistic document and at parity in on a dense one. §10 already listed "whether
  R1, R2 and R5 together put fit-to-view inside a frame without the proxy" as the
  condition that would retire it; residency answered it instead. It is also the
  only item in the programme that can make the picture worse — the unfiltered
  band of §4.5a, the pop of §4.0, the seam of §4.5b — so retiring it on evidence
  is the best available outcome. **The revival condition is narrow and should be
  written down**: a large, *densely* painted document that is slow at fit. 4096²
  fully painted at 54 layers is 4.65 ms at fit, which is real; the corpus says
  documents that large are 6.4–25% covered, so nobody has one.
- **R6, dirty-region compositing: now the best-value item in the programme, and
  the atlas raised its value rather than lowering it.** Everything R6 helps is at
  working zoom, which is exactly where the atlas costs 1.25x to 2.1x. It also has
  a live consumer this document did not have when it was written: the selection
  marquee now animates at `ANT_SPEED`'s sixteen frames a second for as long as
  anything is selected, so a 54-layer document with a selection standing spends
  16 × 1.6 ms every second recompositing nothing that changed.
- **R5, the screen-space cache: keep it designed, below R6.** Same argument — it
  hits at a still camera at working zoom, where the composite just got about
  twice as expensive — but its miss path is a pan, which is the gesture a painter
  spends most of their navigation in, and R6 covers the still-camera case too
  without that failure mode.
- **`layer-residency.md`'s remaining stages**: the composite side of it is what
  has just been measured and it is doing its job. Nothing here argues for or
  against the host-memory half.

The ranking that comes out of this is therefore **R6, then the apron or
`textureGather` question, then R5, and not R7.** The middle one is new and is not
in the roadmap at all, which is what a measurement is for.
