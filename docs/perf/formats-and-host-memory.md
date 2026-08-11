# Bytes per pixel: texture formats and host memory

What a document costs *per pixel*, on the card and in RAM, and which of those
costs are necessary.

This is one of several documents on the memory of a very large canvas. It owns
**precision and byte width** and nothing else: where the pixels live, how many
of them are resident, how allocation is accounted for and how fast the
composite runs are other people's. Where a finding of mine lands in one of
those, §12 hands it over by name rather than designing it here.

The overriding constraint throughout is the artist's: **stroke quality, layer
fidelity and the rasterised image stay pristine.** Every proposal below is
marked exact or lossy, and `saving_and_reopening_does_not_move_a_pixel`
survives all of them — one of them, §5, makes it *more* true than it is today.

**This is the second draft.** `docs/perf/critique-allocation-formats.md`
reviewed the first against the code and found four paths my mask proposal did
not count, one arithmetic contradiction, and one whole allocation class neither
this document nor its sibling had inventoried. Chasing those down overturned
the central recommendation of §5 — not because the criticism said so, but
because counting the paths properly sent me back to the premise underneath
them, and the premise was wrong. §13 records what moved and what I disputed,
for anyone holding the first draft.

## 1. Recommendation, in short

1. **Add a `queue.submit` inside `install_import`'s upload loop.** One line,
   no new API, no policy. Today that loop writes every layer with
   `Queue::write_texture` and never submits, so wgpu holds a canvas-sized
   staging buffer per layer until the *next frame's* submit — 8.4 GB of staging
   on top of an 8.4 GB array for a 21-layer open at the reference canvas. And
   that path's OOM is fatal by construction, so no refusal upstream can rescue
   it. §8.4. **This is the cheapest fix in this document by a wide margin and it
   is on the exact operation the user reported failing.**
2. **The autosave capture is the largest honest number here and deserves a
   stronger recommendation than "worth doing": about 10 GB of host RAM, every
   five minutes, unattended, with no quality trade anywhere in it.** §10.1.
3. **4 bytes per pixel per layer is necessary and must not move.** §4.
4. **Lossy compression is refused, and the reason is not squeamishness — it is
   dominated.** BC7 would reach 1 B/px, and every layer it could compress is a
   layer residency can evict outright, which costs nothing at all. §6.
5. **A mask should move to a dedicated `R8Unorm` array holding *linear*
   coverage.** This reverses the first draft, which recommended packing three
   masks into one slice's RGB channels. Packing turned out to touch four more
   paths than I counted *and* — the finding that actually decided it — the
   sRGB typing I was preserving is not a designed property of a mask at all. It
   is inherited from the array a mask borrowed, it is the wrong distribution for
   a multiplier on alpha, and it is currently **discarding about 74 of the 256
   states an imported mask can express**. §5.
6. **Still: build it into the tiled store, not before it.** The ordering
   recommendation is unchanged and the review strengthens it — every path the
   mask work touches is a path tiling rewrites anyway. §5.5.
7. **The import materialises every layer at canvas size before the first byte
   reaches the GPU**, so `MAX_TOTAL_BYTES` is not a safety margin — it is a
   promise that Umber will allocate up to 16 GiB of RAM to open a file. §8.
8. **Undo is not the problem at this scale**, and the already-noted future fix
   is made *enormously* cheaper by tiling — not marginally. §9.

## 2. The reference canvas, and why no figure here is new

The document that started this is 20000 × 5000. That is **exactly 10⁸ pixels,
which is exactly 10000²** — so every figure CLAUDE.md and the source already
record for a 10000² canvas applies to it verbatim, and nothing in this document
needed re-deriving to make it apply.

| width | bytes at 10⁸ px |
|---|---|
| 1 B/px | 100 MB |
| 2 B/px | 200 MB |
| 4 B/px | **400 MB** |
| 8 B/px | 800 MB |

A 10 GB card therefore holds **25 slices** of the layer array and nothing else
— and on an import, half that, once §8.4's staging is counted.
`MAX_TOTAL_BYTES` admits 42 painted layers at this size, so a document Umber
agrees to open is routinely a document it cannot hold. That is the report.

---

# Part (a): texture formats

## 3. What every allocation in the engine costs

Read off `crates/umber-render/src/canvas.rs`. "Canvas" means the full document
rectangle. The last row is the one the first draft missed entirely and it is
not a texture — see §8.4.

| allocation | format | B/px | extent | live when |
|---|---|---|---|---|
| layer array | `Rgba8UnormSrgb` | 4 | canvas × `capacity` slices | always |
| stroke coverage scratch | `R8Unorm` | 1 | canvas | always |
| per-dab colour scratch | `Rgba16Float` | **8** | canvas | a smudging stroke only; 1×1 otherwise |
| float base + float source | `Rgba8UnormSrgb` | 4 each | canvas | a transform is up |
| flip scratch | `Rgba8UnormSrgb` | 4 | canvas | transient, one flip |
| export target | `Rgba8Unorm` | 4 | canvas | transient, export / capture / save |
| effect coverage, grown, blur ×2 | `R8Unorm` | 1 each | canvas | any effect |
| effect band plane | `R8Unorm` | 1 | canvas | a centred outline |
| effect seed planes ×2 | `Rg16Uint` | 4 each | canvas | an outline |
| commit backdrop | `Rgba8UnormSrgb` | 4 | canvas width × ≤ 64 rows | transient, a blended commit |
| tip mask, grain tile, selection mask | `R8Unorm` | 1 | their own | as bound |
| coloured tip | `Rgba8UnormSrgb` | 4 | ≤ 256² | a coloured stamp |
| thumbnail targets | small | | 64² | as scheduled |
| **`wgpu` staging buffers** | — | 4 | canvas **× layers written since the last submit** | every `write_layer_rect` |

Three observations follow, and only one of them is a defect of *width*.

**The single-channel ones are already minimal.** `STROKE_FORMAT` is `R8Unorm`
and the argument for it is settled and correct: it is exactly as wide as the
layer alpha it commits into, so it adds no loss of its own, and `R16Unorm` is
not a legal render target on the feature set Umber requests. The effect planes
reuse it for the same reason. Nothing to do.

**`Rg16Uint` for the jump flood's seeds is 4 B/px and cannot be narrowed.** It
holds a *coordinate* and must be exact; an `f16` runs out of whole integers at
2048. Two full-resolution planes at 800 MB is a genuine cost and it is a
residency question (they are transient per bake), not a width question. Handed
over in §12.

**The per-dab colour scratch is the widest texture in the engine, it is
canvas-sized, and its width has not been argued against the option the layer
array itself takes.** `STROKE_COLOR_FORMAT`'s doc says `Rgba16Float` "rather
than `Rgba8Unorm` because these are **linear** values. Eight bits of linear
light bands visibly in the shadows". That is true and it is an argument against
`Rgba8Unorm` specifically — it is not an argument against **`Rgba8UnormSrgb`**,
which is 8 bits distributed perceptually with the hardware decoding to linear
on read and re-encoding on write, which is precisely what `LAYER_FORMAT` does
and precisely why it does it. At 4 B/px instead of 8 that is **400 MB back on
every smudging stroke at the reference canvas.**

I am not recommending the change, because there is a real counter-argument the
existing comment does not make and which I cannot settle by reading: the layer
is written once per commit, where the colour scratch is read-modify-written
once per *dab* under an `over` blend, so rounding compounds inside it in the
way the build-up path's does. **The measurement that settles it already exists
in the shape it needs**: the build-up accumulation study behind "Why widening
the scratch does not deliver those 1024 levels" stamped one preset along a
stroke fifty dabs deep against exact arithmetic and answered "at most 3 levels
of 255". Run that against a smudging preset with `Rgba8UnormSrgb`,
`Rgba16Float` and exact float, over a dark painting, which is where the
original comment says it would show. The review adds one condition I had
missed and it is a good one: the colour scratch is *also* read by
`probe_canvas`'s pickup, so a narrower scratch changes what a blender picks up
as well as what it lays down, and the sweep must include a stroke scrubbed back
and forth over its own wet paint, not only a single pass.

## 4. Is 4 bytes per pixel per layer necessary?

Yes, and I could not find a defensible way round it. The review checked this
section and confirmed both mechanical claims.

**Exactness first.** The layer is where the picture *is*. `docimport::srgb`'s
two directions must stay exact inverses or a document moves a level every time
it is saved and reopened, and ORA stores 8-bit straight-alpha RGBA. Anything
narrower than 8 bits per channel breaks that guarantee at the format boundary,
not merely at the eye.

**The sRGB typing is doing real work *here*, and this is the one place it is
the right instrument.** Eight bits of *linear* storage puts a dark ink at
linear 0.0056 on 1–2 of 255. The same eight bits typed sRGB distribute
perceptually, the hardware decodes to linear before blending and re-encodes on
write, and the dark end is where an artist notices. Hold that thought: §5.2 is
about a slice where the same typing is applied to a quantity that is not
colour, and is wrong for it.

Any narrower proposal has to answer the precision bar, and the two obvious ones
do not: `Rgb10a2Unorm` is 4 B/px anyway and gives alpha **two bits**, which is
fatal for premultiplied compositing; `Rgba4Unorm` and friends are not in the
guaranteed set at all.

**Renderability is the harder bar.** A layer slice is a `RENDER_ATTACHMENT` —
commit writes it, the clear writes it, the mask fill writes it, the flip writes
it, a float commit writes it, an effect resolve writes it. That rules out every
block-compressed format outright (§6) and every packed format WebGPU does not
list as renderable. The set of formats that are 4-channel, renderable,
blendable, exact enough for the file boundary and available on
`Features::empty()` has one useful member and Umber is using it.

**And it is an array.** Every slice of a texture array shares one format, so
"a narrower layer for the layers that could take one" is not expressible
without a second texture object. That constraint is what §5 is about.

So: the width stays. What is left is (a) the slices that carry less than four
channels of data, and (b) the slices that carry no data at all. Those are §5
and §7.

## 5. The mask: four bytes to carry one, and worse

`Layer::mask` is another slice of the same array. Four bytes per pixel to carry
one channel of coverage, of which `composite.wgsl` reads `.r`.

### 5.1 The existing argument, and what has changed under it

`layer.rs`'s module docs make the case, and it is a good one: a dedicated
single-channel array "would then need its own banded readback, its own resize,
its own flip, its own autosave capture, its own undo patch width and its own
history file revision — six paths to keep in step with the six that already
exist, for a saving on a texture most documents never allocate at all."

Two clauses of that have gone stale and one has not.

**"A saving on a texture most documents never allocate" is false for the
document that motivated this.** A mask at the reference canvas is 400 MB
carrying 100 MB. A masked layer is 800 MB. Eight masks is 3.2 GB, of which
2.4 GB is three copies of the same byte, on a 10 GB card. This is no longer a
rounding error and the docs should stop describing it as one.

**"A mask *is* a layer to everything" is already not quite true.** `docformat`
has `SaveLayer::mask` as a distinct field, writes it to `umber/masks/NNN.png`
as a **greyscale** PNG, and gets there with
`mask.chunks_exact(4).map(|px| px[0]).collect()` — a canvas-sized allocation
made to throw three quarters of itself away. The reader widens grey back to
`(g, g, g, 255)`. So the file format already pays a mask-shaped special case,
and it already knows a mask is one byte. `docimport::srgb::encode_coverage`
writes `[g, g, g, 255]`: two of the four bytes are literal copies and the
fourth is a constant.

**The six paths are still six paths.** This part of the argument holds and I
costed it:

| path | what a second array actually costs |
|---|---|
| banded readback | `read_texture_rows` hard-codes `let unpadded = width * 4;`. One `bytes_per_pixel` parameter. Small. |
| resize | a second `create_texture` and a second whole-array copy in `CanvasRenderer::resize`. Small. |
| flip | a second scratch of the right format and a second pipeline: `flip.wgsl` writes a `vec4`, and an `R8Unorm` attachment needs its own entry point. Small, and still an exact permutation — more exactly so, see §5.3. |
| autosave capture | `Capture` carries one `padded` for every step; it would need one per step. Small. |
| **undo patch width** | `PatchPiece::new` asserts `bytes.len() == rect.area() * 4` and `PieceBytes::Flat` is `[u8; 4]`. **`PixelPatch::slot` is a bare `u32` naming a slice of *the* array.** With two arrays a slot stops identifying a texture, and that type is in `umber-core`, in the history, in the ORA history writer and reader, and in `write_layer_rect`. |
| history file revision | a one-channel patch is a new body shape, so `history::VERSION` to 4 and an older build discards the history whole. One bump. |

### 5.2 What the first draft got wrong: the sRGB typing is inherited, not designed

**The first draft's central argument was that a dedicated `R8Unorm` mask array
is a *quality regression*, because WebGPU has no single-channel sRGB format, so
a mask would lose the perceptual distribution it has today. The premise is
true. The conclusion was backwards, and re-examining it is what reversed this
section's recommendation.**

The premise was checked by the review and confirmed: enumerating every `*Srgb`
variant of `wgpu::TextureFormat`, the only uncompressed ones are
`Rgba8UnormSrgb` and `Bgra8UnormSrgb`. There is no `R8UnormSrgb` and no
`Rg8UnormSrgb`. So a single-channel mask array cannot have hardware sRGB.

What I did not ask is whether a mask should want it.

**A mask is not colour. It is a multiplier on alpha, and this codebase already
knows that alpha is the wrong quantity to gamma-encode.** `LAYER_FORMAT`'s own
documentation says it: "an sRGB format encodes RGB only", so the layer's alpha
channel is linear 8-bit, and `STROKE_FORMAT` is justified as being "exactly as
wide as where the coverage is going" — a *linear* eight bits. Coverage, alpha
and a mask are the same kind of number. The mask is the only one of the three
stored in a colour channel, and it is there because it was housed in a colour
array, not because anybody argued it belonged there.

**The distribution is wrong in the direction that matters.** With the mask byte
`b` in an sRGB-typed channel, the sampler returns `srgb_to_linear(b/255)`.
Adjacent stored bytes near the reveal end differ by about **0.0089** in the
multiplier; near the hide end by about 0.0003. Stored linearly they differ by a
uniform 0.0039. So sRGB storage is roughly **2.3× coarser where the layer is
fully present** and 13× finer where the layer contributes almost nothing to the
picture. That is precisely inverted: near the hide end a step in the multiplier
is multiplied by an alpha near zero and is invisible, and near the reveal end it
is the picture. A smooth mask gradient bands at the top under sRGB and does not
under linear.

**And it is not merely a redistribution — it is a loss, today, on every
imported mask.** `srgb::coverage_table` is built as
`round(linear_to_srgb(v / 255) * 255)` over `v` in `0..=255`. That map's slope
in output-per-input terms is 12.92 at zero and falls through 1 at linear ≈
0.244 (byte ≈ 62), reaching ≈ 0.44 at the top. Above byte 62 it compresses, so
consecutive source values collide. Counting: inputs 0..62 land on outputs
0..136, all distinct; inputs 62..255 land on outputs 136..255, which is 120
distinct values for 194 inputs. **About 182 of the 256 states survive; roughly
74 are unreachable, all of them in the upper three quarters of the reveal
range.** The suite already knows the shape of this — the guard is
`coverage_encoding_is_monotone_and_never_inverts`, monotone and not injective —
it simply has never been asked to count. The exact figure is a three-line check
over `coverage_table()` and should be run before anybody acts on the estimate.

So the honest position, which is the opposite of the first draft's:

> A dedicated `R8Unorm` mask array holding **linear** coverage is not a quality
> regression. It is a quality **improvement** — it puts the precision where the
> picture is, and it makes an imported mask exact where about 74 of its 256
> states are currently collapsed. It also deletes `encode_coverage`, because
> the conversion it performs becomes the identity.

The one branch my original argument does defeat survives and should be recorded
so nobody takes it: **storing the sRGB-encoded byte in `R8Unorm` and decoding
in the shader is wrong**, because `textureSampleLevel`'s bilinear filter would
run before the manual decode and interpolate in the encoded domain. Store
linear and the filter is in the linear domain, which is what a multiplier wants
and what the hardware decode gives today. Nobody should build the middle
option.

### 5.3 What a linear mask array does to the file, exactly

This has one trap in it and it is worth stating precisely, because the tempting
version is silently lossy.

Today the round trip is exact: the file's greyscale byte becomes `(g, g, g,
255)` in the slice, `docformat` reads `.r` back out, and the same byte is
written. `saving_and_reopening_does_not_move_a_pixel` holds for masks by that
identity.

- **File stays sRGB, slice becomes linear — refused.** The conversion would run
  at both ends, and the forward map is *not injective* (§5.2), so a save and a
  reopen would collapse about 74 states. That breaks the guarantee. Do not
  build this.
- **File becomes linear too — correct, and it is a `umber-version` bump.** The
  round trip is byte for byte again, and it is exact over the whole range
  rather than over the 182 states that survive today. An older build reading a
  version-4 file would read a linear byte as sRGB and show every mask at the
  wrong gamma — a *wrong* picture, not a plainer one, which is exactly the line
  CLAUDE.md draws the version on, and exactly what masks earned revision 2 for
  in the first place. The bump is needed anyway for the patch width.

The composite gets simpler, not harder: it samples a second array and uses the
byte directly, with no decode, and `has_mask` stays the uniform branch it
already is. The flip gets simpler too — `LAYER_FORMAT_LINEAR` exists so the
flip can read the layer array without a transfer function, and an `R8Unorm`
mask array needs no such view because it never had one.

### 5.4 Why packing three masks into one slice loses, with every path counted

The first draft recommended packing three masks into one slice's `R`, `G` and
`B` channels — 1.33 B/px, one array, one format. The review found four paths it
did not count. All four check out against the source and all four are real.

1. **`SlotPool` must become channel-granular, or parking breaks.** My sentence
   "a slice is parked only when all three of its channels are free" was wrong
   and it reopens exactly the corruption `SlotClaim` exists to prevent: with
   layer A's mask at `(7, R)` and layer B's at `(7, G)`, deleting B under a
   slice-granular rule parks nothing — slot 7 is still live — so `(7, G)` goes
   back on the free list and B's `PixelPatch` replays into whatever mask
   inherits it.
2. **`fill_layer_white` and `clear_layer` clobber whole slices.** Both are
   `LoadOp::Clear` over the attachment, and a clear cannot express one channel.
   `fill_layer_white` **is what adding a mask calls** — so under packing,
   adding a second mask to a slice would wipe the first one to white, silently
   revealing everything a painter had hidden. Its own doc comment argues that
   1.0 encodes to 255 "in every channel", which becomes false for two of the
   three masks in the slice.
3. **`slot_revision` is per slice**, so three packed masks share one revision.
   `Thumbs` and `CachedEffect::mask_revision` both key off it. I would put this
   more sharply than the review does, which calls it "not a correctness
   failure": at 100 Mpx a bake is 4–34 ms, so a stroke on one mask would rebake
   unrelated shadows on two other layers on *every frame of that stroke*. On the
   canvas this whole investigation is about, that is dropped frames while
   painting — a frame-rate failure, not a cache inefficiency.
4. **`ColorWrites` is pipeline state, not dynamic state.** My "free at run time"
   was true and misleading. `write_mask` lives in `ColorTargetState` and is
   baked into the pipeline. So packing needs a variant per channel for the new
   `write_layer_rect` pass, for `fill_layer_white`, for `clear_layer` — **and
   for the commit**, which I missed entirely and which is the one that matters:
   `StrokeStyle::on_mask` means an ordinary stroke commits into a mask slice,
   so packing retires CLAUDE.md's "a stroke on a mask needs **no new
   pipeline**" and adds a permutation dimension to `Shared`, which exists once
   per process precisely so that dimension does not multiply.

**Counted properly, the comparison inverts.** The first draft's table claimed
packing won on both code cost and quality. It wins on neither.

| | today | dedicated linear `R8Unorm` array | packed RGB |
|---|---|---|---|
| bytes per mask pixel | 4 | **1** | 1.33 |
| precision where the picture is | sRGB, ~2.3× coarse at the reveal end | **linear, uniform** | same as today |
| imported-mask states reachable | ~182 of 256 | **256 of 256** | ~182 of 256 |
| filtering domain | linear (correct) | linear (correct) | linear (correct) |
| arrays | 1 | 2 | 1 |
| what changes in `SlotClaim` | — | a **tag**: which pool | a **granularity**: which third of a slice |
| `SlotPool`, `slot_capacity_needed`, `live_slot_ceiling`, `has_headroom`, `effect_slot_base`, `StackShape::byte_len` | — | unchanged, one instance per pool | every one of them must learn a slice can be partly claimed |
| `fill_layer_white` / `clear_layer` | — | an `R8Unorm` variant, still a whole-slice clear | a write-masked **draw**, ×3 |
| `slot_revision` | — | per array, unchanged in meaning | cross-invalidates; needs per-channel |
| commit pipeline | — | one variant for the `R8Unorm` target | one variant per channel |
| `history::VERSION` | — | +1 | +1 |
| `encode_coverage` | exists | **deleted** | kept |

The decisive structural difference is the `SlotClaim` row. **Forking changes a
claim's *tag*; packing changes a claim's *granularity*.** A second `SlotPool`
keeps `next`, the tail compaction, `has_headroom`, "one past the highest claim"
and `begin_float`'s reservation all working unchanged, per pool. Channel
granularity retires every one of those invariants at once — including
`StackShape::byte_len`, which would have to charge a third of a slice for a
parked mask channel, which is a change to what the undo budget *means*.

There is a symmetry here worth stating plainly, because it is what makes the
ordering recommendation in §5.5 more than caution: **there is no version of
narrowing a mask that leaves `SlotClaim` alone.** Forking breaks "a slot names
a texture"; packing breaks "a slot names a parkable thing". The review is right
that the two options are closer in cost than my table showed. Counting the four
paths, forking is now clearly the cheaper as well as the better, but neither is
small.

### 5.5 Ordering: build it into the tiled store, not before it

Unchanged from the first draft, and the review agrees and strengthens it.

What fills the card is layer slices, not mask slices; masks are the 20% and the
layers are the 80%. And every path §5.4 enumerates — the pool, the clear, the
revision counter, the readback width, the patch width — is a path a tiled store
rewrites anyway. Build it *into* that store, where the allocator can carry a
width per tile class and a mask tile is simply a tile of a narrower class, and
none of the six paths ever learns about it.

The three things worth carrying into that design now, so they are not
rediscovered:

- A mask tile is **one byte per pixel and linear**, not a quarter of a colour
  tile. §5.2.
- Its claim is a **tag**, not a granularity. §5.4.
- The file's mask byte becomes linear with it, which is a **`umber-version`
  bump** and makes the round trip exact over the whole range. §5.3.

## 6. Lossy compression: refused, and cleanly

BC7 is 128 bits per 4×4 block — 1 B/px, a fourfold saving. BC1 is 0.5 B/px with
one bit of alpha, which is not a paint layer. The verdict is no, on four
independent grounds, any one of which is sufficient. The review verified the
two mechanical ones against `wgpu-types` and confirmed both.

1. **It is not a render target.** `Bc7RgbaUnorm`'s guaranteed usage is
   `(none, basic)` — no `RENDER_ATTACHMENT`, no storage. A layer slice is
   written by commit, clear, mask fill, flip, float commit and effect resolve.
   Every one would become decompress-modify-recompress, which is more memory
   during the operation than the uncompressed layer it replaced.
2. **It needs a feature Umber does not request.**
   `Features::TEXTURE_COMPRESSION_BC` against `required_features:
   Features::empty()`. That constraint is not an oversight — it is what stops a
   desktop build depending on what a mobile GPU refuses — and BC specifically
   is the desktop family; the mobile answer is ETC2/ASTC, which is a *second*
   codec and a second encoder.
3. **There is no encoder here.** A good BC7 encode is seconds of CPU per layer
   at this canvas, on a path that would have to run whenever a layer stops
   being the active one. There is no GPU encoder in the tree and adding one is
   a shader project of its own.
4. **It is lossy, and the artist's constraint is that it is not.**

### 6.1 The proxy idea, and why it loses to eviction

The interesting form of the question is the one worth answering properly: keep
the exact pixels somewhere, and hold a BC7 *proxy* in VRAM for compositing when
zoomed out, replacing it with exact data when the artist zooms in — never
written back to the document, so `saving_and_reopening_does_not_move_a_pixel`
is untouched.

It is coherent. It is also **strictly dominated by residency**:

- The set of layers a proxy could compress is exactly the set of layers that
  are not being painted on — because the active layer must be exact for the
  commit. That is the same set a tiled, resident store pages out entirely.
- Paging out costs **nothing** in quality. A proxy costs colour error on
  every frame the artist is judging by. On a 20000-wide canvas most painting
  happens below 1:1, so "only when zoomed out" is not a rare state, it is the
  normal one.
- A proxy is *more* code than eviction, not less: the encoder, the promotion
  and demotion policy, the zoom threshold, and a second thing the composite can
  be reading.

So there is no role for lossy compression of layer pixels, at any zoom, under
any policy. **The one place a lossy reduced-resolution copy is right, Umber
already builds**: `thumbnail.wgsl`'s 64-square layer thumbnails, which are
explicitly a picture of the layer and never a source of pixels.

## 7. Lossless representations cheaper than 4 B/px

Four ideas, one of which is worth having.

**Uniform tiles — worth having, and it belongs in the tiling design.** Umber
already proves the observation on the CPU: `PieceBytes::Flat` holds a piece
whose pixels are all identical as that one pixel, and the scan stops at the
first pixel that differs, so busy paint pays four comparisons to be told it is
not flat. A tiled layer store gets the GPU analogue nearly free — a tile table
with a "solid colour" entry alongside a tile handle. An empty layer becomes a
table; a flat fill becomes a table; a sketch layer becomes a handful of tiles.
This is the single largest lossless win available and it is not a format
change, it is a *storage* change, so §12 hands it over with the one figure that
would size it.

**Greyscale detection — refused.** A grey layer could live in `Rg8Unorm`
(2 B/px, renderable, blendable, in the guaranteed set) as value plus alpha.
Refused on the failure mode rather than the saving: the trigger is one coloured
pixel, so the promotion happens *mid-stroke*, and a promotion is a fresh
canvas-sized allocation and a copy in the middle of somebody painting — with
both textures alive, which is the moment memory is tightest. It also cannot be
an array slice, so it is §5's second-array problem again for a narrower class
of layer, without §5's precision argument to pay for it. Tiling gets more, for
less, without a state machine.

**Indexed / palettised layers — refused.** No renderable indexed format, and a
paint layer is not palettisable in the first place.

**Hardware delta colour compression — nothing to do.** Modern GPUs already
compress framebuffer traffic losslessly and transparently. It saves
*bandwidth*, not footprint; the allocation is the uncompressed size. Worth one
sentence so nobody proposes it as a saving.

### 7.1 One thing the width makes much worse, handed over

`ensure_slots` allocates the grown array and copies the old one into it with
**both alive**. At 400 MB a slice, adding the 25th layer of a document one
layer at a time is 25 + 26 = 51 slices = **20.4 GB transient** on a 10 GB card,
which is a `create_texture` failure, which is an uncaptured device error, which
is fatal.

Note that the growth *policy* is behaving correctly here and is not the fault:
`GROWTH_DOUBLING_BUDGET_BYTES` is 256 MiB, so at 400 MB a slice both
`initial_slots` and `growth_quantum` degenerate to 1 and the array grows
exactly, one slice at a time, with no speculation at all — which is what the
budget is for. The problem is that a monolithic array cannot grow without
copying itself, and 4 B/px is what makes the copy unaffordable. It is
allocation accounting and it is §12's.

---

# Part (b): host memory

The host side is in worse shape than the GPU side, and unlike the GPU side
almost all of it is contained work with no quality consequence at all.

## 8. The import materialises everything before anything is uploaded

### 8.1 What happens now

`ImportedLayer::pixels` is `width × height × 4` bytes, and
`ImportedDocument::validate` enforces exactly that (`expected = w * h * 4`) for
every non-folder entry. The `.clip` reader's `colour()` allocates
`vec![0u8; canvas.x * canvas.y * 4]` per layer and blits the source bitmap into
it — and the source bitmap **is its own rectangle**, in 256-square zlib blocks,
often far smaller than the canvas and sometimes hanging off it. So the reader's
first act is to inflate already-sparse data to full canvas size.

`check_bounds` runs *before* any decoding, so an over-budget document is
refused with a sentence rather than an out-of-memory kill. That part is right
and is not a bug. But it means:

> **`MAX_TOTAL_BYTES` is not a safety margin. It is a promise that Umber will
> allocate up to 16 GiB of host RAM to open a file, and then hold it.**

At the reference canvas that is 42 painted layers admitted and 16.8 GB held at
peak. And the peak is not shortened at the end: `open()` *moves* each
`pixels` into a `LayerUpload` (no copy, good), but `app.rs` then iterates
`for upload in &uploads` — a borrow — so every layer is still resident while
the last one is being written to the GPU.

### 8.2 What prevents streaming, and what does not

Very little, and the shape is already present.

**Not a blocker: `open()` returning an `Opened`.** It returns the stack and the
history together because a saved history's stack positions become texture slots
and the slots do not exist until the stack does. That is a **metadata**
dependency, not a pixel one. `open()` already does the split internally — one
loop builds the stack from layer *descriptions*, a second loop pushes uploads.
Nothing in the history mapping reads a pixel.

**Not a blocker: the thread boundary.** `loading.rs` already decodes on a
worker and uploads on the main thread, because only the drawing thread has the
device, and it already has an `mpsc` channel and an `EventLoopProxy` wake.
Carrying `LayerUpload`s across that channel one at a time instead of one
`ImportedDocument` at the end is the same machinery.

**The actual blocker is the type.** `ImportedDocument` is a value holding
`layers: Vec<ImportedLayer>`, produced whole by each of five readers and then
validated whole. Streaming means splitting it into a *description* — names,
flags, depths, effects, text records, background, the history — and a
*sequence* of `(slot, pixels)`. Every reader loops over layers already (that is
what makes `loading.rs`'s progress bar honest), so each reader's change is to
yield rather than to push.

**The payoff.** Peak host memory for an import drops from *every layer* to
**one layer**: 400 MB instead of 16.8 GB at the reference canvas. And
`MAX_TOTAL_BYTES` stops being a host bound at all and becomes purely a VRAM
bound — at which point it should be re-derived against the device's actual
memory rather than against a fixed 16 GiB, which is a better refusal than the
one being given now.

### 8.3 The source rectangle, and the dependency to flag

Preserving the source rectangle rather than inflating to canvas is the *second*
half of this, and it feeds directly into tiling:

- A `.clip` layer's pixels arrive as **256-square blocks**
  (`csblocks::BLOCK == 256`). Umber's damage grid is 64 (`damage::TILE`), which
  is 16 damage cells to a Clip Studio block.
- If the tiled layer store uses **256** as its tile side, a `.clip` import
  becomes: decode one block, write one tile, drop the block. No canvas buffer
  at all, at any point, for any layer — and no inflation of a layer that hangs
  three quarters off the page.
- It is not decisive on its own — the composite and the damage grid have their
  own opinions — but it is free alignment with the format that is causing the
  trouble, and it is worth putting on the scales.

### 8.4 The GPU staging accumulation, which neither draft had counted

Confirmed at `app.rs` ~4450: `install_import` loops `write_layer_rect` per
layer with **no `queue.submit` in or after the loop**.

`write_layer_rect` reaches `Queue::write_texture`, and in wgpu 29 that allocates
a fresh `StagingBuffer` of the whole copy size and hands it to `PendingWrites`,
which is flushed at the next submit. `StagingBuffer::new` calls the hal's
`create_buffer` directly, so `max_buffer_size`'s 256 MiB does not apply and a
400 MB write succeeds — which is why this works today and exactly why it
accumulates.

So a 21-slice open at the reference canvas is 8.4 GB of layer array **plus
8.4 GB of live staging**, both alive until the next frame's submit. That roughly
halves the number of layers a card can open: nearer **ten or eleven** slices
than twenty-five.

**And this path's OOM is fatal by construction.** `StagingBuffer::new` maps its
errors through the fatal `handle_hal_error`, which loses the device on
`OutOfMemory`. No error scope, no budget threshold and no refusal at
`ensure_slots` can rescue it, because the failure is not at `ensure_slots`.

**The fix is one line and it should be recommendation 1**: submit inside the
loop, every layer or every few layers. It costs nothing, it needs no new API,
and it bounds the staging at one or two canvases.

**Where I disagree with the review, precisely.** Finding 5 concludes that
"streaming the import fixes the *host* side and does nothing at all about the
staging unless the upload loop also submits. Streaming without submitting swaps
one 16.8 GB peak for another." That is right about the loop as it stands and
wrong about streaming as §8.2 specifies it, and the difference matters to
whoever implements it.

`submit_frame` submits **once per frame**, unconditionally. The staging
accumulates today because `install_import` does the entire upload loop *inside
one frame*. A stream that hands layers across the channel and uploads a bounded
number **per frame** has its staging flushed by that frame's own submit, with
no explicit submit anywhere and no new code — for the same reason the autosave
capture's 4 MB-per-frame budget bounds its own staging. Peak staging becomes
"what one frame uploaded", which is the same bound the explicit submit gives.

So: streaming does fix the staging, **provided the per-frame drain is bounded**
— and it must be anyway, because an unbounded drain would also put every layer
of a large import through `write_texture` in one frame, which is the hitch
`Capture` exists to avoid. The two findings agree on the requirement and differ
only on whether it needs a separate mechanism. It does not.

The explicit `queue.submit` is still the right *immediate* fix, and it stays
recommendation 1: it is one line against a redesign, it works today, and it
protects the fatal path now rather than after the import is restructured.

## 9. Undo

**The honest position: undo is not the host-memory problem at this canvas, and
the thing that would fix its remaining case is not compression.**

A patch is the *cells* a stroke reached — `damage::TileMask` on a 64-pixel
grid, neighbours merged along each row, every piece clipped to the bounding box
— so a thin diagonal across a 10⁸-pixel canvas is 6.8 MB where the bounding box
would have been 381 MB. A piece whose pixels are all identical is held as that
one pixel. That is already most of a tiled patch, and it was already built.

What remains is what CLAUDE.md already states and I am not going to soften: a
wash that genuinely covers the canvas is 400 MB of pixels however they are
described, the 512 MB default budget holds exactly one, and the second ages the
first out. That is correct behaviour that is indistinguishable from a bug, and
the panel says which figure is in force for exactly that reason.

**In-memory compression stays refused, on the existing measurement — I have not
re-derived it and it should not be re-derived.** `examples/measure-history.rs`
puts PNG at the fast level at about 1.6 ms/MB, so a 400 MB patch is roughly two
thirds of a second of encode added to every pointer-up and as much again to
decode on undo, for a factor that does not change the order of magnitude tiling
already delivered. Cite it; do not re-measure it.

**Tiling makes the noted future fix enormously cheaper, not marginally — and
this is the part worth saying loudly.** CLAUDE.md's standing note is "a patch
that stores *tiles* rather than the stroke's bounding box". If the sibling's
layer store is tiled with **immutable, reference-counted tiles**, an undo entry
stops being a pixel copy *entirely*: the pre-edit tiles are still alive because
the edit allocated new ones, so the entry is a table of tile handles. A
full-canvas wash's undo entry becomes kilobytes, the blocking
`read_layer_pieces` at pointer-up disappears, and the 512 MB budget becomes a
budget on *retained tiles* rather than on copied bytes.

That is a large claim about somebody else's design and I am not making it their
requirement. What I am saying is: **copy-on-write tiles are worth evaluating
specifically because of what they do to undo**, and if the tiling design is
being written without that on the table it should be put on it. The measurement
that would size it: over the user's real documents, what fraction of a stroke's
damaged tiles are actually rewritten versus merely touched.

## 10. Everything else canvas-sized on the host

Named, with its peak at the reference canvas.

### 10.1 The autosave capture — the largest honest number in this document

```rust
pub struct DocumentCapture {
    pub size: UVec2,
    pub layers: Vec<Vec<u8>>,   // one canvas-sized RGBA buffer per slot
    pub merged: Vec<u8>,        // one more
}
```

`Capture` reads one slice at a time, in bands, through one reused staging
buffer — that part is careful and it bounds the *transfer*. What it does not
bound is the **assembly**: `results: Vec<Option<Vec<u8>>>` accumulates every
slice and nothing is released until the whole document is home and handed to
the encoder thread.

Peak, for a 20-layer document with 4 masks at the reference canvas:

| | |
|---|---|
| 24 slices + merged, at 400 MB | **10.0 GB** |
| plus `trim`'s per-layer copy, up to canvas-sized | +0.4 GB |
| plus every layer's PNG, accumulated | + |
| plus the whole ZIP as one `Vec<u8>` | + |

**This should be read as the strongest recommendation in part (b) and it was
under-sold in the first draft.** Every other figure here is paid by somebody who
asked for something — an open, a save, a copy. This one is paid every five
minutes, unattended, by a painter who is doing nothing but painting, on the
documents that motivated the whole investigation. It has no quality trade
anywhere in it: nothing about fixing it touches a pixel. And it is the one path
where a failure is invisible until it is total, because an autosave that could
not allocate is a `Report::Failed` that says so once and carries on — the
artist finds out at the crash, or at the next start, or not at all.

The review did not challenge the figure and independently confirmed the type
and the accumulation.

**Three contained fixes, in order of value:**

1. **Encode each slice as it comes home and keep only the PNG.** The encoder is
   already on a thread; the pipeline becomes capture-one → encode → drop the
   raw buffer. Peak falls from *N+1* canvases to **one** canvas plus the
   accumulated PNGs. The work is that `docformat::encode` takes a
   `SaveDocument` borrowing every layer at once, so it would need an entry-at-a-
   time form — a real interface change, and the only one of any size here.
2. **Stream the archive to the temporary file** rather than building it in a
   `Vec<u8>`. A ZIP writer can write entries as they arrive, and
   `write_encoded` already does temp-and-rename. This removes the accumulated
   PNGs too.
3. **Capture masks at one byte.** A mask is read back at 4 B/px and then
   reduced by `chunks_exact(4).map(|px| px[0])` — a canvas-sized allocation to
   discard three quarters. §5's dedicated `R8Unorm` array removes **three
   quarters of the readback** as well as three quarters of the slice.

   *(The first draft claimed three quarters while recommending packing, where
   the true figure was two thirds — the review caught the contradiction and it
   was a real arithmetic error. Under the revised recommendation the saving is
   three quarters again, because a forked mask is read back at 1 B/px rather
   than 1.33. The figure is right; it was right for the wrong design.)*

**What has been built, as of Stage 4.** Fix (2) is done and fix (1)'s
*interface* is done — `docformat::Canvas`/`Canvases`, the entry-at-a-time form
this section called the only real interface change here — but **the table above
still stands for the autosave**, and the reason is worth being exact about.
`DocumentCapture` arrives whole from `CanvasRenderer::take_capture`, so the
twenty-four slices are resident before the writer thread starts; what fix (2)
removed is the two rows *under* them, the accumulated PNGs and the whole
archive. Encoding a slice as it comes home needs the renderer to release
finished slices one at a time, which is `canvas.rs` and therefore Stage 3's.
Fix (3) is §5's and is handed to the atlas. `docs/perf/roadmap.md`'s Stage 4
entry records the same split.

### 10.2 The explicit Save — the same shape, synchronously

`app.rs` builds `pixels: Vec<Vec<u8>>` and `masks: Vec<Option<Vec<u8>>>` by
calling `read_layer_rect` per slot, then `export_rgba` for the merged image,
and holds all of them while `docformat::encode` runs. Its own comment says so
and quotes 2048² figures ("16 MB each ... a few hundred megabytes"); at the
reference canvas the same sentence reads **10 GB**. Fix (1) above serves both
paths, which is the argument for doing it once in `docformat` rather than twice
at the call sites.

**Done.** `SaveSource` reads one slice off the GPU as the archive reaches it,
so nothing here follows the layer count any more. What is resident per layer is
the fetched buffer *and* `trim`'s content-rectangle copy — the row this section
already listed separately — so it is about two canvases rather than one, and
`crates/umber-core/tests/save_peak.rs` is what measures it rather than
reasoning about it: 27 KB per extra layer, which is an XML element and a ZIP
directory record and nothing canvas-sized.

### 10.3 Export

`export_rgba` returns one canvas-sized `Vec<u8>` (400 MB) on top of the 400 MB
GPU target, and the encoder's output on top of that. Bounded at roughly two
canvases, which is the honest floor for "flatten and encode" and is not worth
attacking before §10.1 and §10.2.

### 10.4 The clipboard

`Clip` holds `width × height × 4` straight-alpha sRGB. `Ctrl+C` with nothing
selected on the reference canvas:

| | |
|---|---|
| `read_layer_rect` of the whole canvas | 400 MB |
| the `Clip` itself | 400 MB |
| a cut's complement buffer | 400 MB |
| `arboard`'s own copy to the desktop | 400 MB |
| **plus** the macOS echo, where `TRANSPORT_IS_EXACT` is false | 400 MB |

About 1.6 GB, or 2.0 GB on macOS, and — on the recorded 2.5 ms/MB write —
about a second of blocking with nothing on screen saying so. CLAUDE.md already
records the second and not the first.

`Editor::clipboard` then **holds its 400 MB until the next copy**, which is the
part worth naming: it is not a transient. The cut's own patch is the rectangle
rather than cells, which CLAUDE.md already says out loud, so a bare `Ctrl+X`
also spends 400 MB of the undo budget and the budget holds exactly one.

### 10.5 Loading

While `loading.rs` is decoding, the worker holds a whole `ImportedDocument`
(§8), the channel then carries it, `open()` moves it into `Opened`, and the
uploads stay resident through the upload loop. One document's worth, not
several — but that one document's worth is the 16.8 GB of §8.1, and §8.4's
staging sits on top of it on the device.

## 11. What I could not settle from the code

Named individually, with the measurement that would settle each.

1. **Whether the per-dab colour scratch can be `Rgba8UnormSrgb`** (§3). Worth
   400 MB per smudging stroke. Settled by the build-up accumulation study run
   against a smudging preset over a dark painting, including a stroke scrubbed
   back and forth over its own wet paint so `probe_canvas`'s pickup is covered.
2. **The exact collision count in `srgb::coverage_table`** (§5.2). I derive
   about 182 distinct outputs of 256, so roughly 74 states lost. It is a
   three-line check over `coverage_table()` and it should be run before the
   figure is quoted anywhere else, because it is now load-bearing for §5's
   recommendation.
3. **What fraction of tiles in the user's real documents are uniform** (§7).
   Decides whether the flat-tile representation is a large win or a small one.
   A CPU pass over the already-decoded layers — `survey-documents` is the
   natural place.
4. **How much of a stroke's damaged tiles are actually rewritten** (§9).
   Decides whether copy-on-write tiles collapse undo or merely shrink it.
5. **The real VRAM figure for the user's documents.** Everything here is
   derived from format constants; nothing is an allocation trace. Every texture
   in §3 carries a label already, so `generate_allocator_report` answers it
   directly, and it is the difference between "25 slices" and "eleven slices
   and an outline".
6. **Whether the staging accumulation is exactly one canvas per layer** (§8.4).
   The mechanism is confirmed from the vendored source; the multiplier is not
   measured, and it is the largest unmeasured number on the exact path the user
   reported.

Two facts I flagged in the first draft as load-bearing and unverified have
since been checked and are **true**: there is no single-channel sRGB format in
the guaranteed set, and `Rgba8UnormSrgb` is the only useful member of the
renderable, blendable, 4-channel, `Features::empty()` set. §5.2 records how the
first of those turned out to support the opposite conclusion from the one I
drew from it.

## 12. Handed over to the siblings

- **Tiling.** The uniform-tile representation (§7) is the largest lossless win
  available and belongs there, not here. A 256-pixel tile aligns exactly with
  Clip Studio's own block size (§8.3). Copy-on-write tiles are worth evaluating
  for what they do to undo (§9). A mask tile is one byte and linear (§5.5).
- **Residency.** The effect seed planes are 800 MB of full-resolution
  `Rg16Uint` per outline bake and cannot be narrowed (§3) — they are transient,
  so they are a residency question. So is the float's pair of canvas-sized
  textures, and the flip and export scratches.
- **Allocation accounting.** `ensure_slots` holds both arrays alive across the
  copy, which at 400 MB a slice is 20.4 GB transient on the 25th layer and is
  fatal (§7.1). The growth *policy* is correct at this scale; the monolithic
  array is what makes the copy unaffordable. Parked slices are charged to the
  undo budget and `capacity` never shrinks, so one delete-and-add cycle holds
  400 MB for the session. **And §8.4's staging belongs in that document's
  transient arithmetic**: `c + n` slices is not the peak on an import.

What stays mine, in priority order: **§8.4's one-line submit**, **§10.1 the
autosave's 10 GB**, **§8.2 streaming the import**, **§5's linear `R8Unorm`
masks — built into the tiled store, not before it**, and **§3's colour-scratch
measurement**. The first three are contained work with no quality consequence
whatever, and they are larger numbers than anything on the card.

## 13. What the review changed

For anyone holding the first draft.

**Accepted and folded in.**

- The four uncounted paths in the packing proposal — channel-granular
  `SlotPool`, `fill_layer_white`/`clear_layer`, `slot_revision`
  cross-invalidation, and `ColorWrites` being pipeline state including on the
  commit. All four verified against the source. §5.4.
- The "three quarters" contradiction in §10.1(3). It was two thirds under
  packing; it is three quarters again under the revised recommendation, and it
  was still an error. §10.1.
- The staging accumulation and its fatal OOM path, which neither document had
  inventoried. It is now recommendation 1. §3, §8.4.
- The `probe_canvas` condition on the colour-scratch measurement. §3.

**Where the review changed the verdict rather than the costing.** Counting the
four paths sent me back to §5.2's premise, and the premise was wrong in a way
neither the review nor the first draft had noticed: a mask is a multiplier on
alpha, this codebase already holds that alpha is not gamma-encoded, and the
mask's sRGB typing is inherited from the array it borrowed rather than chosen
for it. It costs about 74 of 256 states on every imported mask. So the
recommendation moved from packing to a dedicated **linear** `R8Unorm` array,
which is the option the first draft argued *against* on quality grounds. §5.2.

**Disputed.** One thing, and narrowly. Finding 5 concludes that streaming the
import "does nothing at all about the staging unless the upload loop also
submits". That is right about the loop as written and wrong about streaming as
§8.2 specifies it: `submit_frame` submits once per frame unconditionally, so a
stream with a bounded per-frame drain has its staging flushed by the frame's own
submit, with no explicit submit and no new code. The two findings agree on the
requirement — the drain must be bounded — and differ only on whether it needs a
separate mechanism. It does not. The explicit submit remains recommendation 1
regardless, because it is one line against a redesign and it protects the fatal
path today. §8.4.

**Unchanged and independently confirmed.** The BC7 refusal on both mechanical
grounds and the eviction-dominance argument behind it; that undo is not the
problem at this scale and `measure-history.rs` should be cited rather than
re-run; the 256-pixel tile alignment argument; the 10 GB autosave figure; and
the ordering recommendation, which the review judges right and which §5.4's
extra paths make more so rather than less.
