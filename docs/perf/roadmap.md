# The performance programme, ordered

Six designs, three critiques and one round of rebuttal sit beside this file.
They are good and this does not summarise them — read them. This is the
cross-cutting view none of the six authors had: where they collide, which one
is right when they do, what order the work goes in, and what should not be
built.

**`survey-residency` has since answered the question five of the six named as
the thing that decides everything.** Real layers are **13.5% covered** across
506 slices in 32 documents; the motivating 20000×5000 document is **21.20 GB
dense against 1.42 GB tiled, 6.5% covered**. `tiled-layer-storage.md` §10
nominated 15% as "obviously right" and 60% as "build residency instead". So the
atlas is GO on its author's own stated criterion, and this document treats that
as settled rather than re-deciding it.

Three consequences the authors could not have drawn, because the number did not
exist when they wrote:

- **Tiling alone opens the artist's document, with margin.** 1.42 GB of content,
  1.44 GB with the apron, ~1.6 GB after page quantisation, ~1.8 GB resting with
  the stroke scratch and the swapchain, ~3.9 GB with a float up and a blender
  loaded. Against 9 GB usable. Nothing else in the programme is needed for the
  GPU half.
- **Tiling is also the largest *throughput* win here, and `composite-throughput.md`
  does not rank it at all** — it hands memory layout to the sibling and never
  asks what an unbacked tile costs. A fragment skips a layer entirely where its
  tile is unbacked, so at corpus occupancy the fit-to-view composite's 6.5 GB a
  frame falls to roughly 0.9 GB. That is most of R7's headline, exactly, and
  R7 is the only item in the programme that is not bit-exact. §4.2.
- **Residency's CPU shadow costs 21.6 GB before tiling and ~1.4 GB after**, and
  its own staging puts it first. §3.1.

---

## 1. The order

Stages ship something. "∥" means the stages beside each other can run as
concurrent worktree agents; everything else is serial. File lists are for
planning the fan-out — overlapping files mean silent overwrites.

**In flight, not re-planned here:** the banded `write_layer_rect` with a submit
per band and a periodic poll; `thumbnail.wgsl`'s `MAX_TAPS` step. Stage 2 below
depends on the first landing; Stage 5's thumbnail rule depends on the second.

### Stage 0 — the loose allocation bugs. Serial, in the shared checkout, first.

Every one is prescribed by a doc comment or a withdrawn recommendation's
survivor. They all touch `canvas.rs`, so they cannot be fanned out and they
must precede everything that does.

| | what | files |
|---|---|---|
| 0a | `resize` rebuilds at the *old* canvas's slice count — thread `slot_capacity_needed()` in, copy `min(old, new)` (`slot-lifecycle` §5.4) | `canvas.rs`, `app.rs::apply_canvas` |
| 0b | `CanvasRenderer::for_document` takes the slot count, so an import allocates once instead of growing `n` times (`slot-lifecycle` §4.2) | `canvas.rs`, `app.rs::add_canvas` |
| 0c | `add_canvas` re-clears every slice `ensure_slots` has already cleared (`critique-allocation-formats` 14) | `app.rs` |
| 0d | `bake_effects` bakes hidden layers' effects into slices the composite discards — one predicate (`layer-residency` §2.2) | `canvas.rs` |
| 0e | Release the per-dab colour scratch and the effect working set when `slice_bytes(doc_size) > GROWTH_DOUBLING_BUDGET_BYTES` (`slot-lifecycle` §8.3) | `canvas.rs` |
| 0f | `effect_slot_base` reserves a canvas-sized slice for a float nobody started — take the float's spare from `EffectCache`'s free list (`slot-lifecycle` §5.3, second option) | `canvas.rs`, `editor.rs` |

0a–0e are mechanical. **0f needs a critic**: it moves the float's preview into
an allocator whose contract is "never handed to a layer, so no `PixelPatch` can
name one", and getting that wrong is the reissued-slot corruption.

### Stage 1 — refuse instead of crashing. Serial, after 0.

`try_reserve` with `critique-allocation-formats` finding 4's ordering — split
the texture creation out of `LayerStore::new`, pop and check **before any
`create_view`**, because a view of an error texture is a *Validation* error an
`OutOfMemory` scope does not catch. `memory_budget_thresholds` set **after**
`with_env`, `for_device_loss` left unset. Call sites at `install_import` and
`add_layer`, stated against `c + n` slices plus in-flight staging.

Files: `canvas.rs`, `gpu.rs`, `app.rs`. **Needs a critic** — the mechanism is
correct in every detail except one that makes it panic on first use, which is
exactly the shape that gets re-broken.

Before the atlas, deliberately: it is small, it protects the months in between,
and the atlas rewrites `LayerStore` around it.

### Stage 2 ∥ Stage 3 ∥ Stage 4 — three independent worktrees.

**Stage 2 — the piece contract** (`import-and-limits` §7.2). `ImportedLayer::pixels`
becomes `Vec<PixelPiece>`; the five readers yield rather than densify;
`install_import` loops pieces. Host peak falls from every layer to one piece,
and it subsumes trimming and streaming rather than sitting on them — all three
are changes to the same field and only one can be made.

**Claim the unowned half with it:** `check_bounds` must be re-derived against
pieces rather than `canvas × 4 × layers`. `tiled-layer-storage.md` §11 names
this and hands it to `docs/document-import.md`, which does not have it;
`import-and-limits.md` §4.3 argues it is retired and does not claim it. Without
it the artist's document is still refused at 17.2 GB no matter what the GPU
does.

Files: `crates/umber-core/src/docimport/{mod,clipstudio,krita,openraster,photoshop}.rs`,
`app.rs::install_import`. Touches `canvas.rs` **not at all**. Needs a critic.

**Stage 3 — the atlas.** `tiled-layer-storage.md` stage 1 (identity, nothing
observable) then stage 2 (sparse). Take the residency signal from block
*presence*, not from an emptiness scan — §3.2 and §4.1. This is the largest
piece of work in the programme and it owns `canvas.rs` and every shader for its
duration.

Files: `canvas.rs`, all eight `shaders/*.wgsl`, a new tile/residency module in
`umber-core`. Needs a critic on the diff **and** an independent reviewer on the
merge, per the rule that a merge commit is reviewed by nobody.

**Stage 4 — the host side of Save and the autosave** (`formats-and-host-memory`
§10.1). Encode each slice as it comes home and drop the raw buffer; stream the
archive to the temporary file rather than building a `Vec<u8>`. Peak falls from
N+1 canvases to one. This is 10 GB every five minutes, unattended, with no
quality trade anywhere in it, and it is the largest honest number in the
programme after the layer array itself.

Files: `crates/umber-core/src/docformat/mod.rs`, `umber-app/src/autosave.rs`,
`app.rs`'s save path. Needs a critic — `docformat::encode` gaining an
entry-at-a-time form is a real interface change.

### Stage 5 — the exact composite wins. After 3.

R1 (elide draws that contribute nothing, on `layer-residency` §2.2's merged
rule), R2 (scissor to the canvas region), R3 (per-frame device objects in
`probe_canvas` and `drive_thumb`), R4 (interleave the two uniform arrays).
Every one is byte-exact and each has a byte-equality guard in
`composite-throughput` §8.4.

After the atlas because R1 rewrites `Editor::layer_draws`, which the atlas also
touches, and because tiling adds a second reason a draw contributes nothing.
One culling rule in one place. `roadmap-review.md` §1.1 already records three
designs rewriting that function; this is the fourth and it goes last.

Files: `editor.rs`, `canvas.rs`, `composite.wgsl`. Mechanical apart from R1's
clipped-run rule, which needs the guard rather than a critic.

### Stage 6 — measure, then decide. Nothing is scheduled here.

Run `measure-composite.rs` **with a tiled column** and `measure-vram.rs`. Then
and only then decide R7 (the proxy), R5 (the screen cache), R6 (dirty regions)
and any part of `layer-residency.md`. §4.2 is why every one of them may turn out
to be unnecessary, and R7 is the only thing in the programme that can make the
picture worse.

---

## 2. The collisions nobody flagged

The reconciliations that *did* happen are real and I checked four of them rather
than assuming: tiling ↔ composite on the proxy versus a mip chain (§12.1 against
§4.5, agreed on one proxy array); tiling ↔ slot-lifecycle on the growth
transient (slot-lifecycle §10 corrected, tiling §11 records it); residency ↔
composite on the merged elision rule and the R5 cut position; import ↔
slot-lifecycle on the refusal's wording, adopted verbatim rather than
duplicated. Those are settled. What follows is not.

### 2.1 `write_layer_rect` has four live proposals and one head-on disagreement

Four call sites outside the tests, four different intents — an import upload
(`app.rs:4454`), an undo patch (`:5661`), a cut's write-back (`:1522`) and the
text tool's erase-and-replace (`:1938`) — and four documents change it:

| document | change |
|---|---|
| `tiled-layer-storage` §3.6 | scan each tile and skip one that is entirely the slot's empty value |
| `import-and-limits` §7.1 | **rejects that location**; put the scan in the importer |
| `import-and-limits` §8.1 | band it, submit per band |
| `layer-residency` §3.3 | make it mean "page in if needed, then write" |

The first two were written in the same round and neither knows the other took
the opposite position — import is arguing against the *critique's* fallback.

**Import is right, and on a stronger ground than the one it gives.** Its
argument is cost: undo pieces are recorded damage and known non-empty, so the
scan is wasted on every undo. The stronger argument is correctness. `app.rs:1938`
writes a union rectangle full of zeroes deliberately, with a comment saying so —
"Zeroes are a fully transparent premultiplied pixel, which is what clearing
means in this form" — to take the old text off before the new goes down. An
unconditional skip-if-empty leaves the old text on the canvas. The same holds
for an undo restoring a stroke's rectangle to transparency over a tile the
commit has already backed.

**The rule that is safe is: skip only where the tile is *unbacked* and the
incoming data is empty.** That is correct, cheap, and useful on exactly one
path — a fresh upload — which is where `import-and-limits` §7.1 puts it. Both
documents converge once the safe rule is written down, and §4.1 says why even
that is not worth building.

Whatever else happens, `write_layer_rect` cannot carry four meanings behind one
signature. It needs an explicit intent, in the shape `StrokeStyle::on_mask`
already takes: replacing a layer's content is not restoring a patch is not
paging a line in.

### 2.2 Residency's CPU shadow is the exact allocation the piece contract exists to delete

`layer-residency.md` §0 Stage 0 populates a per-layer CPU shadow from
`Opened::uploads` "instead of dropping it", and says keeping it "costs nothing
at import". `formats-and-host-memory.md` §8.2 and `import-and-limits.md` §7.2
both exist to make `Opened::uploads` never hold a canvas per layer at all.

They are opposites in the same currency, on the same path, and neither says so.
After Stage 2 there is nothing to keep; residency Stage 0 would have to
*re-materialise* 21.6 GB of host buffers, which is the peak Stage 2 just
removed.

Both are also independently right that Save is the problem —
`app.rs:3063-3071` holds every layer in RAM at once — and they reach it two
ways. **`formats-and-host-memory.md` §10.1(1) is the better half**: encoding
each slice as it comes home holds one canvas, where the shadow holds all of
them for the session to get the same result. So the "Stage 0 pays for itself
via Save" argument does not survive Stage 4 landing.

**The resolution is a sequencing one and it is large.** A shadow at *tile*
granularity, after the atlas, is 13.5% of the dense figure — 1.4 GB for the
motivating document rather than 21.2. So residency's Stage 0 is affordable
after tiling and is not affordable before it, and residency's own staging puts
it first. Move it, or build the shadow as a consumer of the tile store rather
than as a `Vec<u8>` per layer.

### 2.3 The mask change retires tiling's proudest claim, and the handover it accepted is stale

`formats-and-host-memory.md` §5.5 hands the mask work to the tiled store;
`tiled-layer-storage.md` §12.1 accepts it. Two things went wrong in the handover:

**The accepted requirement describes a design that has been withdrawn.** Tiling
§12.1 writes it up as "a tile carries a class — full RGBA for a layer, one
channel for a mask… a slice is unbacked only when all three channels are
empty". That is *packing*, which formats withdrew in the same round in favour
of a dedicated linear `R8Unorm` array. The real requirement on the allocator is
a tile of a narrower **class** holding **linear** coverage, whose claim is a
tag rather than a granularity — not a third of an RGBA tile.

**And the linear mask is a format change.** `tiled-layer-storage.md` §6.1 says
"This design earns no version bump of any kind, in either format, which by this
codebase's own standard is the strongest available evidence that it is a change
to how something is held rather than to what is held." `formats-and-host-memory.md`
§5.3 says the linear mask is an `umber-version` bump, and §5.4 says
`history::VERSION` +1. Build the mask work into the atlas as both documents
recommend and tiling's claim is false the day it lands.

Not a reason to refuse either. It is a reason to **keep them apart**: build the
atlas with masks exactly as they are, so §6.1's claim stays true and stage 1's
identity test means what it says, and take the linear mask afterwards as its
own change with its own two version bumps. That also isolates the one live loss
the programme found — `srgb::coverage_table` is non-injective and about 74 of
256 states of an imported mask are unreachable — which deserves its own commit
and its own guard rather than arriving inside a 2,000-line storage rewrite.

### 2.4 The proxy's apron and the atlas's apron were reconciled to a constant that the sizing rule contradicts

`tiled-layer-storage.md` §12.1 reconciles to "no chain on the atlas, one proxy
array beside it, **`A = 2`**", because generating a proxy tile *tile-locally*
needs `2^k` source apron for `k` reduction levels. But `composite-throughput.md`
§4.5 and `layer-residency.md` §2.5 both made `k` a **runtime rule** — from the
footprint, checked against a byte budget — landing on 2 or 3 depending on canvas
and view. At `k = 3` the reconciled `A = 2` is insufficient, and `A = 8` costs
12.6% of the atlas rather than 3.15%.

**Neither figure is needed.** A proxy tile covering exactly one source tile's
interior is `256 / 2^k` texels a side, and 256 is divisible by `2^k` for every
`k` anybody would choose — so every proxy *interior* texel is a reduction of
source *interior* texels alone, and no source apron is read. The proxy's own
apron is then filled by the same neighbour-copy the layer apron uses, at proxy
resolution.

**So `A = 1` stands, `k` stays a runtime rule, and the proxy grows a second
apron refresh rather than the atlas growing a wider one.** Say this at the
constant, because "`A = 2` because of the proxy" is the kind of reconciliation
that is quoted for years after the reason has evaporated.

### 2.5 `CompositeParams` acquires four fields that each argue separately for having no default

The LOD (`layer-residency` §0, `composite-throughput` §4.2), the proxy binding
(§4.2 again), the seed binding and `first_draw` (§5.5), and `baked_below`
(`layer-residency` §5.1). Each cites `Background`'s precedent independently;
none knows about the other three. The five reuse paths — `export_rgba`,
`pick_colour`, `pick_patch`, `probe_canvas`, the autosave capture — would each
have to state four things correctly, which is the "forgotten at the sixth"
failure with four chances to happen.

**One field, not four.** `export: bool` is already the seed of it. A
`Fidelity::Exact` that binds no proxy, names LOD 0, seeds nothing and bakes
nothing, against a `Fidelity::Screen { .. }` carrying whatever the screen path
wants, makes the five reuse paths say one word each and makes a new fidelity
knob a compile error at all five. Decide this before any of Stage 6 is written,
or it cannot be retrofitted without touching all five again.

### 2.6 `touch_slot` is changed by one document and depended on by four

`tiled-layer-storage.md` §8.3 widens it to `touch_tiles(slot, region)` and moves
`render_float` to `&mut self`. Its consumers are `Thumbs`, `CachedEffect::mask_revision`,
`composite-throughput.md` R6's invalidation predicate, and `layer-residency.md`
§3.4's shadow — which relies on the enumeration of writers being *complete*, a
claim CLAUDE.md already records as having been false for exactly one method.
Four consumers, one signature, one enumeration. Nobody lists the set. Whoever
does §8.3 owns re-checking all four.

### 2.7 Copy-on-write tiles were handed over and dropped

`formats-and-host-memory.md` §9 raises them, argues they would collapse an undo
entry from a pixel copy to a table of tile handles, and hands them to the tiling
design. `tiled-layer-storage.md` §6 does not take them up; §6.1 records only
that "an undo that writes transparency into a backed tile leaves it backed.
That is waste, not damage."

A full-canvas wash's undo patch is 400 MB, the 512 MB budget holds exactly one,
and CLAUDE.md already names "a patch that stores tiles" as the standing fix.
This is plausibly the largest unexploited win in the programme and it fell
between two documents. Not for Stage 3 — it changes the tile's ownership model
and the atlas is large enough — but it should be **designed while the atlas is
being built**, by whoever is building it, because retrofitting reference counts
onto a tile allocator is not a small change.

---

## 3. Adjudications

### 3.1 Screen-space cache versus canvas-sized bake — build neither now

The composite author is right that the screen cache is bit-exact where a
canvas-sized bake carries a resampling error at every screen zoom that no format
removes. The residency author is right that the screen cache is keyed on the
screen camera, so a pan misses it every frame and it buys nothing for residency
in the case residency is hardest. Both kept their positions and both are
correct.

**The condition that decides is whether residency binds at all after tiling, and
the survey says it does not.** The canvas bake's only remaining advantage is
reducing residency during a pan; at 1.42 GB there is no residency problem to
reduce. So the bake's stated revival condition — "if measurement after stages
0–3 shows layer count still binding, and panning at working zoom is the case
that breaks" — is now very likely false, and should be re-tested rather than
assumed either way.

**One thing neither author said, and it is the useful half.** R5, R6 and the
canvas bake are *all* invalidated by camera motion. Nothing in the programme
makes panning cheaper except the proxy and tiling's own skip. If panning at
working zoom turns out to be the complaint, the three cache designs are the
wrong drawer to look in.

### 3.2 `R7` mips at ×1.0 versus ×1.25 — the composite author is right

The critique reported both halves and banked the ×1.0 as R7's win being
"slightly better than stated" while its own next sentence required
`MipmapFilterMode::Linear` in the same commit. The composite author's objection
is exact: the two cannot both be true of the shipped design. `Nearest` pops at
every octave of a continuous zoom, which this project would refuse in any other
control. Take `Linear` and ×1.25.

I would add one thing. Under §4.2 the proxy's *bandwidth* justification largely
evaporates once tiling ships. What is left is the aliasing, which is a quality
feature — and a quality feature that popped at every octave would be absurd. So
the argument for `Linear` gets stronger, not weaker, as R7's other reason goes
away.

### 3.3 The residency signal comes from block presence, not from a scan

The survey measured the gap the whole scan argument was about: presence
over-reports contents by **1.13× at corpus scale and 1.58× on the worst
document**. At 6.5% coverage that takes the motivating document from 1.42 GB to
2.24 GB — still comfortably inside the card. So the emptiness scan is worth
about 13% of a small number, and both its proposed homes cost more than that.

**Take residency from presence. Do not build the scan in either place.** Keep
`tiled-layer-storage.md` §9.5's reclamation, which recovers the same waste later
and is needed anyway for the atlas-full path.

### 3.4 The general shrink and mask packing stay withdrawn, and one more should join them

`slot-lifecycle-and-vram.md` withdrew its shrink; `formats-and-host-memory.md`
withdrew packing. Both withdrawals are right and I checked the reasoning behind
each. The third that should join them is `tiled-layer-storage.md` §3.6's
emptiness scan — §2.1 above.

---

## 4. What should not be built

Beyond the two already withdrawn:

- **The emptiness scan inside `write_layer_rect`.** §2.1 and §3.3. Wrong on
  three of its four callers and worth 13% of a figure that is already small.
- **`A = 2` on the atlas apron.** §2.4. Unnecessary and, at `k = 3`,
  insufficient — a constant fixing a problem that does not exist.
- **Any part of `layer-residency.md` in its current staging.** Stage 0's shadow
  is the piece contract in reverse (§2.2); Stage 1's headline — hidden layers
  hold no VRAM — is worth whatever fraction of a stack is hidden, which is
  *still* nobody's measurement, and delivers a fraction of a figure tiling has
  already reduced by 7.1×. Stage 2 (background tabs) survives and is small.
  Stage 3 is R7. Stage 4 is compression of a shadow that should not exist yet.
- **Releasing a background tab's layer array** — `slot-lifecycle` §9.4 declines
  it with five seconds of transfer, and the critique calls that optimistic.
  Endorsed.
- **Host paging**, BC7 or any lossy proxy, greyscale detection, indexed layers.
  All four are refused with reasons that survive review, and the eviction-
  dominance argument behind the BC7 refusal is the strongest of them.
- **R8**, the hand-written Normal path in the layer loop. It changes a pixel in
  the last bit, reaches an export and an eyedropper, and buys ALU on the one
  machine class this programme is not about.
- **A second `measure-*` example for anything `measure-vram.rs` already covers.**
  Three documents asked for overlapping sweeps; `slot-lifecycle` §11 owns it and
  the others defer correctly. Write it once.
- **The canvas-sized bake**, until §3.1's condition is re-tested and found true.

---

## 5. The quality contract

The overriding requirement is that stroke quality, layer fidelity and the
rasterised image stay pristine. This is every place in the programme where a
pixel could move, consolidated. Hold implementers to it.

| # | Where a pixel could move | What prevents it | The guard |
|---|---|---|---|
| Q1 | **Bilinear across a tile boundary** — the physical neighbour of a tile's edge texel is an unrelated tile, so an unguarded tap is a one-texel line on a 256 grid | A one-texel apron holding the *logical* neighbour's edge texels. Sufficient at LOD 0 because a bilinear tap reads a 2×2 neighbourhood and there are no mips and no anisotropy | A test on the sampler descriptor: `anisotropy_clamp == 1`, `mip_level_count == 1` on the atlas, worded as "the apron is `A` texels because the filter reaches `A`". Today the sufficiency is incidental and nothing pins it |
| Q2 | **A stale apron** — a seam appearing at some zooms on some layers, unreproducible | Mark with the writer (`touch_tiles`, no GPU), refresh once on the frame path (`refresh_aprons`). Omission is then loud and universal rather than quiet and occasional | A debug-mode pass recomputing every backed tile's apron from its neighbours' interiors and asserting equality, run after every operation in the GPU suite. **If it cannot be made to pass, take the hand-lerp fallback** — `tiled-layer-storage` §13.13 |
| Q3 | **A half-texel shift over the whole picture** — `origin` must be the tile's *interior* corner | Nothing structural; it is arithmetic | `a_tiled_layer_composites_byte_for_byte_as_a_dense_one_did`, on content where coverage is only ever 0 or 1. **Prove it by mutation**: shift `origin` by one texel and watch it fail |
| Q4 | **A mask's absent tile read as zero** rather than white | The empty value is per slot *class*, decided by a `select` against the sentinel, never a shared blank tile | A GPU test that adds a mask, writes nothing to it, and asserts the layer is fully revealed. This is the same bug `clipstudio.rs` records fixing on the import side |
| Q5 | **The wet stroke not previewing on a blank layer** — an early `continue` for an unbacked tile skips the `i == active_index` block, so the stroke appears only at pointer-up | Write the skip as a `select` on `lay`, after both the stroke block and `clip_alpha` | A GPU test: paint on an empty layer, read the composite mid-stroke |
| Q6 | **A blended commit against uninitialised memory** — a piece over an unbacked tile has nothing to copy from, and Multiply against garbage is a stroke with garbage in it | Clear the backdrop to the empty value before the copies. Today every texel is overwritten and no clear is needed | A GPU test committing a Multiply stroke onto a blank layer |
| Q7 | **`fs_blend`'s backdrop indexed at the wrong offset** — `rect_min` must stay the *piece* origin when the vertex shader learns about tiles | The two are equal today and diverge the moment a piece is split | A blended commit spanning two tiles, compared against the same stroke on a dense layer |
| Q8 | **The sRGB view trap in any reduction** — `raw_slot_views` exists, is per-slice, and is what somebody writing a downsampler reaches for. Averaging encoded bytes takes black and white to linear 0.214 where the answer is 0.5 | One generator, through **sRGB** views, used for the proxy's level 0 and every level of its chain. A separate proxy array does not escape it | A CPU test: reduce two texels of linear 0 and linear 1, assert encoded **188** and not 128. Fails the day somebody reaches for the linear views |
| Q9 | **The pointer-up jump** — layer mips or a proxy with an unmipped stroke scratch means the preview aliases and the committed mark does not. Both passes still implement identical blending maths and the stroke still jumps | **Layer mips require scratch mips.** They are one change and costing them separately understates R7 | A GPU test comparing the last preview frame against the committed result at fit-to-view. This one is not covered by the "identical maths" rule and never was |
| Q10 | **`probe_canvas` reading a reduction** — it composites at `zoom = 8 / (radius × 2)`, so a 30-pixel brush probes at 0.133. A camera-derived LOD would change what a smudge picks up, which is document pixels | The LOD and the proxy binding are `CompositeParams` fields with **no default** — §2.5's single `Fidelity` field | Fill the proxy with solid magenta and assert `export_rgba`, `pick_colour`, `pick_patch`, `probe_canvas` and the autosave capture never see it. Write it *before* the proxy |
| Q11 | **The smudge probe's existing undersampling must not be "fixed" as a side effect.** Its own comment claims an area average and it takes a four-tap bilinear of a 60-pixel footprint | Nothing. It is a real open question about document pixels and changing it changes every existing document | Recorded, so that the day somebody notices, the answer is not "the proxy work did that" |
| Q12 | **A canvas-sized bake resamples the fold** where the unbaked path folds resamples. Independent of the format; not fixed by 16 bits | Not built. §3.1 | — |
| Q13 | **The linear mask changes the file** — an older build reading a version-4 file shows every mask at the wrong gamma, which is *wrong* rather than plainer | `umber-version` bump **and** `history::VERSION` bump, taken as their own change and not inside the atlas (§2.3) | `saving_and_reopening_does_not_move_a_pixel` extended over masks, with the round trip exact over all 256 states rather than the ~182 that survive today |
| Q14 | **The flip must stay an exact texel permutation**, since undoing a flip is another flip | Two exact permutations: mirror the page table on the CPU, mirror each backed tile in place, regenerate aprons rather than mirroring them | `a_flip_mirrors_the_canvas_and_flipping_twice_restores_it_exactly`, unchanged |
| Q15 | **A commit that cannot allocate a tile**, arriving after the undo patch has been read with the artist's hand off the pen | Refuse a *new stroke* when the atlas is within a threshold of full, at `LayerStack::refusal_at`'s gate. Then reclaim and retry; then grow through a catchable path; then keep the stroke in the scratch and say so | A test driving the atlas to full and asserting the refusal comes before the pen goes down, not after |

Two standing rules that apply to all of it: **no wall-clock assertion on CI**,
and `UMBER_TEST_SOFTWARE=1` over the whole of `umber-render` before any
byte-equality test is believed — hardware and lavapipe do not round identically
and a byte-equality test is exactly the shape that finds out the hard way.

---

## 6. What the programme missed

The six angles were chosen before the survey existed and two of them were sized
against a world it has now changed.

- **Nobody costs the whole programme against the artist's document.** Each
  document ends at its own boundary and says "this does not on its own open it".
  Assembled, tiling plus the piece contract plus a re-derived `check_bounds`
  does, with margin, and nothing else is needed for it. That sentence is worth
  more than any individual finding and it exists in none of the nine files.
- **Nobody asks what tiling does to frame time.** §4.2. `composite-throughput.md`
  hands memory layout away in its second paragraph and then ranks R7 — the one
  inexact item — as the only fix for the case it calls broken. The atlas is a
  competitor for that rank and it is exact.
- **Copy-on-write tiles and undo.** §2.7. Handed over, dropped, and probably the
  largest remaining win.
- **The autosave's *assembly* survives tiling.** `tiled-layer-storage.md` §7
  correctly says `begin_capture` gets much cheaper — unbacked tiles are
  synthesised with no copy — and the host side is still one canvas-sized `Vec<u8>`
  per slot. Tiling fixes the readback and not the 10 GB. Stage 4 is still needed
  afterwards, and neither document says so.
- **Nobody asked what happens with two large documents open**, which is
  ordinary. Residency §2.4 and slot-lifecycle §9.3 each answer part of it from
  their own side; nothing states the combined resting figure.
- **The small-document penalty was never checked against real files, and the
  survey settles it favourably.** Tiling costs a genuinely dense layer 5.2%
  more. The survey found every document above 40% occupancy is small — so the
  penalty lands where 5.2% is a few hundred kilobytes, and the saving lands
  where it is gigabytes. Worth one sentence in `tiled-layer-storage.md` §4.2,
  which currently states the penalty with nothing to bound it.
- **Still nobody's measurement: how much of a real stack is hidden.** Three
  documents rest a ranking on it and `survey-documents.rs` already walks the
  files. It is a small addition and it is the only thing that could move
  `layer-residency.md` Stage 1 back up the list.

## 7. Unowned

- **Re-deriving `check_bounds` against resident bytes.** Assigned to Stage 2
  above; it was in nobody's backlog.
- **`write_layer_rect`'s intent parameter.** §2.1. Four documents change the
  function and none owns its contract.
- **The `Fidelity` consolidation on `CompositeParams`.** §2.5. Must be decided
  before Stage 6 is written.
- **Re-checking `touch_slot`'s four consumers** after §8.3 widens it. §2.6.
- **Getting a log from the artist's machine.** Every document has an open
  question that one `RUST_LOG=umber_render=info` run and one `survey-documents`
  pass would answer — which backend is in force, whether the symptom is at open
  or on adding a layer, whether the driver is paging or failing. Nobody owns
  asking.
