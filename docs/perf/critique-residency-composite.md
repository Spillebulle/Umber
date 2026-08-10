# Critique: layer residency and composite throughput

A review of `docs/perf/layer-residency.md` and `docs/perf/composite-throughput.md`,
against the code rather than against each other's prose. Every claim marked
"verified" below was read out of a file named at the finding.

Both documents are good. They are honest about what they have not measured, they
name their own weakest arguments, and each identified the collision with the
other rather than leaving it for a reader. Most of what follows is either a
reconciliation the two could not perform separately, or a claim that survives
reading the code less well than it reads on the page.

The findings are ordered by weight, not by document.

---

## Verdict on the headline disagreement

**Both authors are right about what they are each looking at, and the residency
author is wrong about one word.**

The word is "correct". `docs/perf/layer-residency.md` §2.5 says a proxy pyramid
"is not a quality compromise at all", on the ground that the present path is
already undersampled, so a prefiltered read is "the *correct* filter arriving".
The first half is verified and the second half does not follow.

What is verified:

- `LayerStore::new` creates the array with `mip_level_count: 1`
  (`crates/umber-render/src/canvas.rs:2073`).
- `Shared::new`'s sampler is `min_filter: Linear`, `mag_filter: Linear`
  (`canvas.rs:2345`).
- `composite.wgsl:193` samples with `textureSampleLevel(..., 0.0)`.

So at fit-to-window the composite is taking a four-tap bilinear of a footprint
tens of texels across, per layer. That is undersampling, it aliases, and the
residency author is right that today's zoomed-out picture is not a faithful
reduction of the document. `docs/perf/composite-throughput.md` §4 agrees with
this in its own words ("It also removes the aliasing, which today makes a
zoomed-out view of a detailed document shimmer"), so this half is not in
dispute at all.

What does not follow is that a per-layer pyramid is *the* correct filter. The
correct reduction of the picture is the area average of the **composite**. What a
per-layer proxy computes is the composite of the **area averages**. Those differ
whenever the fold is not affine in its layer arguments, which is every mode
except approximately Normal — and `composite_over` is not affine even for
Normal, because `dst + src(1 - src.a)` is bilinear and the cross term does not
survive averaging. `docs/perf/composite-throughput.md` §4.4 states this exactly
right and states its consequence exactly right: mips make the zoomed-out view
*more* faithful, not equal. That paragraph is the correct statement of the
position and `docs/perf/layer-residency.md` should adopt it verbatim rather than
claiming exactness it does not have.

So the two positions reconcile as:

| | today | per-layer proxy |
|---|---|---|
| faithful to an export at that scale? | no | no |
| how unfaithful? | a point sample of the area | an average of per-layer averages |
| aliases under camera motion? | yes | no |

The proxy is a large improvement and it is not a free one, and the honest
sentence is "better, not exact". Calling it exact is the thing that would later
be quoted at somebody arguing about a colour shift on a Multiply layer.

**On the remaining four points of the disagreement, the composite author is
right on three and the residency author on one.**

1. **The sRGB view trap is real and the residency document does not mention it
   at all.** Verified: `LAYER_FORMAT` is `Rgba8UnormSrgb` (`canvas.rs:22`),
   `LayerStore::raw_slot_views` is a per-slice `Rgba8Unorm` view built for the
   flip pass (`canvas.rs:2106-2117`), and `view_formats` already declares
   `LAYER_FORMAT_LINEAR` so a linear view of the array is legal and will simply
   work. A generator reaching for the views that already exist averages encoded
   bytes. `docs/perf/composite-throughput.md` §4.1 is correct, the hazard is
   sitting in the same file, and the residency document proposing a *separate*
   proxy array does not escape it — the proxy will hold the same format and its
   downsample has the identical trap. This must be in whichever document
   survives.
2. **The `mip_level: 0` audit is real.** Verified: roughly thirty sites in
   `canvas.rs` name `mip_level: 0`, and they are correct today only because
   there is one level. §4.2's demand that the LOD be a `CompositeParams` field
   with no default is correct and is the same argument `Background` already won.
3. **The `probe_canvas` case is sharper than either document says.** Verified:
   `canvas.rs:5260` sets `zoom: PROBE_SIZE as f32 / (radius * 2.0).max(0.5)`
   with `PROBE_SIZE = 8`, so a 30-pixel-radius smudging brush probes at zoom
   0.133. A LOD derived from the camera inside `composite()` would have a
   smudge picking colour out of a mip, and that reaches document pixels.
   Endorsed without qualification.
4. **The pointer-up jump: the residency author did see it.** §2.5's last bullet
   names the wet scratch as needing "its own reduction". It under-rates it —
   it is filed as "one wrinkle to name rather than hide" where
   `docs/perf/composite-throughput.md` §4.3 correctly makes it the objection —
   but the claim that residency missed it is not fair. What residency does miss
   is *why* it is worse than an aliasing mismatch: it is the stroke changing
   appearance at pointer-up, arriving past the "identical blending maths" rule
   because both passes still agree with each other. §4.3 and §9's last bullet
   are the best two paragraphs in either document and should be preserved
   whole.

**On the object: they are not proposing the same thing, and they should be.**
`docs/perf/composite-throughput.md` §4 proposes a full chain on the layer array
(+33%, unaffordable, and §4.5 concedes it); §4.5 then proposes a
quarter-resolution proxy array with its own chain and flags it as probably the
same object residency wants. It is. Build **one** proxy array, sized by
residency's rule (a per-layer byte budget, so it switches itself off on a small
canvas) rather than by the composite document's fixed quarter — residency's
derivation is the better one and §4.5's `k = 2` is a constant where a rule
belongs. But see finding 4: residency's own threshold is wrong.

---

## Findings

### 1. BLOCKING — "a hidden layer's pixels are never read by anything" is false, and Stage 1 is not bookkeeping

`docs/perf/layer-residency.md` §0.1 and §2.2. The **proof** is correct; the
**scope claimed for it** is not, and the difference is the whole cost of
Stage 1.

I walked `composite.wgsl`'s loop body (lines 177–268) exhaustively with
`visible == false`:

- Cross-iteration state is exactly `acc`, `clip_alpha` and the loop counter
  (via `stroke_here = i == v.active_index`). Nothing else survives an iteration.
- `lay` (line 193) and `m` (line 213) are locals.
- Clipped: `lay = lay * clip_alpha` (257) then `continue` (263). Both dead.
- Unclipped: `clip_alpha = select(0.0, lay.a, visible && opacity > 0.0)` (259).
  WGSL `select(f, t, cond)` returns `f` when `cond` is false; it is a value
  selection with no arithmetic, so `lay.a` cannot influence it even if it were
  NaN — which it cannot be out of an `Rgba8UnormSrgb` fetch anyway. Then
  `continue`.
- The mask sample at 213 is only ever multiplied into `lay`, so it dies with it.
- A hidden **folder** containing visible layers: verified,
  `LayerStack::effective_visible` (`crates/umber-core/src/layer.rs:1125`) ANDs
  the entry's own flag with every ancestor's, and `Editor::effected_draws`
  (`crates/umber-app/src/editor.rs:1939`) writes that into `LayerDraw::visible`.
  So every layer inside a hidden folder arrives with `visible: false` and the
  proof covers all of them.
- The stroke: if the active layer is itself hidden, lines 221–252 modify `lay`
  and `m` and the iteration still `continue`s at 263. Dead.

**The proof holds, completely, for the composite pass.** It is the strongest
piece of reasoning in either document and I could not break it.

What it does not cover is everything else that reads a slice, and three of those
read hidden layers today:

- **Thumbnails.** `crates/umber-app/src/thumbs.rs:71-81` builds its slot list
  from `layers().iter().filter_map(|l| l.slot())` with **no visibility test**,
  and `wanted` returns any slot whose revision has moved. So the layers panel
  draws a real thumbnail for a hidden layer, and it does so every time one goes
  stale. Give a hidden layer no slice and every hidden row's thumbnail goes
  blank — a visible regression on the exact document this is meant to rescue,
  which a painter will file as a bug.
- **Save.** `crates/umber-app/src/app.rs:3063-3070` reads every entry with a
  slot through `read_layer_rect`, hidden included, because a hidden layer's
  pixels must go into the `.ora`. Same for its mask at 3074-3079.
- **Canvas flip.** `app.rs:785-792` collects `[l.slot(), l.mask()]` for every
  entry with no visibility test and hands the lot to `flip_layers`. A hidden
  layer that flipped only on the GPU would come back unflipped when unhidden.

Add to that: unhiding the layer, `resize`, and undo replaying a `PixelPatch`
into a parked slot.

So a hidden layer needs a **complete, current CPU shadow**, and a route from
that shadow to a thumbnail, to a save, and through a flip. That is Stage 2's
machinery, not "nothing but bookkeeping". The 21.6 GB does not vanish; on the
first implementation it moves from a 10 GB card to a 32 GB machine, which is a
real and worthwhile trade and is **not the trade §0 describes**.

Concretely, §0 bullet 1 and §8's Stage 1 should be rewritten to say: a hidden
layer's *VRAM* is free, its RAM is not, and Stage 1 therefore contains the
shadow, the shadow-backed thumbnail and the shadow-backed save — or it ships a
document that cannot be saved correctly. There is no smaller version of this
that is correct.

The one genuinely free part survives: `bake_effects` really does build `wanted`
without consulting visibility (`canvas.rs:5920-5932`), so filtering it by
`draw.visible` is one predicate and is a pure win. Verified and endorsed.

### 2. BLOCKING — a canvas-sized bake is not exact at any zoom but 1, and §5.4 prices the wrong thing

`docs/perf/layer-residency.md` §5.1 and §5.4.

The legality argument is **correct and I want to endorse it explicitly**: the
loop is a fold, `acc` is the only value a blend mode reads about its backdrop,
and a bake is a memoised prefix of that fold. That is a genuinely stronger and
more useful statement than the usual associativity hand-wave, and §5.2's
boundary rule ("the first unbaked draw must be unclipped") is exactly the right
repair for the one other piece of cross-iteration state. I verified both against
the loop and they are right.

But §5.4 then asks only "what does the storage format cost?" and answers it
carefully (`Rgba16Float` verified as a render attachment on `Features::empty()`
— `wgpu-types-29.0.4/src/texture/format.rs:987`, `(msaa_resolve | s_ro_wo,
all_flags)`, and `all_flags` includes `attachment`). The format is not the only
error term.

A bake is **canvas-sized**. The composite samples it at `uv = doc / doc_size`
through the `Linear` sampler, so at any zoom but 1 the screen reads a *bilinear
resample of the fold* where the unbaked path computes a *fold of bilinear
resamples*. Those are the same two quantities finding "verdict" separates for
mips, and they differ for the same reason. So:

- At zoom 1 (`export_rgba`, `pick_patch`) a bake is exact, and §6's fold is
  therefore sound on this point.
- At every screen zoom a bake carries a resampling error that is **independent
  of the format** and is not reduced by going from `Rgba8UnormSrgb` to
  `Rgba16Float`. Paying 8 bytes a pixel to remove an error term that is not the
  dominant one is the wrong trade.
- The error is largest exactly where §5.4's format analysis already says the
  format error is largest: under Colour Dodge, Colour Burn and Divide.

This is not fatal to the bake — it is the same order of approximation as the
proxy, and a document being previewed through a proxy is already accepting it.
It *is* fatal to the sentence "it can be made quality-neutral by writing it to
`Rgba16Float`". Rewrite §5.4 to price both terms, and note that the two
mechanisms then have the same character: both are screen-fidelity trades, both
are exact at zoom 1, and neither may be read by anything that decides pixels —
which §6 already says.

**And this is the argument that settles §5 against §5 of the other document.**
`docs/perf/composite-throughput.md`'s screen-space cache has **no resampling
error at all**: it is stored at screen resolution and read with `textureLoad` at
the same integer coordinate it was written at. Where both mechanisms are
available, the screen-space one is strictly more exact. See finding 9.

### 3. BLOCKING — the R5 cut is in the wrong place when the active layer has effects

`docs/perf/composite-throughput.md` §5.1 puts the cut "immediately below the
active draw". Verified that this is not enough: `Effect::rank`
(`crates/umber-core/src/effect.rs:402-410`) gives a drop shadow rank 1 and an
outside or centred outline rank 3, against the layer's own 4 — so a layer's own
effects produce draws **below** its draw in the list, and `effects_below` is the
function that separates them.

A cut immediately below the active *draw* therefore puts the active layer's own
drop shadow and outline **inside** the cache. Those are derived from the layer's
coverage, and the effect extract takes the wet stroke into account, so they
change on every frame of a stroke — which is precisely the thing the cache
exists to hold still. The cache would be refilled every frame and R5 would buy
nothing on any layer carrying an effect, silently.

The cut must be below the **lowest draw belonging to the active layer**, which
is what `bake_effects` already knows and what `BakedStack::active_index` is
positioned against. One sentence in §5.1 and one in §8.4's guard: include a
layer with a drop shadow in the cut-position sweep.

The same correction applies to `docs/perf/layer-residency.md` §5.2's "the active
layer may not be inside a bake" — it should read "no draw belonging to the
active layer may be inside a bake".

### 4. BLOCKING — the proxy threshold does not engage at fit-to-window on a 4K display

`docs/perf/layer-residency.md` §2.5 derives `k = 3` from a 16 MB per-layer
budget and then says the proxy serves "zoom ≤ 12.5%, which on a 20000-wide
canvas is everything from fit-to-window down".

That is true for the 1600-pixel-wide canvas region §2.5 assumes earlier in the
same section (1600/20000 = 0.08) and false for the one
`docs/perf/composite-throughput.md` §2.2 assumes (3000/20000 = 0.15). The two
documents' fit-to-window zooms differ by nearly a factor of two purely because
they assume different windows, and **residency's threshold falls between them**.

So on a 4K display — the machine the whole programme is aimed at — the proxy
switches off at exactly the zoom the document opens at, and the artist gets the
present 6 GB/frame aliasing path with an extra 450 MB of proxy resident and
unused. The mechanism that "should switch itself off rather than be switched off
by a flag" switches itself off in the wrong place.

More generally, the claim in §2.5 that "the two thresholds meet" — the proxy's
and tiling's — is a coincidence of one arithmetic and not a structural result. A
threshold derived from a *byte budget* has no reason to land where a *screen
footprint* wants it.

The fix is to stop deriving the threshold from `k` and derive it from the
footprint: use the proxy whenever the required LOD on the full-resolution array
is at least `k`, and size `k` so that band starts at or above the widest
fit-to-window this canvas can produce. Equivalently, choose `k` from the byte
budget *and* check it against `max(view_width) / doc_width`, and if the budget's
`k` leaves a gap, either lower `k` (a bigger proxy) or accept that the band from
`1/2^k` to 1 is unfiltered and say so out loud. Both documents currently leave
that band unnamed: `docs/perf/composite-throughput.md` §4.5 has the identical
hole between 0.25 and 1.0 and does not mention it either. A 20000-wide canvas at
40% zoom is a 2.5:1 minification of an unmipped texture and neither design
touches it.

### 5. SUBSTANTIVE — `mipmap_filter` is `Nearest`, so there is no trilinear and the LOD will pop

Verified: `canvas.rs:2347`, `mipmap_filter: wgpu::MipmapFilterMode::Nearest` on
the shared sampler.

Two consequences neither document has:

- `docs/perf/composite-throughput.md` §4 costs the mipped read at "×~1.5 for
  trilinear". With this sampler there is no trilinear; the figure is ×1.0 and
  R7's bandwidth win is slightly *better* than stated.
- The residency document's honestly-flagged risk — "crossing the threshold is a
  discrete switch and can pop" — is understated. With `Nearest` mip filtering
  the LOD snaps at every octave, not only at the proxy boundary, so a continuous
  zoom gesture pops at every power of two. This is a much more visible artefact
  than one seam, and it is a one-line change (`MipmapFilterMode::Linear`) that
  must be made in the same commit as the first mip chain.

Note the sampler is shared by the tip, the paper and the thumbnail passes, so
either the change is made globally and its effect on those checked, or the
proxy gets a sampler of its own. Say which.

### 6. SUBSTANTIVE — the residency document's 12.8 GB/frame overstates by ~2.3×, and it does not change the recommendation

`docs/perf/layer-residency.md` §2.5: "The composite runs at *window* resolution
… At 2560 × 1440 that is 3.7 Mpx. Fifty-four draws, one fetch each … 3.7M × 54 ×
64 B ≈ 12.8 GB".

Verified wrong: `composite.wgsl:155-160` returns before the loop for any
fragment outside the document's uv range. At fit-to-window a 20000 × 5000
document in a 2560-wide region occupies 2560 × 640 = 1.64 Mpx, not 3.7. The
correct figure on the same cache-line assumption is **5.7 GB**.

This matters less than it looks, and saying so is part of the answer to the
brief's question about which numbers would change the recommendation if wrong by
2×. Corrected, residency's 5.7 GB and `docs/perf/composite-throughput.md` §2.2's
independently-constructed 6.5 GB **agree**, which is a stronger position than
either had alone. The proxy is still the answer and the ranking does not move.

What does move is the headline in §0: "12.8 GB/frame to 1.6" becomes "about 6 GB
to well under 1", which is still the largest bandwidth result in the programme.
Fix the number; keep the conclusion.

Other load-bearing numbers, and what a 2× error would do to each:

| figure | where | if wrong by 2× |
|---|---|---|
| 12.8 GB/frame zoomed out | residency §2.5 | already wrong by 2.3×; **no change** to the recommendation |
| 6.5 GB/frame zoomed out | composite §2.2 | no change; the proxy wins at 3 GB too |
| 150–200 ms per 400 MB page-in | residency §4, §9 | **no change to any policy.** See below |
| 2.1 GB/frame at 1:1 | composite §2.1 | changes only whether integrated graphics is viable, which is not this document's case |
| "plausibly a third of the stack is hidden" | composite §3.1, residency §0 | **this one changes everything.** See finding 13 |
| LZ4 ratio 2–4× | residency §4 tier 2 | decides whether tier 2 is sufficient or tier 3 is mandatory |
| +8% for a quarter proxy / 450 MB | composite §4.5, residency §2.5 | no change |

On the page-in figure specifically: `docs/perf/layer-residency.md` §9 calls it
"the number every policy decision in §4 rests on". That is an overstatement of
its own weakness. Band it across frames, two budgets, never evict the active
layer, evict by stack distance — all four are robust at 75 ms and at 400 ms. The
*only* thing the figure decides is whether a tab switch needs a progress
affordance, and the answer is yes at every plausible value. Measure it anyway;
stop describing it as load-bearing, because that framing invites somebody to
block Stage 2 on a measurement that cannot change Stage 2.

The figure that genuinely is load-bearing and is nobody's measurement is how
much of a real document is hidden. `survey-documents.rs` already walks real
files. That is a one-run answer and it should be got before either document is
implemented, because it is the entire benefit of the cheapest item in both.

### 7. SUBSTANTIVE — slot identity: the proposal replaces an RAII guarantee with a discipline

`docs/perf/layer-residency.md` §3.3. The brief is right to flag this and the
document is right that the purpose can be preserved. But the *mechanism* it
proposes is weaker than the one it replaces, in the specific way this codebase
has already been bitten by.

Verified what makes the present arrangement safe: `SlotClaim` is
`Arc<Claim>` and `Claim`'s `Drop` calls `SlotPool::give_back`
(`crates/umber-core/src/layer.rs:265-293`). A slot cannot be reissued while
anything holds a claim, and "anything" includes an undo entry holding a deleted
layer. That is not a rule somebody follows; it is a rule the type system
enforces, and it is why parking a deleted layer's slice works.

The proposal keeps `SlotClaim` for the slot and introduces a **line** with no
equivalent. What protects a line is §3.3's sentence "the table is written in one
place, and a line is not reusable until every draw list naming it has been
submitted", plus §4's list of things never to evict — "anything with a readback
in flight (`capture`, `thumb`, the probes)". That is an invariant enforced at N
call sites, which is the exact shape CLAUDE.md records being forgotten at the
sixth.

And the sixth is easy to name here. All three of those readbacks are
multi-frame: `begin_capture` spans frames by design, `begin_thumb` is one in
flight at a time across frames, and `probe_canvas` collects two frames later.
Meanwhile §3.3 makes `write_layer_rect(slot, …)` mean "page in if needed, then
write" — so an ordinary undo, in the middle of a capture, can trigger a page-in
that evicts the line the capture is reading. The result is another layer's
pixels in the autosaved file. That is precisely the reissued-slot bug, moved one
level down and stripped of the type that prevented it.

**Recommendation: make a line an RAII claim with the same shape as `SlotClaim`.**
`begin_capture`, `begin_thumb`, `probe_canvas`, `begin_float` and the frame's
own draw list each hold a `LineClaim` for as long as they name a line; eviction
takes only lines nobody holds. Pinning becomes provable rather than listed, §4's
"never evict" list shrinks to a policy hint rather than a correctness
requirement, and the failure mode changes from silent wrong pixels to a page-in
that has to wait. Given the project already paid to learn this once, paying for
the type is cheap.

Two smaller notes on the same section:

- `slot_revisions: vec![0; MAX_SLOTS]` (`canvas.rs:3102`) is indexed by *slot*.
  §3.3 says `MAX_SLOTS` "stops being the ceiling on layers and becomes the
  ceiling on lines". If slots become names with no ceiling, that `Vec` indexes
  off the end. The name space must keep an independent bound — which it does
  today via `SlotPool` — and §3.3 should say so rather than leaving the
  impression that the 256 is retired.
- §3.3 says "`MAX_SLOTS`' 256 … the `const` assertion against
  `downlevel_defaults` stays exactly as it is". Verified that assertion exists
  (`canvas.rs:99`) and that `MAX_LAYERS * 2 < MAX_SLOTS` sits beside it
  (`canvas.rs:116`). If lines become the array depth, the second assertion is
  about a quantity that no longer exists and will read as satisfied while
  meaning nothing. Name it.

### 8. SUBSTANTIVE — the two hidden-layer treatments are complementary, and the merged rule is better than either

`docs/perf/composite-throughput.md` §3.1 elides the draw. `docs/perf/layer-residency.md`
§2.2 keeps the draw and points its slot at a void slice, and explicitly warns
against eliding. Read side by side they look like a contradiction. They are not.

I checked the composite document's elision rule against the loop and it is
correct: an invisible **clipped** draw is always droppable, because it writes
neither `acc` nor `clip_alpha`; an invisible **unclipped** draw writes
`clip_alpha = 0` and may be dropped only when no clipped draw appears before the
next unclipped one. The residency document's warning is about the *unconditional*
version of that, which would indeed rebind a clipped run to the wrong layer, and
its own §2.2 says exactly that.

So the merged rule, which neither document states:

- **Elide where the composite document's rule permits** — removes the draw, the
  fetch and the line.
- **Keep the draw with `visible: false` where it does not** — removes the line
  only, since the value is provably ignored.

That is strictly better than either alone and it costs one predicate over the
same single pass both documents already require for `active_draw_index`.

One correction to the residency half: the "shared void slice" is a canvas-sized
slice and therefore **400 MB on this document**, which §2.2's "no copy, no
readback, no GPU work" does not mention. It is also unnecessary: by §2.2's own
proof the sampled value is irrelevant, so a hidden draw can point at any
resident line — the active layer's, which is never evicted. If a void slice is
kept anyway for defensiveness, say that it costs a slice and that it needs no
clear, or somebody will clear it every frame out of caution.

### 9. SUBSTANTIVE — the exact-cache claim holds, with four caveats, and it is the better of the two cache designs

`docs/perf/composite-throughput.md` §5.2. Assessed rigorously, as asked.

**Is `Rgba32Float` a guaranteed render attachment on `Features::empty()`?** Yes.
Verified in `wgpu-types-29.0.4/src/texture/format.rs:990`:
`Self::Rgba32Float => (s_ro_wo, all_flags)`, and `all_flags` is
`attachment | storage | binding` with `attachment` containing
`RENDER_ATTACHMENT` (lines 924-928). The document's §8.3 instinct to verify it
on real machines is still right — `guaranteed_format_features` is what the spec
promises, not what an adapter reports — but the design is not resting on a
mistake.

**Does `textureLoad` bypass filtering and conversion?** Yes for filtering: a
`textureLoad` takes integer texel coordinates and no sampler, so no
interpolation occurs. And `Rgba32Float` has no transfer function, unlike the
layer array. So a 32-bit float written and loaded at the same integer coordinate
returns the identical bits.

**Does the claim survive the sRGB encode at the end of `composite.wgsl`?** Yes.
The encode (line 295, `linear_to_srgb`) is applied to `acc` after the loop and
after the background, identically on both paths. If `acc` at the cut is
bit-identical then everything downstream of it is a deterministic function of
identical inputs, so the encode produces identical bytes. The cache is upstream
of every non-linearity.

**Does it survive the wet stroke?** Yes, because the cut is below the active
draw and the stroke is applied only at `i == v.active_index` (line 194), which
is above the cut. Verified.

The four caveats:

1. **The cut position is wrong when the active layer carries effects.** Finding
   3. This is the one that stops R5 working rather than making it inexact.
2. **The bind group entry must declare `filterable: false`.**
   `Rgba32Float`'s sample type is float32-filterable only with
   `Features::FLOAT32_FILTERABLE` (`format.rs:1050, 1102`), which Umber does not
   request. Declaring the binding as filterable fails device validation on the
   machines that matter. §5.2 says "filtering is not used" but does not say the
   *layout* has to agree. The existing bind group already carries a `Filtering`
   sampler; that is legal beside a non-filterable texture only because nothing
   pairs them, and naga checks that pairing statically. Worth one sentence,
   because getting it wrong is a validation error at pipeline creation, which is
   fatal.
3. **`clip_alpha` at the cut.** §5.1 already gets this right, including the
   correction mid-paragraph that it is per-fragment and cannot be a CPU value.
   Verified against line 259. Endorsed, and §5.1's own note that forgetting it
   "silently unclips a clipped active layer" is the right severity.
4. **The miss path is unpriced.** §5.3 lists what refills the cache — anything
   below the cut changing, a camera move, an active-layer switch. A **camera
   drag** is a continuous miss, so during a pan or a zoom R5 and R6 both cost an
   extra full-screen pass and buy nothing. That is the one interaction where
   frame time is most felt. Neither document prices a panning frame, and §8's
   sweep has no "camera moving" axis. Add one.

With those, §5.2's claim is sound and it is **the better of the two cache
designs**: screen-space and `textureLoad` give bit-exactness that a canvas-sized
bake cannot have at any screen zoom (finding 2). The residency document's §5
bake wins only on *memory*, and §5.6 already concludes the bake is probably not
needed. The reconciliation both documents ask for is therefore:

> Build the screen-space cache. Keep the canvas-sized bake designed and
> unbuilt, as `docs/perf/layer-residency.md` §5.6 recommends, and record that
> it is a screen-fidelity trade rather than a quality-neutral one.

### 10. SUBSTANTIVE — the export fold is not "the same shader with a shorter loop", and its arithmetic is off by one

`docs/perf/layer-residency.md` §6. The intent is right and the claim of "one
statement of the blend maths" is worth defending, but three things are
understated.

- **Off by one.** "run `composite` with `layer_count = 1` and `baked_below = 1`"
  composites nothing: §5.1 defines `baked_below = N` as "start the loop at N",
  and a loop from 1 to `layer_count = 1` is empty. It wants `baked_below = 1`
  and `layer_count = 2`, or a different convention. On the export path, the one
  place exactness is a promise, this should be stated precisely.
- **It needs a third output mode and a new binding.** `composite.wgsl` writes
  either straight-alpha sRGB (the export branch, line 287) or sRGB over the
  checkerboard (line 295). Neither is premultiplied linear `acc`, which is what
  the fold must write for the next iteration to read. So the fold needs a new
  output path and a binding for the accumulator to be read from. That is a real
  change to the pass that four other things reuse — smaller than a second
  shader, and not nothing, and §6 should say which.
- **Ping-pong doubles the memory and the fold is needlessly serial.** Two
  `Rgba32Float` canvas-sized accumulators at 20000 × 5000 is **3.2 GB**, not the
  1.6 GB stated. And folding one layer at a time is 54 full-canvas passes over
  100 Mpx — roughly 170 GB of traffic and several seconds — when the pass
  already takes up to 191 draws. Fold as many layers as there are lines: with a
  dozen lines that is five passes rather than fifty-four. Same exactness, an
  order of magnitude less work, and it needs no new mechanism.

The `Rgba32Float` availability claim and the bilinear-weights-are-(1,0,0,0)
argument at zoom 1 both check out.

### 11. SUBSTANTIVE — the smudge probe is already undersampling document pixels, and that is worth naming

Not in either document. `probe_canvas` sets `zoom = 8 / (radius * 2)`
(`canvas.rs:5260`), so every brush over four pixels of radius probes the canvas
through a minification, using the same unmipped `Linear` sampler as the screen.
A 30-pixel brush takes a four-tap bilinear of a 60-pixel footprint to decide
what colour it picks up.

Two things follow.

- It reinforces `docs/perf/composite-throughput.md` §4.2 to the strongest
  degree: a LOD derived inside `composite()` would silently change what a
  smudging brush picks up, and this path is *already* the one at the extreme of
  the minification range.
- It is also, on its own terms, a pre-existing fidelity question about document
  pixels rather than screen pixels — an area average is arguably what a smudge
  should pick up. **It must not be fixed as a side effect of a mip landing.**
  Changing it changes every existing document's behaviour and is nobody's brief
  here. Record it as an open question in whichever document survives, so that
  the day somebody notices, the answer is not "the proxy work did that".

### 12. SUBSTANTIVE — the "one composite pass" invariant is not broken, and the reason should be written down

CLAUDE.md forbids splitting the stack into a pass per layer. Neither document
does that, and both should say why in the terms the rule is stated in, because a
reviewer will otherwise stop at R5 and R6 and the bake and count passes.

The rule is about the *per-layer* pass-and-bandwidth-round-trip cost scaling
with the stack. A memoisation pass that runs on change and is read with one tap
is O(1) in the stack per frame, not O(N). The bake is the same argument. What
would break the rule is a pass whose count grows with the layer count on a
steady-state frame — and §10's fold does exactly that, on the export path only,
once, which is why it is defensible there and would not be on the screen path.

Write that distinction into both documents; it is the sentence that lets a
reviewer accept three new passes without re-litigating the invariant.

### 13. SUBSTANTIVE — both documents' largest claimed win rests on an unmeasured guess about somebody else's file

`docs/perf/composite-throughput.md` §3.1: "very plausibly a third of the stack".
`docs/perf/layer-residency.md` §0: "54 layers include 30 roughs and alternates
drops from 21.6 GB to 9.6 GB". Neither is a reading of the file. The composite
document is honest about this in §10; the residency document puts the derived
9.6 GB in its recommendation without a hedge.

This is the one figure where a 2× error changes the plan. If a third of the
stack is hidden, hidden-layer non-residency is the cheapest large win and should
ship first, as both documents say. If a *tenth* is hidden — which is entirely
plausible for a finished commission where the roughs were deleted — then it
delivers 2 GB against a 12 GB overage, and the ordering should put the proxy and
background-tab eviction first instead.

`survey-documents.rs` already walks real files. Reporting hidden-layer count and
hidden-layer *bytes* per document is a small addition and it should be run
before either staging plan is committed to. Until then, both §0s should state
the benefit as conditional on a number nobody has.

### 14. MINOR — the `MAX_TAPS` bug is real, is independent of both designs, and should be filed now

`docs/perf/composite-throughput.md` §7.3 reports it as an aside. Verified and it
is worse than an aside.

`thumbnail.wgsl:46` sets `MAX_TAPS = 256`; line 81 computes
`step = max((span + MAX_TAPS - 1) / MAX_TAPS, 1)`; lines 87-99 iterate
`x += step.x` accumulating `peak = max(peak, texel.a)`. The destination is 64
texels, so `step.x > 1` needs `span.x > 16384`, i.e. a canvas over 16384 wide.
The comment at line 44-45 says that is "past `max_texture_dimension_2d` on the
limits Umber requests" — and `Document::MAX_EDGE` is **32768**
(`crates/umber-core/src/document.rs:171`), with `using_resolution` raising the
device limit from the adapter and CLAUDE.md recording an RTX 3080 reporting
32768 on Vulkan.

So on the 20000-wide document in front of us, `step.x = 2` and the bounds pass
samples every other column. A one-pixel vertical line at an odd x is missed
entirely, `peak` stays 0, and `content_rect` reports the layer empty — the
layers panel then draws the "nothing on this layer" checker for a layer that has
something on it, and caches that answer.

This is the third instance of the `using_resolution` failure CLAUDE.md already
names twice. It is a live bug on the exact document this programme is about, it
is unrelated to anything either design proposes, and it should be a commit of
its own today rather than a line in a perf document.

### 15. MINOR — confirmed stale and confirmed unsubmitted

Two of the coordinator's facts, verified independently, both belonging to
neither document:

- `crates/umber-app/src/ui.rs:511` calls `request_repaint_after(ANT_FRAME_MS)`
  whenever a selection or draft exists. CLAUDE.md's "The outline is a dashed
  line, not marching ants" is stale.
  `docs/perf/composite-throughput.md` §7.5 reports it correctly and correctly
  declines to edit CLAUDE.md. Its point that this makes R6 worth more than it
  looks is fair, and finding 6's corrected figure (about 6 GB rather than 6.5)
  does not change it.
- `install_import` (`crates/umber-app/src/app.rs:4452-4466`) loops
  `write_layer_rect` — which is `queue.write_texture` via `write_rect`
  (`canvas.rs:7210`) — with no `queue.submit` in or after the loop, while
  `uploads` is still alive. Peak is the CPU copy plus the staging belt plus the
  destination, all live at once. `docs/perf/layer-residency.md` §1 names the
  RAM half and not the staging half. A `queue.submit(None)` every few layers is
  a one-line fix and belongs to neither design.

### 16. MINOR — `opacity <= 0.0` is free on exactly the same proof

Line 259's `select(0.0, lay.a, visible && opacity > 0.0)` and line 262's
`if (!visible || opacity <= 0.0)` treat the two conditions identically, so a
layer at zero opacity needs no line by the same argument as a hidden one.
`docs/perf/layer-residency.md` §2.2 proves it and claims only the visibility
half; `docs/perf/composite-throughput.md` §3.1 states the elision rule over both
and is the more accurate of the two. Worth one word in the residency document,
because a painter sliding a layer to zero rather than clicking the eye is
common, and the mechanism costs nothing extra.

### 17. MINOR — prior art, and one claim I would soften

`docs/perf/layer-residency.md` §7 marks its confidence per source, which is
exactly right and is the treatment CLAUDE.md's ORA-specification anecdote asks
for. The Clip Studio half is verified in-tree (`csblocks.rs`, `BLOCK = 256`).

The one I would soften is §7's closing: "None of them tries to hold the document
in graphics memory. Umber is the outlier here, not the innovator." True and
slightly unfair — Photoshop and Krita predate the assumption that a GPU holds
the working set, and Krita's Instant Preview exists precisely because it does
not. The useful version of the sentence is narrower: every one of them has a
display pyramid and a paging tier, and Umber has neither. That is the actionable
claim and it does not need the editorial.

---

## What I would most want changed

**Rewrite `docs/perf/layer-residency.md` §0 bullet 1 and §8's Stage 1 so they
describe the change that actually has to be built.**

Everything else here is a correction to an argument. This one is a correction to
a *plan*, and it is the plan the supervisor is most likely to act on first
because both documents call it the cheap standalone win. It is a real win and it
is not cheap and it is not standalone: a hidden layer's pixels are read by the
thumbnail pass, by Save, by the autosave, by a canvas flip and by unhiding, and
three of those were verified above. Stage 1 as written ships a document that
cannot be saved correctly.

The honest Stage 1 is: the residency table, the line indirection, the elision
and void-slice pair from finding 8, **and** the CPU shadow with the thumbnail
and save paths redirected to it — which is most of Stage 2. That is still worth
building first and it is a different size of job, and the difference is 12 GB of
VRAM moving to RAM rather than disappearing.

## What I could not settle

- **Whether the proxy pops.** Finding 5 says it will pop at every octave with
  the present sampler, which is a stronger claim than either document makes, but
  whether it is *visible* at the proxy boundary specifically needs the captured
  frame pair `docs/perf/layer-residency.md` §9 asks for. Nothing short of that
  settles it.
- **How much of the user's document is hidden.** Finding 13. One run of
  `survey-documents.rs`.
- **Whether `Rgba32Float` is a render attachment on the machines Umber ships
  to.** The spec guarantee is verified; the adapters are not.
  `docs/perf/composite-throughput.md` §8.3 is the right instrument.
- **Whether the residency document's bake or the composite document's cache is
  needed at all**, which both documents say depends on measurements neither has.
  I agree with both §5.6 and §0's ranking that this is the last thing to build.
