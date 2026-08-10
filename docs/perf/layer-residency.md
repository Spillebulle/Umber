# Layer residency and paging

A real Clip Studio document, 20000 × 5000 with 54 layers, asks for **21.6 GB of
texture** on a card that has 10. Every layer occupies a full canvas-sized slice
of one `Rgba8UnormSrgb` array, whether it is visible, hidden, on screen, behind
a panel, or in a tab nobody is looking at.

This document is about one question only: **given whatever the storage unit
turns out to be, does it have to be on the GPU right now?** A sibling design
covers tiled storage — a layer held as sparse tiles rather than one rectangle —
another covers allocation accounting, and `docs/perf/composite-throughput.md`
covers the same pass from the speed side. §2.6 and §10 say exactly where the
four meet.

Nothing here is built.

**This is the second draft.** `docs/perf/critique-residency-composite.md`
reviewed the first against the code and found four blocking errors in it. Three
were arguments and one was a *plan*, and the plan was the thing a supervisor
was likeliest to act on. The retractions are marked in bold where they sit, in
this file's usual style, rather than collected at the end — but §0 is where the
important one landed, so read it as new rather than as a summary.

---

## 0. The recommendation, first

**Five stages, and the first one is not the one this document originally
named.**

**0. The CPU shadow, which saves no VRAM at all and pays for itself anyway.**
Everything below needs it, and the first draft filed it under "Stage 2" while
promising a Stage 1 that could not work without it. A layer's pixels change only
through methods that already bump `slot_revision`, and at commit the pre-stroke
bytes have *already been read back* for the undo patch — so a current CPU copy
of every layer can be maintained with **no new GPU work** (§3.4). It then makes
Save cheaper rather than dearer: `App::save_document` already materialises every
layer in RAM simultaneously through 54 blocking `read_layer_rect` calls
(`app.rs:3063-3071`), and reading the shadow instead deletes all 54.

**1. A hidden layer needs no VRAM. It still needs its pixels.** The proof that
`composite.wgsl` never reads a hidden layer's texels is sound — the critic
walked it exhaustively and could not break it, and §2.2 keeps it — but the
first draft's sentence "never read by **anything**" was false, and the
difference is the whole size of the job. **Verified: thumbnails
(`thumbs.rs:71-81`), Save (`app.rs:3063-3071`) and a canvas flip
(`app.rs:785-792`) all walk every slot with no visibility test.** So this stage
is Stage 0's shadow plus a thumbnail rule plus a shadow-backed save and flip,
and **the 21.6 GB does not disappear — it moves from a 10 GB card to a 32 GB
machine.** That is still the right trade, because VRAM is the fixed scarce
resource and RAM is neither fixed nor as badly behaved under pressure, but it is
not the trade the first draft described and it is what makes Stage 4's
compression a near-term item rather than a late one.

**How much it delivers is a number nobody has**, which the first draft also
glossed. "30 of 54 hidden" was a plausible guess about somebody else's file
presented as a derivation. `examples/survey-documents.rs` already walks real
files; adding hidden-layer count and hidden-layer *bytes* is small, and it
should be run before this plan is committed to. If a tenth is hidden rather
than a third, stages 2 and 3 should go first.

**2. A background tab is 100% evictable.** Every open document owns a whole
`CanvasRenderer` and array (`app.rs:141`), and a parked document contributes to
no frame. Two of these open is 43.2 GB with the second drawing nothing. Needs
Stage 0 and nothing else.

**3. One proxy array, shared with `composite-throughput.md`'s R7**, at a
reduction derived from the **footprint** and checked against a byte budget —
which is the reverse of what the first draft said, and §2.5 has the arithmetic.
It is a large bandwidth result (about 5.7 GB a frame at fit-to-window down to
about 0.7 — a corrected figure; the first draft said 12.8 and had missed the
loop's early return for fragments outside the document) and it removes the
aliasing that is there today. **It is not "the correct filter", and that word is
retracted**: a per-layer proxy computes the composite of the area averages where
the correct reduction is the area average of the composite, and those differ for
every mode. `composite-throughput.md` §4.4's wording — *more faithful, not
equal* — is right and is adopted here verbatim.

**4. Full-resolution slices become a fixed-depth cache of lines**, held by an
RAII `LineClaim` with `SlotClaim`'s shape rather than by a list of things not to
evict (§3.3, and the critic is right that the first draft swapped a
type-enforced guarantee for a discipline). wgpu exposes no residency or
sparse-texture API at all (§3.1), and a texture array cannot shrink without
reallocating and copying the whole thing, so a cache is the only mechanism
there is.

**Eviction is free and only page-in costs**, which survives review and is still
the most useful structural result here (§3.4).

**On the bake, this document has changed its recommendation.** A canvas-sized
bake is legal for every blend mode — it is a memoised prefix of the fold, which
§5.1 keeps and the critic endorsed — and the first draft then claimed it "can be
made quality-neutral by writing it to `Rgba16Float`". **That is retracted.** The
format is not the dominant error term: a canvas-sized bake is read through the
`Linear` sampler at `uv = doc / doc_size`, so at any screen zoom but 1 it is a
*resample of the fold* where the unbaked path is a *fold of resamples*, and no
format fixes that. `composite-throughput.md` §5.2's screen-space `Rgba32Float`
cache read with `textureLoad` has **neither** error term and costs 133 MB fixed
rather than 800 MB canvas-sized. **Build that one.** §5.7 records the single
condition under which the canvas bake comes back, which is a case the critic's
reconciliation does not cover: a screen-space cache is invalidated by camera
motion, so during a pan it needs every layer below the cut resident *every
frame*, and it therefore buys nothing for residency in exactly the case
residency is hardest.

**Nothing that decides pixels may read a proxy, a mip or a bake.** §6, and the
LOD must be a `CompositeParams` field with no default rather than derived inside
`composite()` — `probe_canvas` composites at `zoom = 8 / (radius × 2)`
(`canvas.rs:5260`), so a 30-pixel brush probes at 0.133 and a camera-derived LOD
would change what a smudge picks up.

---

## 1. The arithmetic, and what the driver is already doing

20000 × 5000 is 100 Mpx. `LAYER_BYTES_PER_PIXEL` is 4, so a slice is
**400 MB** (381 MiB). Fifty-four of them is **21.6 GB**. A mask is another
slice, so a masked stack doubles it. The float's preview spare is one more, and
a baked layer effect is one more each.

The card is 10 GB. So the document does not fit, and it is not close.

**The driver is already paging, and that is what the report is describing.**
Windows' WDDM demand-pages video allocations over PCIe when a process
over-subscribes; the application is not told. So "VRAM skyrockets" and the
canvas becomes unusable rather than the process dying. The driver has no idea
that thirty of those layers are hidden, that another twenty are outside the
working window, or that at fit-to-window zoom none of them is being read at
more than one texel in eight. **An application-level scheme beats the driver
because it has semantic information the driver cannot have**, not because it
pages more cleverly.

Three figures worth having in mind before any of this:

- **`ensure_slots` never shrinks and reallocates the whole array to grow.**
  At 400 MB a slice, `grown_capacity`'s doubling budget
  (`GROWTH_DOUBLING_BUDGET_BYTES`, 256 MiB) permits no doubling and
  `growth_quantum` degenerates to 1, so growth is exact — but each growth
  allocates a new array *while the old one is still alive*. Going from 53 to 54
  slices is a 42.8 GB transient. Adding layers one at a time to reach 54 copies
  594 GB in total. The import path avoids this by calling `ensure_slots` once
  with the final count (`Graphics::add_canvas`); interactive layer-adding does
  not. That belongs to the allocation-accounting design, and it is the reason
  §3.2 concludes that residency cannot be implemented by resizing the array.
- **`Opened::uploads` holds every layer's canvas-sized pixels in RAM
  simultaneously**, at `install_import`, and then drops them. For this document
  that is 21.6 GB of system memory at the moment of greatest pressure — and it
  is *exactly the CPU shadow §3.4 wants*, thrown away. Keeping it costs nothing
  at import; what it costs is holding it afterwards, which §4 budgets.
- **And the import's peak is worse than that**, which the first draft missed and
  the critic supplied: `install_import` (`app.rs:4452-4466`) loops
  `write_layer_rect` — `queue.write_texture` via `write_rect`
  (`canvas.rs:7210`) — **with no `queue.submit` in or after the loop**, while
  `uploads` is still alive. So the CPU copy, wgpu's staging belt and the
  destination are all live at once. A `queue.submit(None)` every few layers is a
  one-line fix belonging to no design here.

---

## 2. What genuinely has to be resident

### 2.1 The active layer

Full resolution, always, with no exception worth arguing for. It receives the
stroke, its slice is what `commit_stroke` scissors into, and its pre-stroke
bytes are what `read_layer_pieces` captures for the undo patch. Its mask is
resident whenever `StrokeStyle::on_mask`. A float's `base`, `source` and
preview slice are resident for the whole gesture.

A layer can be active *and hidden* — `LayerStack::refusal_at` refuses folders,
locks and text records but not invisibility — so the rule is **active or
visible**, never visibility alone.

### 2.2 A hidden layer's texels are never read *by the composite*, and that is provable

Walk `composite.wgsl`'s loop body with `visible == false`:

```wgsl
var lay = textureSampleLevel(layer_tex, samp, uv, slot, 0.0);
…
lay = lay * m;
if (clipped) { lay = lay * clip_alpha; }
else         { clip_alpha = select(0.0, lay.a, visible && opacity > 0.0); }
if (!visible || opacity <= 0.0) { continue; }
acc = composite_over(acc, lay * opacity, mode);
```

Cross-iteration state is exactly `acc`, `clip_alpha` and the counter (through
`stroke_here = i == v.active_index`). `lay` and `m` are locals.

* **Hidden and clipped**: `lay` is scaled and the iteration `continue`s. Dead.
* **Hidden and unclipped**: `clip_alpha = select(0.0, lay.a, false)` is `0.0`
  **whatever `lay.a` holds** — WGSL's `select` is a value selection with no
  arithmetic, so not even a NaN could propagate — and then the iteration
  `continue`s.
* The mask sample is only ever multiplied into `lay`, so it dies with it.
* A layer inside a **hidden folder** arrives with `visible: false` already:
  `LayerStack::effective_visible` (`layer.rs:1125`) ANDs the entry's flag with
  every ancestor's and `Editor::effected_draws` (`editor.rs:1939`) writes that
  into the draw.
* If the active layer is itself hidden, the stroke lines modify `lay` and `m`
  and the iteration still `continue`s. Dead.

The comment above that sample — "Sampled before the visibility test now,
because a hidden layer still has to bound whatever is clipped to it — to
nothing" — is right about *why the sample moved* and is easily misread as
saying the value matters. It does not.

**A layer at zero opacity is covered by the identical proof**, which the first
draft noticed and did not claim: line 259's condition is `visible && opacity >
0.0` and line 262's is `!visible || opacity <= 0.0`, so the two are treated
alike throughout. A painter sliding a layer to zero rather than clicking the eye
is common and the mechanism costs nothing extra.

#### The merged elision rule

`composite-throughput.md` §3.1 elides such a draw entirely; the first draft of
this document warned against eliding and kept the draw with a redirected slot.
Read side by side those look like a contradiction and are not — the warning was
about the *unconditional* version, which rebinds a clipped run to the wrong
layer. The merged rule, which neither document stated:

- **Elide** an invisible **clipped** draw always: it writes neither `acc` nor
  `clip_alpha`.
- **Elide** an invisible **unclipped** draw only when no clipped draw appears
  before the next unclipped one.
- **Otherwise keep the draw with `visible: false`** and point its slot at a
  resident line. The line's contents are provably ignored.

That removes the draw, the fetch *and* the line where it applies, and the line
alone where it does not.

Two corrections to how the first draft spelled the second half. It proposed a
**shared void slice**, and a void slice is canvas-sized — **400 MB on this
document**, which "no copy, no readback, no GPU work" did not mention. By the
proof above the sampled value is irrelevant, so no dedicated slice is needed at
all. Point such a draw at **line 0**, with a stated invariant that line 0 always
exists and its contents are never read; that is easier to hold than "the active
layer's line", which changes identity every time the selection moves and shares
a line with the draw the wet stroke is applied to.

And eliding shifts positions: `active_index` is a *position in the draw list*,
and getting it wrong previews the stroke on the wrong layer and jumps at
pointer-up — the failure `Editor::active_draw_index`'s own docs describe. The
elision must happen in the same pass that computes it.

#### What the proof does not cover, and why Stage 1 is not bookkeeping

**Three paths read a hidden layer's slice today, all verified:**

- **Thumbnails.** `thumbs.rs:71-81` builds its slot list from
  `layers().iter().filter_map(|l| l.slot())` with **no visibility test**, and
  `wanted` returns any slot whose revision has moved. Give a hidden layer no
  line and every hidden row's thumbnail goes blank — a visible regression on the
  exact document this is meant to rescue.
- **Save.** `app.rs:3063-3071` reads every entry with a slot through
  `read_layer_rect`, hidden included, because a hidden layer's pixels go into
  the `.ora`. Its mask likewise at 3074-3079.
- **Canvas flip.** `app.rs:785-792` collects `[l.slot(), l.mask()]` for every
  entry with no visibility test. A hidden layer flipped only on the GPU would
  come back unflipped when unhidden.

Add unhiding, `resize`, and an undo replaying a `PixelPatch` into a
non-resident slot.

So a hidden layer needs a **complete, current CPU shadow** and a route from that
shadow to each of those. That is Stage 0's machinery, which is why Stage 0 now
exists. Two of the four are cheaper than they look, and it is worth saying which:

- **Save is not a new cost — it is the same cost, earlier.** The save path
  already holds `Vec<Vec<u8>>` of every layer at once and says so in its own
  comment. Reading the shadow deletes 54 blocking readbacks and allocates
  nothing that was not already allocated.
- **Thumbnails need an ordering rule rather than a CPU renderer.** `Thumbs`
  caches by `(slot, revision)`, and a hidden layer is by definition not being
  painted on, so its revision is frozen for as long as it stays hidden. **Take
  the thumbnail before evicting** and the cache never goes stale. The only gap
  is a layer hidden before its thumbnail was ever taken — straight after an
  import — which is answered by rendering thumbnails during the import upload,
  while the pixels are resident anyway.
  **That answer has a prerequisite, and it is a live bug.** `thumbnail.wgsl`'s
  `MAX_TAPS` is 256 and its `span` is per destination texel, so `step` exceeds 1
  once the canvas is wider than 64 × 256 = 16384. At 20000 wide `step.x = 2`, the
  bounds pass samples every other column, a one-pixel vertical line at an odd x
  is missed, `content_rect` reports the layer empty and the panel caches "nothing
  on this layer" for a layer that has something on it. The comment at
  `thumbnail.wgsl:44-45` claims 16384 is "past `max_texture_dimension_2d` on the
  limits Umber requests"; `Document::MAX_EDGE` is **32768**
  (`document.rs:171`) and CLAUDE.md records an RTX 3080 reporting 32768 on
  Vulkan. It is the third instance of the `using_resolution` trap CLAUDE.md
  already names twice, it is unrelated to any design here, and it should be a
  commit of its own.
- **The flip** wants the shadow flipped on the CPU (a strided reversal, roughly
  a memcpy's cost) rather than 54 page-ins. Recording a per-layer "pending flip"
  applied lazily at page-in is more elegant and is refused: it adds state the
  file writer and the undo path both have to get right, for a gesture nobody
  performs in a loop.

**The one genuinely free part survives.** `bake_effects` builds its `wanted`
list without consulting `draw.visible` (`canvas.rs:5920-5932`), so a hidden
layer's drop shadow is baked every time it goes stale, into a slice it holds for
ever, and then discarded by the composite. Filtering by visibility is one
predicate and is a pure win.

`a_hidden_layer_holds_no_line_and_the_picture_is_unchanged` is the guard, and it
has to be a **pixel** test over a stack containing a clipped run above a hidden
layer — the case the unconditional elision breaks. Restating the rule in the
test proves nothing.

### 2.3 A collapsed folder is not a residency signal — confirmed

`Layer::collapsed` is documented as "purely a property of the list", CLAUDE.md
states it, and `Editor::effected_draws` never reads it: folders are flattened
away by `filter_map` on `slot()`, and every layer inside a collapsed folder
produces exactly the draw it produced when the folder was open. **Collapse says
nothing whatever about residency.** Worth writing down because it is the
intuitive signal and it is wrong.

The folder's *eye* is a different matter and is already handled: it folds into
its contents through `effective_visible`, so hiding a folder makes every layer
in it hidden and §2.2 applies to all of them. That is the control an artist
would actually reach for, and it works.

### 2.4 A background tab is entirely evictable

`Graphics::canvases` is one `CanvasRenderer` per open `DocId`, each with its own
`LayerStore`. A parked document (`Tab::parked`) is drawn by nothing: `render`
reaches exactly one canvas per frame, by the active id. The only things that
read a background document's slices are the autosave's capture and a tab
switch, and §3.4's shadow serves the first without touching the GPU.

So the whole of a background document's array can be given back, and this is
the largest single win available to anyone who works with two big files open.

**The machinery half-exists and loses the pixels.** `suspended()` drops `gfx`
outright and `resumed` rebuilds storage for every open document from
`Tab::parked_storage`; CLAUDE.md is explicit that "pixels do not survive, and
never have". Eviction is that path with a shadow behind it, which is the same
change that would make Android's resume non-destructive.

The cost is the tab switch, which pages the working set back in. §4 puts that at
roughly 150–200 ms a layer, so it must show that it is working. The mitigation
is the proxy: a switched-to document composites from its proxy immediately and
sharpens as lines arrive.

### 2.5 Zoomed out: the proxy — better, not exact

**Retraction first.** The first draft called a proxy pyramid "not a quality
compromise at all" and "the *correct* filter arriving". The first half of the
premise is verified and the conclusion does not follow.

What is verified: the array is created with `mip_level_count: 1`
(`canvas.rs:2073`), the shared sampler is `min_filter: Linear`
(`canvas.rs:2345`), and `composite.wgsl:193` samples at LOD 0. So at
fit-to-window the composite takes a four-tap bilinear of a footprint tens of
texels across, per layer. **That is undersampling and it aliases**, and today's
zoomed-out picture is not a faithful reduction of the document.
`composite-throughput.md` §4 agrees in its own words, so this half was never in
dispute.

What does not follow is that a per-layer pyramid is *the* correct filter. The
correct reduction of the picture is the area average of the **composite**; a
per-layer proxy computes the composite of the **area averages**. Those differ
whenever the fold is not affine in its layer arguments, which is every mode —
and `composite_over` is not affine even for Normal, because the cross term in
`dst + src(1 - src.a)` does not survive averaging. So:

| | today | per-layer proxy |
|---|---|---|
| faithful to an export at that scale? | no | no |
| how unfaithful? | a point sample of the area | an average of per-layer averages |
| aliases under camera motion? | yes | no |

**Better, not exact.** Nothing on screen or in a comment may say otherwise.

#### The bandwidth figure, corrected

The first draft said 12.8 GB a frame at fit-to-window. **That is wrong by
2.3×**: `composite.wgsl:155-160` returns before the loop for any fragment
outside the document's uv range, so at fit-to-window a 20000 × 5000 document in
a 2560-wide region occupies 2560 × 640 = 1.64 Mpx and not 3.7. On the same
cache-line arithmetic that is **about 5.7 GB**. `composite-throughput.md` §2.2
independently constructs 6.5 GB for a 3000-wide region, and corrected the two
**agree** — which is a stronger position than either had alone. The proxy takes
it to roughly 0.7 GB, which is also where that document lands. The conclusion
and the ranking do not move.

#### Sizing it: the footprint decides, and the byte budget is the check

**This inverts the first draft, which derived `k` from a byte budget and
asserted that "the two thresholds meet".** They do not meet structurally. A
threshold derived from a byte budget has no reason to land where a screen
footprint wants it, and on this canvas it landed in exactly the wrong place: a
16 MB per-layer budget gives `k = 3`, a threshold at zoom 0.125, and
fit-to-window on a 4K display is **0.15**. The mechanism that was supposed to
switch itself off rather than be switched off by a flag switched itself off at
precisely the zoom the document opens at.

The correct derivation runs the other way:

```
footprint_k = floor(log2(doc_width / widest_view_width))
byte_k      = smallest k with (slice_bytes / 4^k) <= per_layer_budget
k           = footprint_k                 -- the footprint decides
              ... and byte_k is what tells you whether you can afford it
```

At 20000 wide against a 3840 view, `footprint_k = floor(log2(5.2)) = 2` — a
quarter-resolution proxy, 25 MB a slice, 1.35 GB for 54 layers and 1.8 GB with
its own chain. **That is `composite-throughput.md` §4.5's fixed quarter, arrived
at from the footprint rather than picked**, so that document's constant turns
out to be right for this canvas and this document's *rule* is right about the
property that matters: on a 2048² canvas `footprint_k` is negative, `k` is 0,
the proxy is the layer, and the mechanism disappears without a flag.

Where the byte budget disagrees with the footprint, the honest answer is to say
so rather than to silently pick the cheaper: either pay for the finer proxy, or
accept a stated over-blur at fit-to-window and name the factor.

#### The band between the proxy and 1:1, which both documents left unnamed

Above the proxy's top level and below 1:1 there is a range — 0.25 to 1.0 at
`k = 2` — where the full-resolution array is minified by up to 4× with no mip
chain. A 20000-wide canvas at 40% zoom is a 2.5:1 minification of an unmipped
texture and neither document touched it.

**It is covered by giving the *lines* their own mip chains**, which falls out of
§3.3's cache and is affordable precisely because it is a cache: a full chain on
54 slices is +33% of 21.6 GB and unaffordable, and on a dozen lines it is four
slices' worth, 1.6 GB. So:

- **Every needed layer resident as a line** → sample the lines at the LOD the
  camera implies. Exact coverage of 1.0 down to `1/2^k`.
- **Otherwise** → sample the proxy, at best at LOD `k`.

Which makes the "threshold" a *residency predicate* rather than a zoom, and that
is both more principled and a new kind of nondeterminism: the picture's
filtering would change because somebody opened another tab. The recommendation
is therefore the simpler version — **a zoom threshold at `1/2^k` with the lines'
mips covering above it, and the proxy as the fallback below it or whenever the
set does not fit** — with the residency predicate recorded as the more correct
form if the simple one is seen to misbehave.

#### Four hazards, three of them supplied by review

- **The sRGB trap, and a separate proxy array does not escape it.** The first
  draft did not mention this at all. `LAYER_FORMAT` is `Rgba8UnormSrgb`
  (`canvas.rs:22`) and `LayerStore::raw_slot_views` is a per-slice `Rgba8Unorm`
  view built for the flip pass (`canvas.rs:2106-2117`) — sitting right there, and
  exactly what somebody writing a downsampler would reach for. Averaging encoded
  bytes takes black and white to linear 0.214 where the answer is 0.5: a
  60-level error, every reduced view far too dark. The generator must read and
  write through **sRGB** views. A separate proxy array inherits the trap
  identically, because it holds the same format for the same reason.
  Two things that are *not* problems: alpha is linear in an sRGB format either
  way, and premultiplied storage is what makes an unweighted mean the right box
  filter.
- **`mipmap_filter` is `Nearest`** (`canvas.rs:2347`), so `textureSampleLevel`
  at a fractional LOD snaps to a level. The first draft's honestly-flagged "can
  pop" is therefore *understated*: it pops at every octave of a continuous zoom,
  not only at the proxy seam. `MipmapFilterMode::Linear` is a one-line change and
  must land in the same commit as the first mip chain. **The sampler is shared**
  with the tip, the paper and the thumbnail passes, so either the change is made
  globally and its effect on those checked, or the proxy and the lines get a
  sampler of their own. Recommend the latter: a second sampler is a handful of
  bytes and does not require re-arguing three unrelated passes.
- **The LOD may not be derived inside `composite()`.** `probe_canvas` sets
  `zoom = PROBE_SIZE / (radius × 2)` (`canvas.rs:5260`), so a 30-pixel brush
  probes at 0.133; a camera-derived LOD would have a smudging brush picking
  colour out of a mip, which reaches document pixels. It must be a
  `CompositeParams` field **with no default**, the same shape `Background`
  already won. `composite-throughput.md` §4.2 is right and is adopted.
- **The wet scratch, which this document under-rated.** The first draft named it
  as "one wrinkle" and it is the objection. At zoom-out today both the layer and
  the scratch are point-sampled, so the preview and the committed result alias
  identically. With layer mips and an unmipped scratch, a thin wet stroke draws
  aliased over a smoothly minified stack and then *changes appearance at
  pointer-up* — which is the one thing the whole preview/commit discipline
  exists to prevent, arriving past it: `composite.wgsl` and `commit.wgsl` would
  still be implementing identical blending maths and the jump would happen
  anyway. **Layer mips require scratch mips**; they are one change and costing
  them separately understates it. `composite-throughput.md` §4.3 is the right
  statement of this and should be read whole.

### 2.6 Scrolled off screen at high zoom — this is tiling's, not residency's

At high zoom the visible region is a small rectangle of a large canvas, and
whole-layer residency has **no** signal: every visible layer is partly on
screen, so every one is resident whole. The saving is entirely in *which part*,
which is the sibling design's unit.

- **Zoomed in, tiling wins outright.** At 100% on a 4K monitor the visible
  region is 8 Mpx; 54 layers of it is 1.7 GB plus a scrolling margin. Fits.
  Whole-layer residency gives nothing here.
- **Zoomed out, tiling gives nothing** — every tile of every layer is on screen
  — and the proxy gives everything.
- So: **tiling is the working-zoom mechanism, the proxy is the overview
  mechanism, and §2.2/§2.4 are true at every zoom.** Residency is a policy layer
  above whichever unit tiling settles on; nothing in §3 or §4 assumes the unit is
  a whole layer.

---

## 3. The mechanism, because wgpu has no residency API

### 3.1 There are no sparse textures, and this was checked

`wgpu-types` 29.0.4 — the version in the lockfile — has no sparse,
tiled-resource or virtual-texture feature of any kind across all 47 features.
`TEXTURE_BINDING_ARRAY` (bindless, which would allow a texture per layer rather
than an array) is a feature and is not in `Features::empty()`, so it is
unavailable under the `downlevel_defaults` device Umber requests.

**So the only way to make a slice non-resident is not to have allocated it.**
There is nothing to tell the driver.

### 3.2 The array cannot shrink, so residency must be a cache

`ensure_slots` grows by allocating a new `LayerStore` and copying; it never
shrinks, and its own docs say why a shrink cannot be decided from inside it.
Shrinking a 54-slice array to 30 on this canvas allocates 12 GB while 21.6 GB is
still alive — 33.6 GB transient to *reduce* memory.

Therefore: **fix the full-resolution array's depth from a byte budget once, and
treat its slices as cache lines.** A dozen lines at 400 MB is 4.8 GB, plus 1.6 GB
if they carry mips (§2.5), which leaves room on a 10 GB card for the proxy
(1.8 GB), the stroke scratch and its mips, egui and the swapchain. That is
tight and it is a budget somebody can read.

### 3.3 The slot stays the layer's name; a line is an RAII claim

The `SlotClaim` remains the layer's identity, never reissued while anything
names it, and a residency table maps `slot -> Option<line>`. `LayerDraw` gains a
`line` beside its `slot`; only the renderer's array indexing goes through the
table. Every consumer that names a slot goes on naming one, so no `PixelPatch`
stops meaning its own pixels and no cache key changes meaning.
`write_layer_rect(slot, …)` becomes "page in if needed, then write" — or, for a
non-resident layer, "write the shadow", which is cheaper and is what an undo
into a hidden layer should do.

**The first draft protected a line with a sentence and a list, and that is the
wrong shape.** It said "the table is written in one place, and a line is not
reusable until every draw list naming it has been submitted", plus a list in §4
of things never to evict. That is an invariant enforced at N call sites, which
is exactly what CLAUDE.md records being forgotten at the sixth — and the sixth
is easy to name. All three readbacks that must pin a line are **multi-frame**:
`begin_capture` spans frames by design, `begin_thumb` keeps one in flight across
frames, and `probe_canvas` collects two frames later. Meanwhile
`write_layer_rect` now means "page in if needed", so an ordinary undo in the
middle of a capture can evict the line the capture is reading, and another
layer's pixels land in the autosaved file. That is the reissued-slot bug moved
one level down and stripped of the type that prevented it.

**So a line is a `LineClaim` with `SlotClaim`'s shape**: `Arc<…>` with a `Drop`
that returns the line. `begin_capture`, `begin_thumb`, `probe_canvas`,
`begin_float` and the frame's own draw list each hold one for as long as they
name a line; eviction takes only lines nobody holds. Pinning becomes provable
rather than listed, §4's "never evict" list drops to a policy hint rather than a
correctness requirement, and the failure mode changes from silent wrong pixels
to a page-in that has to wait. The project has already paid to learn this once.

Two corrections to what the first draft said about the constants:

- **`MAX_SLOTS` is not retired.** `slot_revisions: vec![0; MAX_SLOTS]`
  (`canvas.rs:3102`) is indexed by *slot*. If slots become names with no
  ceiling that `Vec` indexes off the end. The name space keeps an independent
  bound — which it has today, through `SlotPool` — and the sentence "`MAX_SLOTS`
  stops being the ceiling on layers and becomes the ceiling on lines" was
  careless: it becomes **both**, one bounding names and one bounding the array
  depth, and they are different numbers.
- **`MAX_LAYERS * 2 < MAX_SLOTS`** (`canvas.rs:116`) asserts that a fully masked
  stack and a float fit under the array depth. If lines become the array depth
  that assertion is about a quantity that no longer exists and will read as
  satisfied while meaning nothing. It has to be restated against the *name*
  space or deleted with a reason; leaving it is worse than either.

### 3.4 Eviction is free; only page-in costs

A layer's pixels change only through methods that already bump `slot_revision` —
commit, float commit, `write_layer_rect`, clear, mask fill, flip, resize, and
(as `docs/layer-effects.md` found the hard way) `render_float`. That set is
enumerated and `a_dragged_float_carries_the_effect_derived_from_it` holds it.

So maintain a **CPU shadow** per layer with a revision beside it. A line whose
shadow matches is **clean**, and evicting a clean line costs nothing: forget the
mapping, reuse the line. Only the layer just painted on is dirty, and it is the
active layer, which is never evicted.

Bringing a shadow up to date after a commit is **free**: the undo capture has
*already read back* exactly the damaged pixels at pointer-up. Write the shadow
from the patch that was captured rather than adding a second readback. That is
the neatest consequence here and it should be built that way.

Two more fall out:

- **The autosave's capture should read the shadow.** `Capture` exists to spread
  a whole-document readback across frames without a stall; with a current shadow
  there is nothing to read back. Same for Save's 54 blocking `read_layer_rect`
  calls.
- **There is no page-out queue.** Nothing dirty is ever evicted.

**When it runs.** Residency is *decided* once per frame before any GPU work, in
the place `bake_effects` already sits. Page-ins are *executed* banded across
frames, `drive_capture`'s shape exactly. Evictions are a table write.

**Thrash is a refusal condition, not an eviction condition.** If a single
frame's draw list will not fit, evicting to make room only means evicting again
next frame. The answer is the proxy, and if that does not apply the honest
response is to say the document is over its residency budget and name the
figure — the treatment `EffectsOverBudget` and the undo panel's "Earlier edits
discarded" already get, which exist because the silent versions were read as
bugs.

---

## 4. Where evicted pixels go, and what a page-in costs

**The cost, from in-tree measurements.** `CAPTURE_CHUNK_BYTES` is 4 MiB because
that is "comfortably under a millisecond" for a memcpy out of a mapped staging
buffer, and a 16 MB layer measured about 5 ms on the same path — 3–4 GB/s for a
CPU-side pass over uncached memory. `Queue::write_texture` does one such copy
into a staging belt and then a DMA. So a 400 MB page-in is **roughly
100–130 ms of CPU copy plus 20–40 ms of DMA**, call it 150–200 ms, **dominated
by the CPU side rather than by PCIe**. It is an estimate built from a
measurement of a different path.

**The first draft called this "the number every policy decision in §4 rests on"
and that was an overstatement of its own weakness.** Band it across frames, two
budgets, never evict the active layer, evict by stack distance: all four are
robust at 75 ms and at 400 ms. What the figure genuinely decides is narrower and
is worth naming precisely, because it is not what the first draft implied:

- Whether a **tab switch** needs a progress affordance. The answer is yes at
  every plausible value, so this does not need measuring to be decided.
- Whether **unhiding a layer** can be synchronous. That is the one that turns on
  the number: unhide is a single click on an eye with nowhere to put a progress
  bar, and 150 ms is a hitch where 400 ms reads as a hang. If it is the high
  figure, unhide has to composite from the proxy immediately and sharpen — which
  is the same mechanism as the tab switch, so the cost of being wrong is that
  the mechanism is needed a stage earlier than planned.

Measure it anyway; stop describing it as blocking Stage 0, because it cannot
change Stage 0.

**Where the bytes live**, in the order to try them. Note that Stage 1 moving
21.6 GB from VRAM to RAM makes tier 2 a near-term item and not a late one:

1. **System RAM, uncompressed.** 21.6 GB for this document, most of a 32 GB
   machine. The first implementation, with a budget that refuses.
2. **System RAM, cheaply compressed.** The uniform-piece scan `PixelPatch`
   already uses — "a piece whose pixels are all identical is held as that one
   pixel" — costs four comparisons on busy paint and collapses an empty or
   flat-filled layer to nothing. Beyond that, LZ4 (`lz4_flex` is pure Rust) at
   roughly 500 MB/s–1 GB/s; the ratio on 8-bit premultiplied artwork is the
   figure that decides whether tier 2 is sufficient or tier 3 is mandatory, and
   it must be measured **per layer** on the user's own files and reported as a
   distribution: the win is in the empty and flat layers and a mean will hide
   it. PNG at `Compression::Fast` is measured elsewhere at about 1.6 ms/MB —
   640 ms a layer, fine on a background thread for cold layers and useless on
   the eviction path.
3. **A memory-mapped scratch file.** Photoshop's answer and Krita's (§7). The
   containment rules are written: `autosave::Reaper` is the model for anything
   that deletes on the user's behalf and `SessionMark`'s exclusive lock for "did
   the last run stop rather than end". A scratch file must never be mistakable
   for a document and the sweep must be structurally unable to leave its own
   directory.
4. **The source file itself.** For a layer opened from a `.clip` or `.ora` and
   never edited, the pixels are already on disk, compressed, and `csblocks`
   decodes a `.clip` layer **per 256-square block** — genuinely per-tile. It is
   a fork rather than a step: Umber reads a document whole and closes the file,
   so this means keeping the source open for the session, and it stops working
   the moment a layer is edited. Named so nobody rediscovers it as a shortcut.

**The eviction policy**, once `LineClaim` has made pinning a correctness
guarantee rather than a list:

- **Never chosen**: anything holding a claim — the active layer and its mask,
  the float's slices, the readbacks, the frame's own draw list.
- **Chosen first, for free**: hidden and zero-opacity layers, which need no line
  at all rather than needing eviction; then background tabs.
- **Then by stack distance from the active layer**, which predicts better than
  recency: a painter moves up and down the stack a step at a time.

---

## 5. The bake, and why this document no longer recommends it

### 5.1 A bake is a prefix of the accumulator, and that part stands

Every blend mode in `blend.wgsl` reads exactly one thing about what is below
it — `acc` — because the W3C compositing formula is a function of the backdrop
colour and the backdrop alpha and nothing else. So **replacing draws `0..k` by a
single slice holding `acc` after draw `k-1` is exact for every blend mode above
it**, not just for Normal. Associativity of `over` is not the argument; "the
loop is a fold and a bake is a memoised prefix of it" is.

The shader change does not touch the loop body: one uniform, `baked_below: u32`.
`acc` is initialised from the bake instead of from zero and the loop starts at
N. Zero is the exact identity and is what every path that must not bake passes.

### 5.2 The two boundary rules, one of them corrected

**A bake may not end in the middle of a clipped run.** `clip_alpha` is a second
piece of running state and a single RGBA slice does not carry it. The cheap rule
that avoids exporting it: **the first unbaked draw must be unclipped.** It then
sets `clip_alpha` itself before anything above can read it. A clipped run
entirely inside the bake is fine.

**No draw belonging to the active layer may be inside a bake** — and the first
draft said "the active *layer*", which is not enough. `Effect::rank`
(`effect.rs:402-410`) gives a drop shadow rank 1 and an outside or centred
outline rank 3 against the layer's own 4, so `baked()` pushes those draws
*below* the layer's own. A cut immediately below the active draw therefore puts
the active layer's shadow and outline inside the cache — and the effect extract
takes the wet stroke into account, so they change on every frame of a stroke,
which is precisely what the cache exists to hold still. The cut must be below
the **lowest draw belonging to the active layer**, which `bake_effects` already
knows. The same correction applies to `composite-throughput.md` §5.1, and a
layer carrying a drop shadow belongs in the cut-position sweep.

Any layer whose values are being dragged must also come out of the bake for the
duration of the gesture, which is what `Editor::layer_draws` already does for
the float.

### 5.3 The suffix bake is the weaker half

An "over cache" is computed from `acc = 0`, so it is only valid if the whole
suffix composited independently and then put over the prefix equals compositing
each in turn. That holds when **every draw in the suffix is `src-over`** (any
opacity, since opacity is a pre-scale of the source inside each `over`) and
fails for every other mode. Its first draw must also be unclipped.

**The prefix bake is always available and the suffix bake is conditional**, and
a stack's upper layers are likelier to carry Multiply, Screen or Overlay than
its lower ones — so the conditional half is the one that will frequently be
refused, and the residency budget must not have been sized assuming it applies.

### 5.4 What a bake actually costs: two error terms, and the first draft priced the wrong one

**The format term.** Writing `acc` to `Rgba8UnormSrgb` re-encodes RGB and
quantises alpha to 8 bits linear. Alpha is at most 1/510 out. Colour precision
is **never worse than the worst layer inside the bake**: alpha composites as
`ao = as + ab(1-as)` under every blend mode, so the accumulator's alpha is
monotonically non-decreasing and is at least every contributor's, and
premultiplied 8-bit loses colour precision in proportion to how small alpha is.
The extra rounding is at most half an sRGB code (one code near white is 0.0089
linear), so under `src-over` above the bake the final output differs from the
unbaked path by at most one code. Under Colour Dodge, Colour Burn and Divide it
is unbounded, because each amplifies a backdrop error without limit. And errors
do not compound over time: a bake is always rebuilt from the original layers.

**The resampling term, which the first draft missed entirely.** A bake is
**canvas-sized** and the composite samples it at `uv = doc / doc_size` through
the `Linear` sampler. So at any screen zoom but 1, the screen reads a *bilinear
resample of the fold* where the unbaked path computes a *fold of bilinear
resamples*. Those are the same two quantities §2.5 separates for the proxy, and
they differ for the same reason. It is **independent of the format** and is not
reduced by going from 8 bits to 16.

**So the first draft's conclusion is retracted.** It recommended `Rgba16Float`
at 8 bytes a pixel — 800 MB a bake slice on this canvas — to remove an error
term that is not the dominant one. Since the resampling term dominates at every
screen zoom and cannot be paid off, the format should be chosen for **memory**:
`Rgba8UnormSrgb` at 4 bytes, with the refusal rule kept (no bake beneath a
Colour Dodge, Colour Burn, Linear Burn or Divide draw, because that is where
*both* terms amplify).

At zoom 1 the resampling term is exactly zero, which is why §6's export fold is
sound on this point and why its accumulator is chosen on precision instead.

`Rgba16Float` was verified as a guaranteed attachment and filterable on
`Features::empty()` (`wgpu-types-29.0.4/src/texture/format.rs:987`,
`(msaa_resolve | s_ro_wo, all_flags)`), and Umber already renders to it as
`STROKE_COLOR_FORMAT`. The verification stands; the recommendation it supported
does not.

### 5.5 Invalidation, and the rebuild that is too expensive

A prefix bake is stale when any draw inside it changes pixels, opacity, blend
mode, visibility, clip flag, mask or effects — and when the boundary moves,
which is whenever the artist selects a different layer.

- Moving the boundary **up** by one is an incremental fold,
  `prefix' = over(prefix, layer_k)`: one page-in, one pass.
- Moving it **down** is not. `over` is not invertible once alpha saturates, so
  it is a rebuild from layer 0, **paging in every layer under the new
  boundary** — forty layers at 150–200 ms each is six to eight seconds, on a
  click.

A **ladder** of cumulative prefixes at every eighth layer bounds a downward move
to seven folds and confines an edit's invalidation to the rungs above it. Seven
rungs at 400 MB is 2.8 GB on this canvas, which is most of the line budget; on a
4096² canvas a rung is 67 MB and the ladder is obviously worth it. **The right
structure, and its rungs cost about as much as the layers they replace on the
canvas that motivated this document.**

### 5.6 It is probably not needed, and two arguments for it turned out to be false

- **It buys nothing for frame time.** The composite's fragment count is the
  *window* clipped to the document, not the canvas. What is expensive at
  zoom-out is the bandwidth per tap (§2.5), and the proxy fixes that far more
  cheaply.
- **Tiling plus the proxy may already fit.** Zoomed in, 54 layers of an 8 Mpx
  view with masks and a margin is 3–4 GB. Zoomed out, the proxy is 1.8 GB.
  Neither needs a bake.

### 5.7 The reconciliation, and the one thing it leaves out

`composite-throughput.md` §5.2's screen-space cache stores `acc` at the cut as
`Rgba32Float` at **screen** resolution and reads it with `textureLoad` at the
same integer coordinate it was written at. That has **neither** of §5.4's error
terms — no resampling, because it is never resampled; no format term, because 32
float bits written and loaded are the same bits — and it costs 133 MB fixed
rather than 800 MB canvas-sized. Where both are available it is strictly more
exact and strictly cheaper.

**So: build the screen-space cache, and keep the canvas bake designed and
unbuilt.** Two notes to carry with it: the cut position must be
§5.2's corrected one, and the bind group entry must declare `filterable: false`,
because `Rgba32Float` is float32-filterable only with `Features::FLOAT32_FILTERABLE`
and declaring otherwise is a pipeline-creation validation error, which is fatal.

**The one thing that reconciliation leaves out, and it is this document's
remaining disagreement.** A screen-space cache is keyed on the screen camera, so
**a camera drag misses it every frame**. During a pan or a zoom it must be
refilled from draws `0..k`, which means every layer below the cut resident *on
every frame of the gesture*. So it buys nothing for residency in the one case
residency is hardest, and it is the case a painter is in constantly. A
canvas-sized bake lives in document space and is invalidated by *edits* only, so
a pan over a baked stack needs nothing below the cut resident at all.

They are therefore not competitors settled by exactness; they are invalidated by
different things and answer different questions:

| | screen-space cache | canvas bake |
|---|---|---|
| exact at screen zoom? | yes | no (resampling) |
| memory | 133 MB, fixed | 400 MB × rungs, canvas-sized |
| survives a camera drag? | no | yes |
| reduces residency? | no | yes |

The ranking is unchanged — build the exact one first, because §5.6 says the
residency problem is probably answered by tiling and the proxy. **The condition
under which the canvas bake comes back is specific and should be written down
rather than rediscovered: if measurement after stages 0–3 shows layer count
still binding, and panning at working zoom is the case that breaks, the bake is
the only mechanism that helps and its inexactness is the price.** Neither
document should be read as having ruled it out.

---

## 6. Nothing that decides pixels may read a proxy, a mip or a bake

The rule `docs/thumbnails.md` states for a stored preview applies here with the
same force. Export, Save, the autosave capture, `pick_colour`, `pick_patch`,
`probe_canvas`, `drive_thumb`, commit, the undo capture, `flip_layers` and every
transform pass take the full-resolution path at LOD 0. `baked_below` is zero and
the proxy is unbound on every one of them.

**Those `mip_level: 0` literals become load-bearing the day a mip exists.**
About thirty sites in `canvas.rs` name it and they are correct today only
because there is one level — the same shape as `.min(MAX_SLOTS)` quietly
changing meaning when `MAX_SLOTS` moved from 129 to 256, and it should be
recorded at the constant, in advance.

That promise creates one real problem: **an export needs every layer, and the
layers do not fit.** The answer is to **fold rather than loop**, and the first
draft got three things about it wrong.

1. **An `Rgba32Float` accumulator, and a ping-pong pair is 3.2 GB, not 1.6.**
   Band it in horizontal strips. `Rgba32Float` is `all_flags` on
   `Features::empty()` (`format.rs:990`), so it is a render attachment; it is
   not filterable without `FLOAT32_FILTERABLE`, which does not matter because
   `render_export` composites at zoom 1 with the pivot at the document centre,
   so the sampler's bilinear weights are (1,0,0,0) — already what
   `saving_and_reopening_does_not_move_a_pixel` relies on. The binding must
   still declare `filterable: false`.
2. **Fold as many layers as there are lines, not one at a time.** The first
   draft's "`layer_count = 1` and `baked_below = 1`" composites *nothing* —
   a loop from 1 to 1 is empty — and folding singly is 54 full-canvas passes,
   roughly 170 GB of traffic and several seconds. With a dozen lines it is five
   passes at `baked_below = 1, layer_count = 13`. Same exactness, an order of
   magnitude less work, no new mechanism.
3. **It needs a third output mode and a binding.** `composite.wgsl` writes
   straight-alpha sRGB (the export branch) or sRGB over the checkerboard.
   Neither is premultiplied linear `acc`, which is what the fold must write for
   the next iteration to read. That is a real change to a pass four other things
   reuse — smaller than a second shader, and not nothing, and it should be
   stated as such rather than as "the same shader with a shorter loop".

**And this is where the one-pass invariant has to be answered explicitly**, or a
reviewer stops at the bake and the cache and counts passes. CLAUDE.md forbids a
pass *per layer*, because the cost it is protecting is a full-screen bandwidth
round trip that scales with the stack. A memoisation pass that runs on change
and is read with one tap is O(1) in the stack per steady-state frame. The fold
above is genuinely O(N) passes — **on the export path, once, where the
alternative is not being able to export at all**, and it would not be
defensible on the screen path. That distinction is what lets a reviewer accept
these without re-litigating the rule.

---

## 7. Prior art

Marked for confidence, because CLAUDE.md records a case where a search summary
contradicted a published specification and the specification was right.

**Krita — verified from KDE's own wiki and Krita's manual.** The paint device is
tiled at **64 × 64 pixels** (`TILEWIDTH 64`, `TILEHEIGHT 64`). Tiles are
compressed **individually**, with LZF and LZO named and a `KisAbstractCompression`
abstraction permitting others, and the same compressor serves both the save
subsystem and the **swapper**. The swapper writes to a swap file whose size
limit and location are user settings, alongside a memory limit in percent or
bytes; the manual notes that hitting both limits can freeze the application.
Krita also offers "Use Region of Interest", which renders only the visible
portion above a configurable image size. *Inference, not verified*: that
eviction is LRU over the tile store, and the exact role of `KisImagePyramid` —
the class exists and Krita does maintain a display-side pyramid (its Instant
Preview / Level of Detail mode paints strokes at reduced resolution on large
canvases), but I have not read the source.

**Photoshop — verified from Adobe's documentation.** Photoshop maintains an
**image pyramid** whose depth is the "Cache Levels" preference, where "Level 0
indicates the full resolution canvas, and Level 1 indicates a cache that is half
of the size of the full resolution, and so forth"; there is a separate "Cache
Tile Size" preference; and scratch disks are used "as temporary memory when
system RAM is low", explicitly distinct from the operating system's own paging.
That is §2.5's proxy pyramid and §4's tier 3, both described by the vendor.
*Inference*: the specific tile size and the eviction policy.

**Clip Studio — half verified in this repository.** A `.clip` layer's bitmap is
stored as **256-square zlib blocks**, and Umber decodes them: `csblocks.rs`,
`const BLOCK: usize = 256`, one zlib stream per block. *Inference*: that Clip
Studio's runtime pages at the same granularity. It is a strong inference and it
is an inference; §4's tier 4 is the only thing that would depend on it.

**The actionable shared conclusion**: every one of the three has a display
pyramid and a paging tier, and Umber has neither. The first draft closed this
section with "Umber is the outlier here, not the innovator", which is editorial
and slightly unfair — Photoshop and Krita predate the assumption that a GPU
holds the working set, and Krita's Instant Preview exists precisely because it
does not. The narrower sentence is the useful one.

---

## 8. Staging

Costs are rough, and the units are "days of one person who knows this codebase".

**Stage 0 — the CPU shadow. Medium, and it saves no VRAM.**
Populate it from `Opened::uploads` at import instead of dropping it; keep it
current from the undo patch at commit, which is free; redirect Save and the
autosave capture to it, which *removes* 54 blocking readbacks and the `Capture`
machinery's reason to exist. Nothing visible changes and everything below
depends on it. Ships with the budget and its refusal notice, because this is the
stage that puts 21.6 GB in RAM.

**Stage 1 — hidden and zero-opacity layers hold no line. Medium.**
The residency table, `LineClaim`, `LayerDraw::line`, the merged elision rule
(§2.2) with `active_index` recomputed in the same pass, the thumbnail ordering
rule, the shadow-backed flip, and `bake_effects` filtering by visibility.
Prerequisite: the `MAX_TAPS` fix, or hidden layers' thumbnails are wrong before
they are cached. Delivers whatever fraction of the stack is hidden — **run
`survey-documents.rs` first**, because if it is a tenth rather than a third this
stage should follow Stage 2 rather than precede it.

**Stage 2 — background tabs. Small, on top of Stages 0 and 1.**
Evict a parked document's whole array; page in on a tab switch, banded; show
that it is working.

**Stage 3 — the one proxy array, shared with R7. Medium, highest quality
return.** `k` from the footprint, checked against the byte budget; its own mip
chain and its own sampler with `MipmapFilterMode::Linear`; generation through
**sRGB** views; the LOD as a `CompositeParams` field with no default; mips on
the lines to cover the band above the proxy; **and the stroke scratch mipped in
the same commit**, because layer mips without scratch mips is a stroke that
changes at pointer-up.

**Stage 4 — compression, then the scratch file. Medium, and earlier than the
first draft said.** Stage 0 puts the whole document in RAM, so this is what
stops RAM being the new wall. `PixelPatch`'s uniform-piece scan first; LZ4 next,
gated on a per-layer measurement of the user's own files; the memory-mapped
scratch file last, with `Reaper`'s containment discipline.

**Stage 5 — the exact screen-space cache** (`composite-throughput.md` §5), with
§5.2's corrected cut position. The canvas bake stays designed and unbuilt, under
§5.7's stated condition.

---

## 9. What could not be settled from the code, and what would settle it

- **How much of the user's document is hidden.** This is the figure a 2× error
  changes the *plan* on, and it is nobody's measurement. `survey-documents.rs`
  already walks real files; reporting hidden-layer count and hidden-layer bytes
  is small. Get it before committing to the staging above.
- **The compression ratio on real painted work.** Decides whether §4 tier 2 is
  sufficient or tier 3 is mandatory. Per layer, reported as a distribution.
- **The page-in figure.** 150–200 ms for 400 MB is extrapolated from `Capture`'s
  measurement of a different operation. It decides whether unhide can be
  synchronous and nothing else. `examples/measure-residency.rs`: upload a
  canvas-sized slice through `Queue::write_texture` at several band sizes and
  print the CPU copy and the whole call separately, because they are different
  costs with different fixes.
- **The zoomed-out bandwidth.** 5.7 GB is cache-line arithmetic corrected once
  already. A frame capture at fit-to-window would settle it and would also say
  whether the composite is bandwidth-bound or already PCIe-bound.
- **Whether the proxy seam is visible.** The `Nearest` mip filter means it will
  snap at every octave, which is a stronger claim than "can pop" and is fixed by
  a one-line sampler change; whether the seam at the proxy boundary specifically
  is visible needs a captured frame pair at either side of it.
- **Whether `Rgba32Float` is a render attachment on the machines Umber ships
  to.** The spec guarantee is verified; the adapters are not.
  `measure-limits.rs` should print `adapter.get_texture_format_features` for it.
- **Whether the driver is paging or failing.** "VRAM skyrockets" reads as WDDM
  demand-paging, but a `create_texture` failure would reach `crash::device_error`
  and be fatal. A log of `ensure_slots`' growth lines and the adapter's reported
  memory from the affected machine would say.

And one that is **not** this programme's to fix, recorded so that the day
somebody notices, the answer is not "the proxy work did that":

- **The smudge probe is already undersampling document pixels, and its own
  comment says otherwise.** `probe_canvas` sets `zoom = 8 / (radius × 2)`
  (`canvas.rs:5260`) with a comment saying this "makes the readback an area
  average over what the dab covers rather than a point sample that a single
  stray pixel could dominate". With `mip_level_count: 1` and a `Linear` sampler
  it is exactly the point sample the comment disclaims: a 30-pixel brush takes a
  four-tap bilinear of a 60-pixel footprint. Mipping would make it do what its
  comment says — and would change what every existing document's smudging
  brushes pick up. **It must not be fixed as a side effect of a mip landing.**

---

## 10. Where this sits beside the other three designs

- **`composite-throughput.md`.** Agreed: one proxy array serving both, sized by
  the footprint (this document's §2.5, corrected) rather than by a fixed
  quarter — which lands on that document's `k = 2` for this canvas and switches
  itself off on a small one. Agreed: the sRGB view trap, the LOD as a parameter
  with no default, the scratch-mip requirement, and that the screen-space cache
  is the one to build. The remaining disagreement is §5.7's: the screen cache is
  invalidated by camera motion and therefore reduces residency by nothing during
  a pan, which is the case the canvas bake exists for.
- **The tiling design.** §2.6. Tiling is the working-zoom mechanism and the
  proxy is the overview mechanism; residency is a policy layer over whichever
  unit tiling settles on.
- **The allocation-accounting design.** §1's three figures — `ensure_slots`'
  whole-array copy, `Opened::uploads`, and `install_import`'s missing submit —
  are all its, and none is caused by anything here.
- **`LayerStack::MAX`'s 64** is under every entry cap and over every byte budget
  in the codebase. The cap that matters on a large canvas is not a count, and
  the same sentence is already written about the undo budget and the effect
  cache. Four designs have now reached it independently; it may be worth one
  shared answer rather than a fifth.
