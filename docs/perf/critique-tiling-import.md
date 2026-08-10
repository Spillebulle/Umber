# Critique: `tiled-layer-storage.md` and `import-and-limits.md`

A review of the two documents together, because they are paired by a dependency
neither of them owns. Everything below was checked against the code rather than
against the prose; where I could not settle something I say so and name what
would.

Both documents are good. They are careful about what they have not measured,
they refuse the obvious wrong answers with reasons, and they are honest about
their own limits — `tiled-layer-storage.md` §11 in particular is the kind of
"what this does not fix" section that makes a design worth trusting. The
findings below are what is left after that, and the two blocking ones are both
about the **join** between the documents rather than about either document's
interior.

---

## Verdict, up front

**Do the two proposals compose? No — not as written, and the gap is at the one
place both authors thought the other had covered.** Neither document specifies
a way for the importer to tell the layer store *which tiles hold content*.
Without it, the atlas is built and every layer is fully backed, so stage 2 saves
nothing on an import — which is every large document Umber will ever meet.
Finding 1 is that in detail; finding 2 is a second, independent conflation
inside `tiled-layer-storage.md` §9.1(4).

**The single thing I would most want changed:** make the residency-carrying
upload path an explicit, named deliverable owned by one of the two documents,
with its signature written down. Everything else here is repairable in a
paragraph; this one is a hole in the middle of the plan that reads, from either
side, like somebody else's paragraph.

---

# A. Composition

## 1. BLOCKING — Neither document delivers a residency signal, and both assume the other does

**What I checked.** `crates/umber-app/src/app.rs:4448-4466` (`install_import`'s
upload loop), `crates/umber-render/src/canvas.rs:7180-7218`
(`write_layer_rect`), `tiled-layer-storage.md` §7 and §9.3,
`import-and-limits.md` §7.2 and §7.3.

**What I found.** The two documents describe three different changes and treat
them as one:

| | what it produces | host peak | tile residency |
|---|---|---|---|
| import §7.2 "stream the import" | one canvas-sized dense buffer at a time | one layer | **none** |
| tiling §9.1(4) "trim the importer" | `pixels` as a rectangle + origin, all layers still held | see finding 2 | a bounding box only |
| import §7.3 "if tiles exist" | one 256-block written into one tile | one tile | exact |

`tiled-layer-storage.md` §9.3 says residency is decided "on load from what the
importer found". Nothing in `import-and-limits.md` §7.2 produces a "what the
importer found" — it produces a dense canvas-sized `Vec<u8>` per layer, exactly
as today, one at a time. And `tiled-layer-storage.md` §7's own table says
`write_layer_rect` becomes "allocate, then one `write_texture` per tile". Today
`install_import` calls it with `PixelRect { x: 0, y: 0, width: size.x, height:
size.y }` — the whole canvas, verified at `app.rs:4457-4463`. So under stage 2,
opening a document backs **every tile of every layer**, and the atlas costs
exactly what the dense array costs.

Conversely, `import-and-limits.md` §7.3 assumes an API the sibling never
promises: "a stored block inflates straight into a tile and is uploaded; an
absent block allocates and uploads nothing" is a `write_layer_tile(slot, tile,
&bytes)` or equivalent. It is not in `tiled-layer-storage.md` §7's table, which
is the table written so "nobody discovers the eighth one late".

So each author is relying on a change the other did not undertake, and the
symmetry is what made it invisible: tiling waves at the importer, the importer
waves at tiling, and the deliverable is in neither backlog.

**What settles it.** One of the two documents has to own the interface and write
its signature down. The two candidate shapes, both worth stating so the choice
is deliberate:

- **A tile-wise upload.** `write_layer_tiles(slot, &[(TileCoord, &[u8])])`, fed
  directly by `csblocks::for_each_block` for a `.clip` and by a shredder for
  ORA/PSD. This is the version that gets the whole win — no canvas buffer at any
  point for a `.clip`, per `formats-and-host-memory.md` §8.3, which makes the
  same argument.
- **A CPU emptiness scan inside `write_layer_rect`.** Keep the dense upload, and
  have the store skip a tile that is entirely the slot's empty value. This
  composes with §7.2 streaming *without a new API*, costs one extra pass over a
  buffer the importer already traverses twice (`import-and-limits.md` §3 counts
  the blit and `srgb::encode_buffer` as two full traversals), and it throws away
  the free residency `.clip` already carries. It is the cheap join and it should
  be named as the fallback rather than discovered later.

Until one of them is written down, `tiled-layer-storage.md` §9.3's "on load from
what the importer found" is a promise with nothing behind it.

## 2. BLOCKING — `tiled-layer-storage.md` §9.1(4) conflates trimming with streaming, and its headline figure needs the half it does not propose

**What I checked.** `crates/umber-core/src/docimport/mod.rs:179-199`
(`ImportedLayer`, `pixels`, `mask`), `:626` (`validate`), `app.rs:4448`.

**What I found.** §9.1(4) says: "Making `pixels` a rectangle plus an origin
takes the host peak from 21.6 GB to the size of the largest layer". That is
false. Trimming changes what *one* layer costs; it does not change how many are
alive at once. `ImportedDocument::layers` is a `Vec<ImportedLayer>` built
complete by every reader, `open()` moves each `pixels` into a `LayerUpload`, and
`install_import` then iterates `for upload in &uploads` — a borrow, so all of
them stay resident until the loop ends. Bounding the peak to one layer requires
**streaming**, which is `import-and-limits.md` §7.2 and which §9.1(4) does not
mention.

Two consequences:

- On the artist's document, trimming alone is worth an unknown amount and
  possibly close to nothing. A `.clip` layer's bitmap is its own rectangle and
  `docs/document-import.md`'s own measurement over 5,438 bitmaps records a worst
  **area of 15.93× the canvas** — layers routinely reach well past the page. A
  trimmed layer clipped to the canvas is at most canvas-sized and at worst
  exactly that, and one full-page paper or background layer is enough to keep
  the peak where it was. §9.1(4) calls this "the single highest-value item on
  the list" and prices it with streaming's figure.
- The masks are not in the arithmetic either. `ImportedLayer::mask` is
  documented at `mod.rs:194-196` as "canvas-sized and in the same form as
  `pixels`" — another canvas × 4 per masked layer, which `check_bounds`
  deliberately does not count (`import-and-limits.md` §4.2 gets this right and
  §9.1(4) does not carry it across).

**The repair is a paragraph:** §9.1(4) should be split into "trim" and "stream",
credit `import-and-limits.md` §7.2 for the second, and state that the peak
figure belongs to the second. As it stands the tiling document's own §11 —
"§9.1(4) is the fix for the second" — is resting on it.

## 3. SUBSTANTIVE — What the two together actually buy, traced

Since the composition question is the brief's first, here is the trace, so the
answer is not left implicit:

- **Importer trimmed first, atlas never built.** Coherent, and worth having: the
  host peak falls by whatever the trim is worth (unmeasured, finding 2), and
  `MAX_TOTAL_BYTES` starts to be a bound on something real. The GPU cost is
  unchanged — 21.6 GB of dense array — so the artist's document still cannot
  open. Nothing breaks.
- **Atlas built first, importer not trimmed.** Stage 1 changes nothing
  observable by construction. Stage 2 changes nothing *at all* for an imported
  document, per finding 1: the upload backs every tile. It does help documents
  the artist paints from blank in Umber, and it makes `clear_layer`,
  `fill_layer_white` and `clear_all_layers` free, which are real. It does not
  address the reported problem.
- **Both, with the residency signal from finding 1.** This is the combination
  that works, and it is the only one that opens the document.

So the answer to "does the smallest useful increment save a byte" is: only with
a third piece. `tiled-layer-storage.md` §9.3's claim that "Stages 1 and 2
together are the smallest increment that saves a byte" is true for painting and
false for opening.

---

# B. `tiled-layer-storage.md`

## 4. BLOCKING — The apron refresh cannot hang off `touch_slot`, and the failure it is being hung on is the one CLAUDE.md already recorded

**What I checked.** `canvas.rs:5438-5447` (`touch_slot`), `:5449-5455`
(`touch_all_slots`), every call site (`:3285, 3932, 3986, 4229, 4244, 4271,
4318, 4771, 4798, 7189`), and `:4809-4895` (`render_float`).

**What I found.** §8.3 proposes making the apron refresh structural by hanging
it off `touch_slot`, on the strength of CLAUDE.md's claim that
`slot_revision` "is bumped inside every method that writes a slice". Three
things are wrong with the premise:

1. **`touch_slot` has no device, no queue and no encoder.** Its signature is
   `fn touch_slot(&mut self, slot: u32)` and its whole body increments a counter
   and abandons a thumbnail. An apron refresh is GPU work —
   `copy_texture_to_texture` between tiles, recorded into an encoder. Rewriting
   it as `touch_tiles(slot, rect, encoder)` is not "the rule stated once at a
   narrower granularity"; it is a signature change at ten call sites, two of
   which (`touch_all_slots` from `resize` at `:3285` and `clear_all_layers` at
   `:4271`) have no rect to give it.
2. **`render_float` takes `&self` and writes a slice; its two callers do the
   touching.** `draw_float` (`:4770-4771`) and `commit_float` (`:4797-4798`)
   each call `self.render_float(...)` and then `self.touch_slot(...)`. That is
   *precisely* the "enforced at N call sites" shape, and it is the exact method
   CLAUDE.md records as the one the "exhaustive by construction" claim was false
   for. §8.3 quotes that correction and then builds on the claim anyway. The
   `&self` receiver is not incidental: it is why the bump could not be inside
   the method in the first place, and it would have to change for the apron.
3. **The ordering is only accidentally right.** In `draw_float`,
   `touch_slot` runs after `render_float` has recorded its pass into the
   caller's encoder, so an apron copy appended there would be correctly ordered.
   Nothing enforces that; a future writer that touches before it draws produces
   an apron refreshed from stale interiors, which is the stale-apron bug wearing
   a green test suite.

**This is the design's own §13.13 firing.** "An apron maintained by discipline —
if it cannot be hung off the `touch_slot` hook and verified in debug builds,
take refusal 7 instead." The hook, as it exists, cannot carry it. The document
should either (a) redesign the write surface so every slice write goes through
one function that owns an encoder — which is a real and defensible change, and
would incidentally fix the `render_float` gap CLAUDE.md complains about — or
(b) take refusal 7.

**On refusal 7 (manual bilinear from four `textureLoad`s), which §8.1 dismisses
too quickly.** Its two objections are "eight fetches per layer per fragment
instead of two" and "a second implementation of bilinear filtering in the hot
path". The first is real but is stated against the wrong baseline: the design's
own §4.2 argues the tiled composite is *bandwidth*-bound and expects to win by
skipping unbacked layers, so a fetch count is not obviously the binding
constraint — and four `textureLoad`s of adjacent texels in one tile hit the same
cache lines a bilinear tap does. The second is the weaker objection: the four
loads all resolve through **the same page table entry** whenever the sample sits
strictly inside a tile, which is 255/256 of the interior, so the common path is
one table read and four loads, not four table reads. And a hand-written lerp is
not "a second implementation of filtering" in the sense `blend.wgsl` refuses —
there is no other copy for it to drift from, because the hardware sampler is not
a text this codebase maintains.

I am not saying refusal 7 is right. I am saying the document ranks it below an
apron whose structural guarantee does not exist, and the ranking should be
redone with that corrected. **A one-texel seam appearing at some zooms on some
layers is exactly the class of defect the artist's "must stay pristine"
constraint forbids**, and it is unreproducible.

## 5. BLOCKING — "Pages are allocated one at a time" reintroduces the growth policy `grown_capacity` measured and rejected

**What I checked.** `canvas.rs:540-628` (`grown_capacity`'s doc comment and
body), `:629-650` (`growth_quantum`), `:3149-3213` (`ensure_slots`).

**What I found.** §3.1 says "Pages are allocated one at a time, so growth is in
68 MB steps rather than `grown_capacity`'s 400 MB ones — which is a second,
quieter benefit". The atlas is a `texture_2d_array` of pages, because the
composite must index pages from a loop (§2.2's own argument). Growing a
`texture_2d_array` means creating a new one and copying, with the old alive —
which is what `ensure_slots` does at `:3172-3212`, verified.

`grown_capacity`'s doc comment records that exact growth was **measured and
refused**: "a 2048² document going from 16 slices to 128 one layer at a time
[…] is 112 growths and 134 GB copied. […] The 112 separate requests for a fresh
multi-gigabyte texture with the old one still live are not [fine]: a
`create_texture` failure there is an uncaptured device error, and therefore
fatal."

One page at a time is the same policy at 6× the frequency. Filling the artist's
document takes on the order of 300 pages; growing one at a time copies
`68 MB × n(n+1)/2` ≈ **3 TB** in aggregate and makes ~300 multi-gigabyte
allocation requests with the old atlas live. That is strictly worse than what
the codebase already rejected, and §3.1 presents it as a benefit.

Nothing about the tile design forces this — the fix is that the *atlas* answers
to `grown_capacity`'s budget rule exactly as the layer array does, and pages are
allocated one at a time from a **capacity** that grows in quanta. The document
should say so, and the sentence about 68 MB steps should be about the
*allocation granularity of a tile*, not about the texture.

This also refutes `slot-lifecycle-and-vram.md` §10, which lists §4's growth
transient as "retired" by tiling on the grounds that "a tiled layer grows by
adding tiles, and there is no monolithic array to reallocate". There is; it is
the atlas. **Two siblings currently disagree about this and neither knows it.**

## 6. SUBSTANTIVE — §7's table is wrong about the float, and stage 2 has no answer for the preview slot

**What I checked.** `canvas.rs:4605-4735` (`begin_float`), `:4759-4776`
(`draw_float`), `:4809-4895` (`render_float`).

**What I found.** §7's table says `begin_float`, `render_float`: "stage 1:
unchanged, still canvas-sized, still 1.2 GB at this canvas." They are not
unchanged. `begin_float` performs three whole-canvas `copy_texture_to_texture`
operations against `self.layers.texture` — layer slice → base (`:4610-4632`),
layer slice → floating (`:4634-4660`), base → preview slice (`:4707-4732`) — and
`render_float` restores out of `base` into `self.layers.texture` at a slot
origin (`:4824-4849`) and then renders into `self.layers.slot_views[slot]`
(`:4880-4894`). Under an atlas none of those is a single copy and none of those
views exists. This is the eighth path the table was written to prevent
discovering late.

Worse, in stage 2: the preview slot is written by a whole-canvas copy of `base`,
so it must be **fully backed** — 1,580 tiles, 400 MB of atlas — the moment
somebody presses T. And that is not a bug in the design so much as an unstated
requirement: a float's preview slice has to hold whatever the layer holds *plus*
wherever the picture has been dragged to, so its residency is the layer's union
the destination rect. §9.4 defers `base` to a copy-on-write page table and says
nothing about the preview.

## 7. SUBSTANTIVE — "A piece is at most one `damage::TILE` row tall" is false for a cut and for a loaded history

**What I checked.** `crates/umber-core/src/damage.rs:190-193` and
`app.rs:5658-5660`.

**What I found.** §6.2 and §6.3 both rest on "A piece is at most one
`damage::TILE` row tall — 64 pixels", from which §6.3 derives "Every piece is
within one storage-tile row, so no readback ever spans tile rows."

`TileMask::pieces` opens with:

```rust
if self.cells.is_empty() {
    return vec![rect];
}
```

and its own doc comment says why: "a patch built from a file or by a test has no
mask at all and must still describe itself". CLAUDE.md states the live case
explicitly — "**The cut's patch is the rectangle, not cells.** There is no
`TileMask` to have accumulated one from" — and `app.rs:5658` feeds
`patch.pieces()` straight into `read_layer_pieces` on the undo path. A revision-1
saved history is the same shape: one piece covering the rect.

So a full-canvas piece reaches `read_layer_pieces` today, and under tiling it
spans every tile row. The existing code already falls through to the banded
reader for a piece too large (`canvas.rs:7010`), so this is not fatal — but §6.3
lists "no readback ever spans tile rows" as one of the three things the
divisibility assertion *buys*, and it does not buy that. The claim should be
narrowed to stroke-derived pieces, and the cut path should be named.

## 8. SUBSTANTIVE — The atlas holds *less* than the dense array on a `downlevel_defaults` device

**What I checked.** `crates/umber-render/src/gpu.rs:73`
(`Limits::downlevel_defaults().using_resolution(adapter.limits())`),
`canvas.rs:88-99` (`MAX_SLOTS`), §3.1's own arithmetic.

**What I found.** §3.1 works the floor case: "On a machine reporting only the
guaranteed 2048 the page is `258 × 7 = 1806` and holds 49 tiles". Carried to its
conclusion, the whole atlas then holds 256 × 49 × 65,536 = 822 Mpx, **3.29 GB**.
The dense array on the same device holds 256 × 2048² = 1,073 Mpx, **4.29 GB**.

So on exactly the device class `downlevel_defaults` exists to protect — a mobile
GPU, which `docs/mobile.md` is about and which §12 says "benefits" — the atlas
is a 23% *reduction* in total document capacity. It is probably not reachable
today (64 layers at 2048² is 16 slices' worth of tiles), but it is a ceiling
moving downwards and the document does not notice it. §3.1 should carry the
floor-device capacity figure beside the 17.18 GB one.

Related and smaller: §3.1 says the page is 16 tiles a side "because a page must
be no larger than `max_texture_dimension_2d`". That constrains it to *at most*
127 a side on the RTX 3080's Vulkan 32768; it does not explain 16. Either give
the real reason or say it is a free parameter to be measured.

## 9. SUBSTANTIVE — §12 lists no collisions with the other four `docs/perf/` siblings, and at least two are material

**What I checked.** `ls docs/perf/` and the headings of each; then
`composite-throughput.md` §0 and §4, `slot-lifecycle-and-vram.md` §4, §7, §10,
`formats-and-host-memory.md` §8.

**What I found.** §12 reviews `group-compositing.md`, `roadmap-review.md`,
`layer-effects.md`, `structural-undo.md` and `mobile.md` — all outside
`docs/perf/` — and none of the four documents sitting in the same folder. Three
collisions:

- **`composite-throughput.md` R7 proposes mip levels on the layer array**, and
  calls them "the only thing that fixes zooming out", at ~9× the bandwidth of
  the fit-to-view composite. `tiled-layer-storage.md` §13.11 refuses mips.
  These are not merely different priorities: **a one-texel apron is only
  sufficient at LOD 0.** A mipped atlas needs an apron of `2^levels` texels, or
  a per-tile mip chain that is not a mip of the layer at its boundaries — which
  §13.11 says, without noticing that the sibling's headline zoom-out fix is
  thereby foreclosed. Whichever way this goes, it is a decision, and it is
  currently being made by whichever document lands first.
- **§4.4's "cull whole layers on the CPU per frame" is `composite-throughput.md`
  R1**, already designed there with its own subtle rule. Two documents proposing
  one change is how the two end up disagreeing about the edge case.
- **§9.1(1)'s `resize` slice-count fix and §9.1(3)'s colour-scratch release are
  `slot-lifecycle-and-vram.md` §4/§5.** Correct in both places; owned in
  neither.

I verified the sampler while checking the mip question: `canvas.rs:2340-2349`
sets `anisotropy_clamp` to its default of 1 and `mipmap_filter: Nearest` with no
mip levels, so the "bilinear taps a 2×2 neighbourhood at every scale" argument
in §3.5 and §8.1 **is correct today**. It is correct *because* of two settings
nothing pins. A `const` assertion or a test on the sampler descriptor would make
the apron's sufficiency structural rather than incidental — that is the cheap
version of the guard §8.3 is reaching for, and it is one worth having whichever
way finding 4 goes.

## 10. SUBSTANTIVE — What happens when the atlas is full is not designed, and it happens at pointer-up

§9.3 disposes of it in a clause: "what changes is the initial state and the
allocator being allowed to say no." A commit that cannot allocate a tile arrives
*after* the undo patch has been read (§5.2 step 2), with the stroke on screen and
the artist's hand off the pen. Dropping the paint silently is unacceptable;
CLAUDE.md's standard is that a refusal names the bound. There is no obvious good
answer — which is exactly why it needs a paragraph rather than a clause. §9.4
defers reclamation on the grounds that "not reclaiming is never worse than
today", which is true of *memory* and false of this: today a stroke on a
canvas-sized slice cannot fail to find storage.

## 11. SUBSTANTIVE — Block presence over-reports residency, and §3.3 oversells it

**What I checked.** `crates/umber-core/src/csblocks.rs:29` (`BLOCK == 256`),
`:286-310` (`for_each_block`), `:330-372` (`decode_block`, `Some(None)` for
`present == 0` before any inflate).

**What I found.** Everything §3.3 says about the mechanism is right, and
`import-and-limits.md` §12's claim that occupancy "needs no decompression at
all" is verified — `decode_block` returns `Some(None)` at line 339, before the
`ZlibDecoder` at line 366.

What is overstated is "residency at 256 falls straight out of the file with no
decode and no heuristic — which is the difference between a measurement and a
guess". A *present* block is not a *non-empty* block: Clip Studio stores a block
it has touched, and a stroke that entered a block and was then erased leaves it
present and empty. So block presence is an upper bound on residency, not
residency. This cuts two ways and both are worth saying: the survey in §10 is
therefore **conservative**, which is fine and should be stated; the *storage*
would over-back, which is a real cost and is what §9.4's reclamation would
recover. §9.4 currently calls reclamation "not urgent" on the assumption that
residency is exact.

## 12. MINOR — Figures checked

I recomputed every load-bearing number in the document. These are right:

- 4128² × 4 = 68.2 MB per page; 256 interior tiles × 65,536 × 4 = 67.1 MB. ✓
- Apron 258²/256² = +1.57%; canvas rounding 20224 × 5120 / 100 Mpx = +3.55%;
  product +5.2%. ✓
- 256 × 256 × 65,536 × 4 = 17,179,869,184 = `MAX_TOTAL_BYTES` exactly. ✓ —
  though calling it "coincidence" is generous: both are 2³⁴, and the atlas
  figure is an artefact of the arbitrary 16×16 page. At 32×32 tiles a page
  (legal at 8256 square on the reporting card) the ceiling is 68.7 GB. §3.1
  presents 17.18 GB as though it were a property of the design.
- 4K × 54 layers ≈ 448 M texel fetches a frame. ✓
- 128 × 128 × 256 × 4 = 16.8 MB page table at 32768². ✓
- 79 × 20 = 1,580 tiles per 20000×5000 layer. ✓
- Effect working set at 13 B/px: 5 × `R8Unorm` + 2 × `Rg16Uint` = 13. ✓ And
  `EFFECT_LIVE_PIXELS` (`canvas.rs:292`) does gate the bake and not the
  allocation, as §1.1 says. ✓
- `ensure_stroke_color` (`canvas.rs:3656-3684`) has no release path. ✓ §9.1(3)
  is correct.

**These are wrong or unsupported:**

- §7: `transform.wgsl`/`flip.wgsl` need "a view per *page* rather than per slot
  — at most 256 views, which is fewer than today". `LayerStore::new`
  (`canvas.rs:2093-2118`) builds `capacity` views, and capacity is at most 256.
  "At most equal", not fewer — and note it builds *two* sets, `slot_views` and
  `raw_slot_views`, so an atlas needs both per page too.
- §4.2's "if the median real layer covers a third of the page" is the whole
  saving argument and there is no median. That is what §10's `survey-residency`
  is for, and §4.2 should say the number is absent rather than picking one.

**Would change the recommendation if wrong by 2×:** occupancy (§10's first
example) is the only one. At 15% the design is obviously right; at 60% —
plausible if presence over-reports as finding 11 says — a 21.6 GB document
becomes 13 GB, which still does not fit a 10 GB card, and the answer is
`layer-residency.md` instead. **The survey must therefore report both presence
and non-emptiness**, or it will answer the wrong question at exactly the
occupancy where the answer matters.

## 13. MINOR — Invariants: I checked the ones the brief named, and they hold

Stated so the author does not have to re-derive it:

- **Stroke opacity applied once at commit** — §5.1 is right; `dab.wgsl` is
  untouched and the scratch is unchanged in stages 1 and 2.
- **The `max` blend saturating** — unchanged, for the same reason. §5.5's note
  that build-up is not idempotent and needs each fragment in exactly one tile is
  correct and is the right thing to have flagged.
- **Composite and commit identical maths** — §5.3's vertex/fragment split is
  sound. I read `commit.wgsl`: `fs` and `fs_blend` derive `uv` from `in.doc /
  u.doc_size` (lines 117, 141), and `vs` maps `mix(rect_min, rect_max, c)` into
  clip space (lines 71-75). Moving the atlas mapping into `vs` while leaving
  `in.doc` in document space does keep both fragment shaders byte for byte. One
  thing to carry: `fs_blend` computes `vec2<i32>(floor(in.doc - u.rect_min))` to
  index the backdrop (line 145), so the backdrop's own origin has to stay the
  *piece* origin and not become the tile origin.
- **"A layer's slot never changes"** — preserved; §3.2's argument for a
  slot-indexed page table over a storage buffer is the right one and is the
  reason parking and recycling need no second scheme.
- **§4.3's `clip_alpha` ordering** — verified against `composite.wgsl:256-264`.
  The line is as quoted. The recommendation to write the skip as a `select` on
  `lay` rather than a `continue` is correct and is what §4.1's own snippet
  already does. Worth adding: an early `continue` would *also* drop the wet
  stroke preview on the active layer (lines 228-252), which is a louder bug than
  the clip one and is not mentioned.
- **`history::VERSION` and `umber-version` do not move** — §6.1 is right;
  `PixelPatch` is document-space throughout.

---

# C. `import-and-limits.md`

## 14. BLOCKING — §5.3's second refusal states a GPU capacity figure Umber cannot read, and a sibling already says so

**What I checked.** `wgpu = "29"` / 29.0.4 in `Cargo.lock`; `gpu.rs:73`;
`slot-lifecycle-and-vram.md` §6 ("Does Umber know how much memory it has? No.")
and §7.2's own draft wording.

**What I found.** The proposed sentence is:

> This document needs 21.6 GB of graphics memory for its 54 layers at
> 20000×5000, and **this GPU has 10.0 GB**.

wgpu exposes no total-memory query. `Device::generate_allocator_report` reports
*Umber's own* allocations, not the card's capacity. §5.3 hedges — "if no honest
figure is available the gate should be stated against something that is" — but
it prints the figure in the draft, and a draft in a design document is what gets
implemented.

`slot-lifecycle-and-vram.md` §7.2 has already drafted this refusal and
deliberately does **not** claim a "has" figure: "and this graphics card could
not provide it. […] The 'has' figure is not [computable], without §6.2's hal
route, and the sentence deliberately does not claim one." That is the right
answer and it is already written down. The two documents are in direct conflict
and `import-and-limits.md` does not cite the sibling.

Two smaller problems in the same two sentences:

- **The first refusal names Clip Studio Paint.** `ImportError::StackTooLarge`
  carries `{ width, height, layers, bytes }` (`docimport/mod.rs:904`) and no
  format, and its `Display` is the generic one at `:965-977`. "Merging layers
  together in Clip Studio Paint" would be printed over a `.kra` and a `.psd`.
  Either take the format into the variant or say "in the application that made
  it".
- **`import-and-limits.md` §5.3 and `slot-lifecycle-and-vram.md` §7.2 also give
  different layer counts for the same card** — ~22 and ~21 slices — and the
  arithmetic gives 25. Neither shows its working. Small, but it is two
  unmeasured figures for one quantity in one folder.

## 15. BLOCKING — The trial `create_texture` is fatal as written, and is a no-op in the case the same document calls more likely

**What I checked.** `app.rs:4886`
(`.on_uncaptured_error(Arc::new(crash::device_error))`),
`crates/umber-app/src/crash/mod.rs:477-480` (`device_error` **panics**),
`crates/umber-render/tests/gpu_pipeline.rs:3683` (the `push_error_scope` idiom
already in the repo).

**What I found.** §5.3 and §8.2 both land on "A trial `create_texture` of the
full array followed by a drop is the version that needs no budget reading and no
new platform knowledge, and it asks precisely the right question." Two defects:

1. **It kills the process.** `create_texture` does not return a `Result`;
   failures go to the uncaptured-error handler, which `app.rs:4886` wires to
   `crash::device_error`, which panics. A trial allocation that fails therefore
   produces the crash box the gate exists to avoid — the failure mode the
   document is trying to replace, arriving one step earlier. Making it
   answerable needs `device.push_error_scope(wgpu::ErrorFilter::OutOfMemory)`,
   the allocation, `pop_error_scope()` and a `device.poll` to settle the future.
   That idiom is already used in this repo (`gpu_pipeline.rs:3683`, for
   `Validation`), so it is available — but it is not "no new platform
   knowledge", and the document should name it.
2. **It does not detect the case §8.2 itself calls more likely.** §8.2 says:
   "On Windows it may not even fail. WDDM permits over-commitment and pages
   texture memory to system RAM, so the more likely outcome is that the document
   opens and every frame thereafter drags the whole array across PCIe." A trial
   `create_texture` **succeeds** in exactly that case. So the proposed gate
   answers "no" only where the current behaviour already refuses loudly, and
   answers "yes" where the artist is about to get an unusable canvas. It is not
   "precisely the right question"; it is the wrong one on the platform the
   report came from.

The `memory_budget_thresholds` route §8.2 identifies is the mechanism that does
address over-commitment, and the document is right to say the policy belongs
elsewhere. It should not then recommend the trial allocation as the version
that needs no policy — it needs a *different* policy, not none.

## 16. SUBSTANTIVE — §5.2's argument about the refusal is sound, and I would keep it

Since an unwarranted objection costs a cycle, here is the one place I went
looking for a flaw and did not find one. I verified:

- The message is exactly as quoted (`docimport/mod.rs:965-977`).
- `MAX_TOTAL_BYTES` is `16 << 30` (`:619`) = 17,179,869,184 = 17.18 GB decimal,
  and `check_bounds` compares `width × height × 4 × painted` against it
  (`:1138-1166`).
- 17.18 GB / 400 MB = 42.9, so 42 layers admit, at 16.8 GB — comfortably past a
  10 GB card. The "followed exactly, the advice produces a document Umber cannot
  draw" claim is arithmetically correct.
- The canvas genuinely is the other multiplicand and genuinely is omitted from
  the advice.

The argument that this fails CLAUDE.md's own standard is fair and well made.
"A refusal whose advice leads to a worse failure is not a refusal that has named
the right bound" is the right generalisation of the 15000×5000 case, and it is
the sentence I would keep from this document if I could keep only one. The
replacement's second half — "each halving of the width and height quarters the
figure" — is honest, actionable and costs nothing. Keep it; fix finding 14.

## 17. SUBSTANTIVE — Neither document notices that the upload path is unbanded while the readback path is banded

**What I checked.** `canvas.rs:7180-7218` (`write_layer_rect` → `write_rect`,
one `queue.write_texture` for the whole rect), against `read_layer_rect`'s
banding at `:6915` and `band_rows`; `gpu.rs:73` (`using_resolution` raises the
three texture dimensions only, so `max_buffer_size` stays at
`downlevel_defaults`' 256 MB).

**What I found.** CLAUDE.md is explicit that "**Every readback goes in bands,
because a document can be larger than the largest buffer the device will
make**", and gives 256 MB as the limit real hardware has. A 400 MB
`write_layer_rect` at import is a single `write_texture` of 400 MB, and it is
what `install_import` calls. It evidently works today, because wgpu's internal
`StagingBuffer` is created below the validated `create_buffer` path — but that
is a wgpu implementation detail, not a guarantee, and it is the mirror image of
a rule this codebase already treats as load-bearing.

§7.2's streaming design preserves it: "decodes one layer into a fresh
canvas-sized buffer and yields it" is still one 400 MB `write_texture`. Two
consequences worth writing into the design: the upload should band for the same
reason the readback does, and banding the upload is *also* the fix for §8.1 —
each band's staging buffer is retired by the submit that follows it, so the
peak becomes one band rather than one layer. That is a better shape than
"submit once per upload" and it costs the same code.

## 18. SUBSTANTIVE — `docs/perf/allocation-accounting.md` does not exist

§5.3 and §8.2 both defer the policy decision to it. `ls docs/perf/` gives
`composite-throughput.md`, `formats-and-host-memory.md`, `import-and-limits.md`,
`layer-residency.md`, `slot-lifecycle-and-vram.md`, `tiled-layer-storage.md`.
The document meant is `slot-lifecycle-and-vram.md`, whose §6 is "Does Umber know
how much memory it has? No." and whose §7 is "The bound that is missing, and how
a refusal should read" — i.e. exactly the two deferrals, already written, and
already disagreeing with §5.3 (finding 14). A dangling reference matters more
than usual here because it is what hid the disagreement.

## 19. MINOR — §8.3's stale comment is real; the backend trace is probably right and is not load-bearing

- **§8.3 verified.** `ImportedDocument::MAX_DIMENSION` is
  `Document::MAX_EDGE` (`docimport/mod.rs:590`), which CLAUDE.md records as
  32768. The `install_import` comment saying 16384 is stale. Correct finding.
- **§10's backend-ordering trace** I could not verify independently — the wgpu
  source is not vendored in this checkout and I did not fetch it. The reasoning
  (Vulkan first in `instance_per_backend`, `sort_by_key` stable) is internally
  consistent and matches the coordinator's reading. It is also **not
  load-bearing**: both recommendations that follow from it — name the backend in
  the dimension refusal, and log the ceiling with the backend at startup — are
  right whichever way the ordering goes, and the third option is correctly
  refused. Nothing needs to change here.
- **§9's assessment of `loading.rs`** I spot-checked and agree with; the three
  small suggestions are all cheap and all right. §9's third (run the device gate
  before the worker) is the most valuable of the three and should be promoted
  into §0's numbered list, because a minute-long wait for a refusal is the
  artist-visible half of this whole document.

## 20. MINOR — §2 and §11's figures

- "each inflating to `(1 + 4) × 65,536` = 320 KiB" — `MAX_CHANNELS` is 5
  (`csblocks.rs:41`) and is described as "the widest shape Clip Studio writes".
  It is a bound, not what every block is; a mask block is one plane, 64 KiB. The
  518 MB/layer and 28 GB figures are therefore upper bounds and should say so.
- §2's ~25 s open time rests on "if that file carried around 40 layers", which
  is a guess inside an extrapolation. §13 names it, correctly. It is not
  load-bearing for any recommendation.
- §11's advice to "aim for ten to twelve merged raster layers" is sound as far
  as it goes, but at that count *adding a layer afterwards* hits the
  `ensure_slots` transient — a 12-slice array reallocating to 13 peaks at
  10 GB on a 10 GB card. The advice should either say "and do not add layers"
  or point at the growth fix.

---

# D. What neither author considered

## 21. SUBSTANTIVE — The `ensure_slots` growth transient is the other half of the reported symptom, and neither document has it

**What I checked.** `canvas.rs:3157-3212` (grown store built, copied, then
assigned — old and new both live across `:3172-3201`), `grown_capacity`'s
`growth_quantum` (`:629+`, "1 at 10000²").

**What I found.** At 400 MB a slice the growth quantum is one, so a document at
this canvas grows one slice at a time, and each growth allocates a whole new
array while the old is live. Adding one layer to a 12-layer 20000×5000 document
allocates 5.2 GB with 4.8 GB live — 10 GB on a 10 GB card, for one layer.

`import-and-limits.md` attributes the "VRAM skyrockets" report entirely to §8.1's
staging doubling. That is real and verified, but it fires *once, at open*. The
growth transient fires on **every layer the artist adds afterwards**, which is
the ordinary use of a painting application. `slot-lifecycle-and-vram.md` §4 has
it — "the transient nobody counted" — and neither of the two documents under
review cites it, so a reader of either would come away with an incomplete
diagnosis of the symptom they were asked about.

## 22. MINOR — Effects grow outside their layer's content

An outline or a drop shadow reaches outside the pixels it derives from, so under
sparse residency an effect slice's backed set is the layer's content *dilated by
the effect's reach*, not the layer's content. `tiled-layer-storage.md` §7 says
`effect.wgsl` "needs the page table, in the same shape as the composite" and
that the working set stays dense in stage 1, which is right as far as it goes;
the residency of the effect's *result* slice is not addressed anywhere. Same
point applies to §4.4's per-frame culling: a layer entirely off screen may have
a shadow that is not.

## 23. MINOR — Scope, in the other direction

Two places where less would be better:

- **`tiled-layer-storage.md` §9.1 is four other documents' work.** Items 1 and 3
  are `slot-lifecycle-and-vram.md` §4/§5; item 4 is `formats-and-host-memory.md`
  §8 and `import-and-limits.md` §7; item 2 is `layer-effects.md`'s stage 3.
  Every one of them is a good recommendation and none of them belongs to the
  document that would be *deferred* by doing them. Listing them as "before any
  of it" is right; owning them is what produces four half-fixes.
- **`import-and-limits.md` §6 is longer than it needs to be.** The three
  refusals (dropping layers, proxy resolution, read-only) are each settled in a
  sentence by the constraint the brief already states, and the section reads as
  though the decision were open. §6's genuinely useful paragraph is the last one,
  naming the residency middle case; the rest could be three bullets.

---

# What would settle the things I could not

- **Whether the composition works** — writing the residency-carrying upload
  signature down (finding 1). This needs no measurement and no device; it is a
  decision, and it is one paragraph in whichever document takes it.
- **Whether the atlas is worth building at all** — `survey-residency.rs`
  (`tiled-layer-storage.md` §10), extended per finding 11 to report *non-empty*
  block counts as well as present ones. Two numbers per layer instead of one,
  and the second needs the inflate the first does not, so it is slower but still
  bounded by one block at a time.
- **Whether the apron can be made structural** — enumerate every write to a
  slice in `canvas.rs` (there are ten `touch_slot`/`touch_all_slots` sites and
  `render_float` is not one of them), and decide whether they can all be routed
  through one encoder-owning function. If they cannot, finding 4 says take
  refusal 7.
- **Whether the staging doubling and the growth transient are what the artist is
  seeing** — `slot-lifecycle-and-vram.md` §11's `measure-vram.rs`, which already
  proposes exactly the `generate_allocator_report` sweep both documents ask for
  separately.
- **What actually happens when the array does not fit on that machine** — still
  unsettled, and finding 15 is why it matters: the fatal case and the
  over-commitment case want different gates, and the trial allocation only
  detects one of them.
