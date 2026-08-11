# Sparse layer storage

Every layer in Umber occupies a full canvas-sized slice of one texture array.
At 20000×5000 that is 400 MB a layer whether the layer holds a portrait or a
signature, and a 54-layer document is 21.6 GB before a mask, a float, an effect
or a scratch buffer. This is what stops Umber opening real Clip Studio
documents, and it is the single architectural change that would most widen what
it can hold.

This document is the design for replacing the array of canvas-sized slices with
a **tile atlas plus an indirection table**, so that a layer costs what it
covers. **It is built, in both stages** — §0a is the record of what that turned
out to be and where it departs from the rest of this file, and it is the section
to read before any other. What is here below it is the shape, the arithmetic,
the places it touches, the ways it could damage the picture and how each is
prevented, and the measurements that were taken before any of it was written.

---

## 0a. What was built, and where it departs from this document

**Both stages are in.** `umber_core::tile` is the arithmetic, `shaders/tiles.wgsl`
is the shader-side unpack, `LayerStore` is the atlas and the page table, and the
allocator is `back_tiles`/`promote`/`release_slot` on `CanvasRenderer`. Read this
section before the rest of the file: several of the decisions below are
deliberate departures, and taking the design's own wording for the code is the
"stale instructions" failure `CLAUDE.md` records.

**Measured, on an RTX 3080 over the artist's 33 real documents**, by
`umber-core`'s `measure-atlas` (the reservation arithmetic, no device) and
`umber-render`'s `measure-vram` (the real renderer, the real upload):

| | dense | a page a slice | atlas |
|---|---|---|---|
| `Valorants magical bitches.clip`, 20000×5000, 53 slices | 19.74 GB | 20.44 GB | **1.54 GB** |
| all 33 documents | 55.26 GB | 57.64 GB | **10.44 GB** |

The motivating document opens: four pages, 5,412 tiles backed of a dense 83,740
(6.5%), uploaded in 703 ms, 908 cells left free in the pool. That is not a
projection — `measure-vram` puts it on the card.

**The regression is in the same table.** Five documents come out *above* 100% of
dense, at 104.9%, and that is the page padding on layers covering their whole
canvas. See "What sparse residency does not do to the padding" below: it follows
how far a dimension is from a multiple of 256, so it is 4.9% at 3000 square and
**26.4% at 1920×1080**. The two 1920×1080 documents in the corpus still measure
32.8% and 36.1% of dense, because their layers are not full.

- **There is no apron, and `A` does not exist.** §8.1's fallback — refusal 7, the
  hand-reconstructed bilinear tap — is what shipped, and §8.3's mark-and-refresh
  is therefore not built and not needed. What decided it was not the ranking in
  §8.1 but §3.5's consequence: an apron makes a tile's pitch 258, and a page
  whose pitch is not the tile cannot be the canvas rounded up, which is the next
  bullet. `a_tap_across_a_tile_boundary_blends_the_logical_neighbour` is the
  guard, and it needs the page table deliberately rearranged to say anything at
  all — under the identity, adjacent logical tiles are adjacent in the atlas too.
- **A page is the canvas rounded up to whole tiles, not a fixed 16×16 grid.**
  §3.1's free parameter is gone: a page holds exactly one layer's worth of tiles,
  so a page *is* what a slice was and §3.1's growth correction, `try_reserve`,
  `Vram`, `resize` and every `MAX_SLOTS` figure are unchanged rather than
  re-derived. The ceiling is therefore today's ceiling and not 17.18 GB. The
  costs: the page-side sweep §10 asks `measure-atlas.rs` for is not a question
  any more, and the dense-layer penalty is **per axis** rather than §4.2's flat
  5.2% — which makes it worst on a *small* dimension and best on the canvas this
  programme is about. 3.5% at 20000×5000, 5.4% on an A4 page at 600 dpi, 6.7% at
  2560×1440, **26.4% at 1920×1080** and 63.8% at 800×600. 1080 is 4.22 tiles, so
  the most ordinary canvas anybody paints on is the worst realistic entry, and
  `growth_quantum` reads that figure — a 1920×1080 document's quantum falls from
  32 slices to 25. `canvas.rs`'s `slice_bytes` carries the table. Quoting the
  3.5% alone, which a first draft of this section did, is "state the figures one
  by one" broken in the document a later reader takes for instructions.
  **It rests on one property**: rounding a canvas up to a multiple of 256 cannot
  cross `max_texture_dimension_2d`, because every value that limit takes is
  itself a multiple of 256. That is true of every adapter anybody has measured
  and it is not something the specification promises.
  `rounding_a_canvas_up_to_tiles_never_passes_the_device_limit` sweeps the real
  figures; the residual is a device reporting, say, 5000, where a 4900 canvas
  would want a 5120 page and `create_texture` would refuse it *fatally*. The fix
  if one ever appears is to round `CanvasLimit::of_device` **down** to a whole
  tile — and to route `install_import`'s own direct check through the same
  function, which today keeps a second copy of that comparison.
- **Vertex shaders were not taught about pages.** A page is larger than the
  canvas and every vertex shader that writes a layer maps document pixels to clip
  space through `doc_size`, so `aim_at_document` sets a viewport instead. Three
  passes take it; the effect passes already set their own. Under stage 2 a pass
  targets a *tile*, and a viewport cannot express that — the offset can be
  negative — so that is where the uniform field §5.3 describes has to arrive.
- **`transform.wgsl`'s `fs_mask` reads with `textureLoad`.** It sampled the layer
  at `doc / doc_size`, which stopped being where the texel was. Integer is what
  a 1:1 quad wanted anyway, and it is the same argument `fs_blend` and
  `flip.wgsl` already make.

### What sparse residency does *not* do to the padding, which is the premise to correct first

The obvious reading of "a page is the canvas rounded up, and unbacked tiles cost
nothing" is that residency retires the padding. **It does not, and a
fully-painted layer still pays all of it.** A page is `tiles × 256` and *every*
tile slot in it is a real tile of the canvas grid — there are no padding tiles,
only padding *inside* the edge tiles. On 1920×1080 the grid is 8×5 and the
rightmost column covers document x 1792..1919, 128 of its 256; a layer touching
that column backs the whole tile. So a layer covering the canvas costs 2.62 Mpx
against a dense slice's 2.07, at every occupancy of 100%.

What residency retires is the padding **in proportion to sparsity**, which for a
corpus at 13.5% is nearly all of it and for a background fill layer is none. The
honest statement is that stage 2 wins overwhelmingly on the documents this
programme is about and leaves one bounded regression — a fully-painted layer on
a canvas whose dimensions are far from a multiple of 256 — which is the price of
free relocation and is the thing to weigh if anybody proposes partial edge
slots.

### What stage 2 did, and the five places it departs from the plan above

Every numbered item of the plan this section used to hold is built. What follows
is what it turned out to be, in the same order, with the departures marked —
because the plan is what somebody will read next and four of its sentences are
now false.

1. **A tile allocator over pages, and page-backed slots.** `PageUse` is `Pool` or
   `Owned(slot)`; `free` is the pool's cells as `Entry`s; `back_tiles` hands them
   out, `promote` takes a whole page identity-mapped, `release_slot` gives
   everything back. The page table is `MAX_SLOTS` deep from the moment the store
   exists and is never grown, exactly as the plan says, and `ensure_slots` is
   therefore an assertion and nothing else.

   **The departure: `try_ensure_slots` did not become vacuous, it became a
   headroom check.** The plan says a blank layer costs nothing so `add_layer`'s
   refusal has nothing left to refuse, and moves it to the first stroke. That is
   right about the cost and wrong about what to do: a gate that goes quiet
   exactly when the card is full is worse than no gate, and §9.5's stroke-time
   refusal is still the thing with no good answer. So `try_ensure_slots` reserves
   a *page of headroom in the pool* — enough free cells that the first stroke on
   the new layer cannot be the thing that meets the ceiling — which keeps the
   refusal live and its sentence true, and is idempotent, so sixty-four blank
   layers grow the atlas once rather than sixty-four times.

2. **`write_layer_rect` backs the tiles it writes**, with no emptiness scan, for
   the reason the plan gives and the roadmap's §2.1 argues.

3. **The commit is per (piece ∩ tile)**, `CommitUniforms` gained `atlas_delta`
   and `target_size`, `out.doc` stayed in document space, `commit_layout`'s
   binding 0 took a dynamic offset and `aim_at_document` came off both commits.
   The plan's estimate of ninety lines and ten of WGSL was close.

   **One addition the plan does not have: the *blended* commit is cut the same
   way**, which bounds its backdrop copy at one tile rather than at
   `canvas width × 64`. That is a straight improvement to §5.4's figure and it
   arrived for free, because both commits now go through one `commit_aims`.

4. **The readbacks and the capture synthesise.** `read_layer_pieces` fills its
   output with `SlotClass::empty_bytes` before any copy lands and copies only
   backed fragments; `read_layer_rect` is one call to it. The capture clears its
   band buffer and copies backed fragments into their own columns.

   **The departure: the capture cannot do a sparse *mask*.** `clear_buffer`
   writes zeroes, which is a layer's empty value and not a mask's, so a partly
   backed mask would come back black where nothing was stored. It is unreachable
   — every mask a save or an autosave reads is fully backed, from an import's
   single canvas piece or from a stroke — and there is a `debug_assert` saying
   so rather than a comment claiming it cannot happen. A mask at one byte,
   §5's item, is where that gets closed.

5. **`clear_layer`, `fill_layer_white` and `clear_all_layers` are table writes**,
   and `fill_layer_white` is the one that is more than a tidy-up: full reveal
   *is* a mask slot's empty value, so **a new mask now costs no storage at all**
   until somebody paints on it. That is half of §5 arriving as a side effect of
   `SlotClass` existing.

6. **`upload_table` is per slot** on every path a drawing frame takes.

7. **`resize` needs no scratch**, exactly as the plan says: clip, shift,
   `copy_texture_to_texture`.

   **The departure: the flip did not promote.** The plan offers "promote and use
   today's code, or teach `flip.wgsl` the page table". Promotion is *fatal* here
   — it is every layer at once, which on the motivating document is the 19.7 GB
   the whole exercise exists to avoid, reached by pressing a key. So
   `flip.wgsl` reads the page table, through a raw non-sRGB `D2Array` view, into
   a **page-sized scratch laid out at identity positions** — which is what lets
   one pass do a whole slot rather than one pass per tile.

   **And a cost the plan does not anticipate: a flip coarsens residency.** All
   that is known about a source tile is that it is backed, not where inside it
   the paint is, so a 256-wide source tile mirrored onto a canvas that is not a
   whole number of tiles lands across *two* destination tiles. The picture is
   exact — `a_flip_mirrors_a_sparse_layer_and_flipping_twice_restores_it_exactly`
   compares every byte — and the storage is an over-approximation that at most
   doubles per flip and is bounded by the grid. Nothing short of a whole-layer
   readback can do better.

8. **§9.5's refusal is unchanged and is still the thing with no good answer.**
   `back_tiles` grows fallibly; where the device refuses, the tiles that could
   not be backed are logged and skipped and the stroke loses them.

### The one hazard that is not in this document at all

**A growth part-way through a caller's encoder loses what was already recorded
into it.** A growth replaces the atlas texture and copies the old one into the
new; recorded on its own encoder and submitted, that copy reads the old texture
*before* the caller's still-open encoder writes to it, so those writes land in a
texture nothing will ever read again.

It is reachable in `render`: `draw_float` writes the float's preview page into
the frame's encoder several statements before `bake_effects` can promote an
effect slice and grow. `commit_stroke`'s own encoder holds only `draw_dabs`,
which writes the scratch, so `finish_stroke` was safe by accident rather than by
rule.

`ensure_pages` and `try_ensure_pages` therefore take an
`Option<&mut CommandEncoder>` — the caller's where there is one, a fresh
submitted one where there is not — and the second half of it is that
`ensure_effect_scratch` must run *after* the promotion, because its bind groups
name the array view a growth replaces and `bound_capacity` can only notice when
it is asked afterwards.
`a_growth_part_way_through_an_encoder_keeps_what_was_recorded_before_it` is the
guard; put the growth back on its own encoder and it fails.

### What is left, and what it is worth

- **A mask at one byte** (§5). `SlotClass` is the hook and it exists; what is
  left is the format and the readers. There is a measured precision *gain*
  available with it — `coverage_table` is non-injective, about 74 of 256 states
  unreachable at the reveal end — but that touches the file format and needs a
  version bump, so it is deliberately not foreclosed and deliberately not built.
- **The autosave's whole `DocumentCapture`** (`formats-and-host-memory.md`
  §10.1's fix 1). `take_capture` still hands the writer thread every canvas at
  once. Releasing finished slices one at a time is the remaining half of "10 GB
  every five minutes", and it is `canvas.rs`'s, which is why it was handed to
  this stage; it did not get done.
- **A transformed layer stops being sparse.** The float promotes its layer and
  the page is not given back at commit, because demoting would need to know
  which tiles are empty and that is a readback. The preview's page *is* given
  back, by `end_float`.
- **An effect slice's page is held until the slot is reused**, which is no worse
  than before — the layer array never shrank either — and is worth noting because
  `EffectCache::forget_all` reads like a release and is not one.

---

## 0. The recommendation, in short

Replace `LayerStore`'s `texture_2d_array` of canvas-sized slices with an
**atlas**: a `texture_2d_array` whose slices are fixed-size *pages*, each holding
a grid of 256-square **tiles**, plus a **page table** — a second, tiny
`texture_2d_array<u32>` indexed by `(tile coordinate, slot)` — saying where each
of a layer's tiles lives, or that it is not backed at all. A tile that is not
backed reads as the layer's empty value, which is transparent for a layer and
white for a mask. `composite.wgsl` gains one `textureLoad` per layer per
fragment and a coordinate transform; `commit.wgsl`'s fragment shaders are
untouched, and only its vertex shader learns where to put the result.

The wet-layer scheme survives whole, and that is not a hope: nothing in this
design changes coverage, stroke opacity, the `max` blend or the blend maths. It
changes only *where a texel is stored*. Residency is decided once per commit,
from `damage::TileMask` — the grid the undo patch already accumulates — so the
allocator's input is a quantity the drawing path already computes without
allocating.

The work splits into a stage that changes nothing observable (the atlas, with
every layer fully resident, guarded by a byte-for-byte identity test against
today's output) and a stage that makes it sparse.

**§3.6 is the load-bearing addition of this revision, and it is small.** The
first draft of this document said residency arrives "on load from what the
importer found", and no such thing existed on either side of the join: the
importer's own design streams *dense canvas buffers*, `install_import` calls
`write_layer_rect` with the whole canvas, and this document's own table said that
path allocates a tile for every rectangle it is given. Under that composition the
atlas backs every tile of every layer on import and stage 2 saves nothing on
exactly the documents it exists for. §3.6 owns the interface and writes its
signature down: a tile-wise upload for the formats that store tiles, and — the
part that makes the composition work with *no* importer change at all — an
emptiness scan inside `write_layer_rect`, so residency exists from the day the
atlas does.

### The three prerequisites, and who owns them

Two other things must be true before the atlas is worth building, and **neither
belongs to this document**. §9.1 used to list them as work to do "before any of
it", which reads as ownership and produces four half-fixes:

- **Streaming the import** is `import-and-limits.md` §7.2. It is what takes the
  host peak from 21.6 GB to one layer. This document's first draft claimed that
  figure for its own "trim the importer" item, which was wrong — trimming changes
  what one layer costs and not how many are alive. §9.1 now says so.
- **The growth transient and the colour-scratch release** are
  `slot-lifecycle-and-vram.md` §4 and §5. The second of these also refutes a
  claim that sibling makes about *this* document: see §11.
- **Hidden layers needing no slice at all** is `layer-residency.md` §0(1), is
  provable rather than argued, and is worth 12 GB on a document with thirty
  roughs in it for nothing but bookkeeping. It is cheaper than everything here
  and should ship first.

**The honest caveat, up front.** Sparse GPU storage does not on its own let the
21.6 GB document open. `ImportedLayer::pixels` is a canvas-sized host buffer per
layer and the whole stack is built before a byte reaches the GPU, so the import
peaks at 21.6 GB of *system* memory whatever the GPU does; and
`MAX_TOTAL_BYTES` is computed from canvas × layers, which is a bound on what a
file *declares* rather than on what it holds. §11 has both.

### What is still open, stated rather than buried

- **Whether the apron can be made structural.** The first draft hung it off
  `touch_slot`, which cannot carry it — that method has no encoder, and
  `render_float` writes a slice from `&self` with its *callers* doing the
  touching, which is precisely the case CLAUDE.md records the "exhaustive by
  construction" claim being false for. §8.3 is rewritten around a mechanism that
  does work, and §8.1 re-ranks the alternative it previously dismissed too
  quickly. This is the single largest open risk in the design.
- **Occupancy.** If real layers are 60% covered rather than 20%, the artist's
  document goes from 21.6 GB to 13 GB, which still does not fit a 10 GB card, and
  the answer is `layer-residency.md`'s eviction rather than this. §10 is the
  survey, and it must report non-empty blocks and not merely present ones.
- **What happens when the atlas is full**, which happens at pointer-up. §9.5.

---

## 1. What a document costs today

### 1.1 The arithmetic

`LAYER_FORMAT` is `Rgba8UnormSrgb`, four bytes a pixel. A slice is
`width × height × 4`, so at 20000×5000 (100 megapixels) one slice is **400 MB**.

| | 2048² | 20000×5000 |
|---|---|---|
| one slice | 16.8 MB | 400 MB |
| the document that prompted this (54 layers) | 906 MB | **21.6 GB** |
| every layer masked as well | 1.81 GB | 43.2 GB |
| stroke scratch, `R8Unorm` | 4.2 MB | 100 MB |
| stroke colour scratch, `Rgba16Float` (a smudging brush) | 33.5 MB | **800 MB** |
| a live float: base + source + preview slice | 50.3 MB | **1.20 GB** |
| the effect working set at its widest | 54.5 MB | **1.30 GB** |
| one baked effect's slice | 16.8 MB | 400 MB |
| a blended commit's backdrop (canvas width × 64) | 0.52 MB | 5.1 MB |

Three of those deserve to be read twice. The **colour scratch** is allocated the
first time a smudging stroke needs it and then held for the life of the
document, by a decision `ensure_stroke_color` argues for on the grounds that
reallocating it per stroke is a stutter — at 2048² that trade is obviously
right and at 20000×5000 it is 800 MB held for ever because somebody once picked
up a blender. The **float** allocates two canvas-sized textures and takes a
third slice of the array for its preview, so pressing T on this document costs
1.2 GB on top of everything else. The **effect working set** is four `R8Unorm`
planes, a fifth for a centred outline, and two `Rg16Uint` seed planes at four
bytes each — 13 bytes a pixel at its widest, allocated as soon as one effect
exists on the document. `EFFECT_LIVE_PIXELS` gates how often the *bake* runs at
that size; it does not gate the allocation.

The layer array itself is not over-allocated at this scale, and that is
`grown_capacity` working: at 400 MB a slice the growth quantum is one, so the
array is exactly as deep as the document needs. At 2048² the same rule allows
doubling, so a nine-layer document holds sixteen slices and 117 MB of them are
speculation — correct, bounded, and not what this document is about.

### 1.2 Where the ceilings actually are, and which one binds

Four separate limits are in play and they are routinely confused.

- **`MAX_SLOTS` is 256**, and it is not ours. `Limits::downlevel_defaults` does
  not name `max_texture_array_layers`, so it inherits `Limits::defaults()`' 256,
  and `using_resolution` raises only the three texture *dimensions*. A 257th
  slice is a `create_texture` validation error, which is fatal. `canvas.rs`
  asserts this at compile time and the comment there is emphatic about why.
- **`max_texture_dimension_2d`** is 2048 by guarantee and whatever the adapter
  reports in practice — measured, 32768 on an RTX 3080 on Vulkan and **16384 on
  Dx12**, 16384 on an Intel iGPU. A 20000-wide canvas therefore cannot exist at
  all on the Dx12 backend of the very card this was reported from.
- **`ImportedDocument::MAX_TOTAL_BYTES` is 16 GiB**, 17.2 GB, which is what
  refused the document with a sentence. It counts `canvas × 4 × painted layers`.
- **Physical VRAM**, which on the reporting machine is 10 GB.

Today the first two are structural and the last two are the ones a painter meets.
Note the ordering: 21.6 GB is past `MAX_TOTAL_BYTES` *and* twice the card, so
even lifting the refusal would trade a sentence for a device-lost crash. That is
worth saying because "just raise the limit" is the obvious first suggestion and
it is strictly worse than the refusal.

### 1.3 The wall is not only the GPU

`ImportedLayer::pixels` is documented as "`width * height * 4` bytes", canvas-
sized, and `ImportedDocument::validate` has a `debug_assert` insisting on
exactly that. Every layer of the stack is built before `open()` is called, so
`docimport::import` on this document allocates 21.6 GB of **host** memory. That
is not hypothetical: `docs/thumbnails.md` records `survey-documents` measuring
12.3 GB for one real file in the same folder, which is why the thumbnailer reads
a stored preview instead.

The autosave has the same shape at the other end: `DocumentCapture::layers` is
one canvas-sized `Vec<u8>` per slot, all held until the file is encoded.

So there are three separate canvas-sized-per-layer costs — GPU storage, import,
capture — and this document is primarily about the first. §11 says what the
other two need, because a design that fixes only the GPU and claims the document
now opens would be false.

---

## 2. The option space, and what the array-layer ceiling kills

### 2.1 A slice per tile — refused

The obvious first thought, and `MAX_SLOTS` ends it in one line. One 20000×5000
layer is 79 × 20 = **1580** tiles at 256 square. The array holds 256 slices
*in total*, for every layer, every mask, every effect and the float's spare.
Nothing about this can be tuned into working; a tile size large enough to fit
1580 tiles into a fraction of 256 would be larger than most layers.

This is the constraint that kills every naive design, and it is worth stating
because two of Umber's own numbers have already been got wrong by assuming
`using_resolution` raises a limit it does not: `MAX_SLOTS` was designed at 257
and would have shipped, and the effect pass budget took its canvas dimension
from the guaranteed 2048 while the real domain was sixteen times larger.

### 2.2 A texture per layer, sized to its content — refused

Attractive: no indirection at all, one rectangle test and an offset. It fails on
the composite. The whole stack composites in **one pass** — `composite.wgsl`
loops bottom to top and CLAUDE.md's instruction is "Do not 'simplify' this into
a pass per layer" — and a loop can index array *slices*, not bindings. Umber
requests `Features::empty()`, so there is no binding array and no bindless
descriptor indexing to reach for.

The variant that keeps the array but sizes every slice to the *largest* layer's
bounds is worse than it looks: a Clip Studio document with one full-page paper
or background layer and fifty-three small ones saves nothing, and that is
precisely the shape of the documents in question.

### 2.3 Sparse or virtual textures — not available

`VK_EXT_sparse_binding` and D3D12 tiled resources are exactly this feature in
hardware, and wgpu exposes no API for either. There is nothing to evaluate.

### 2.4 Block compression — refused on the stated quality constraint

BC7 would be four times smaller and is lossy. `LAYER_FORMAT`'s own comment
argues carefully that eight bits of *linear* storage is not good enough because
a dark ink lands on one or two levels of 255; a codebase that will not accept
that will not accept a block codec on the artist's layers. It is also not a
render target, so every commit would become decompress-modify-recompress.

### 2.5 A tile atlas with an indirection table — the recommendation

A layer becomes a set of fixed-size tiles. Tiles from every layer live together
in a small number of large **pages**; a **page table** says where each logical
tile is, or that it is not backed. This is what Photoshop, Krita and every
virtual-texturing system do, it needs no optional feature, and it leaves the
one-pass composite intact at the cost of one indirection per layer per fragment.

The rest of this document is that design.

---

## 3. The storage

### 3.1 The atlas

A `texture_2d_array` in `LAYER_FORMAT`, with `LAYER_FORMAT_LINEAR` in its
`view_formats` — the flip pass still needs a raw view, for the same reason it
needs one today, and its exactness argument is unchanged.

A page holds a `16 × 16` grid of tiles. With a tile of 256 and an apron of one
texel (§3.5) the pitch is 258, so a page is **4128 square, 68.2 MB**, of which
67.1 MB is tile interior.

**Sixteen a side is a free parameter and this document does not know the right
value.** The only hard constraint is that the page side be no larger than
`max_texture_dimension_2d`, which on the reporting card is 32768 on Vulkan and
would permit 127 tiles a side. Sixteen was chosen for a 68 MB allocation
granularity, which is a guess about the right growth step and nothing more.
`measure-atlas.rs` should sweep it. What the page side *must* be derived from is
the device, not a constant — the shape `CanvasLimit::of_device` already keeps.

### Growth: the atlas is a texture array, and it inherits the transient

The first draft said "pages are allocated one at a time, so growth is in 68 MB
steps rather than `grown_capacity`'s 400 MB ones — a second, quieter benefit".
**That was wrong, and it reintroduced the policy `grown_capacity`'s own doc
comment records as measured and refused.** The atlas is a `texture_2d_array`,
because §2.2's argument requires the composite to index pages from a loop, and
growing a texture array means creating a new one and copying with the old still
live — which is exactly what `ensure_slots` does. Exact growth is 112 growths and
134 GB copied on the case that comment works through, and one page at a time
would be the same policy at six times the frequency: roughly 300 pages for the
artist's document, `68 MB × n(n+1)/2` ≈ **3 TB** copied, and 300 multi-gigabyte
requests with the old atlas live, each of which is a fatal uncaptured error if it
fails.

Three corrections follow, and the third is the interesting one.

1. **The atlas answers to `grown_capacity`'s byte budget, exactly as the layer
   array does.** At 68 MB a page the doubling budget allows 1 → 2 and then a
   quantum of three, so a 96-page document is 32 growths rather than 96, and the
   aggregate copy is ~108 GB rather than 3 TB. The sentence about 68 MB steps
   should have been about the *allocation granularity of a tile*, which is what
   tiling genuinely improves, and not about the texture.
2. **The transient is not retired, it is scaled.** `slot-lifecycle-and-vram.md`
   §4.2 is right that `c → c + n` needs `2c + n` live and that this is inherent
   to a single texture array. Under tiling the peak is `c + quantum` *pages* of
   68 MB rather than `2c + 1` *slices* of 400 MB, and `c` is measured in content
   rather than in canvases — so the transient shrinks by the occupancy ratio and
   by the page-to-slice ratio, and it does not disappear. §11 records that this
   contradicts that sibling's §10.
3. **The import should size the atlas up front, and that is §3.6's deliverable
   again.** `slot-lifecycle-and-vram.md` §4.2 already recommends
   `CanvasRenderer::for_document` take the slot count so a document being opened
   is built at its final capacity rather than grown to it. The tiled analogue is
   the *page* count, which is knowable from the residency the upload path
   carries — so a document that arrives with its residency known allocates once
   and copies nothing. The two findings resolve together.

### The ceiling, and what it depends on

256 pages × 256 tiles × 65,536 px × 4 B is **17.18 GB**. The first draft called
that "`MAX_TOTAL_BYTES` to the byte, by coincidence" and that is generous: both
are 2³⁴, and the atlas figure is an artefact of the arbitrary 16×16 page. At
32×32 tiles a page — legal at 8256 square on the reporting card — the ceiling is
**68.7 GB**. So the ceiling is `256 × tiles_per_page × 65,536 × 4` and is a
consequence of a parameter nobody has measured, not a property of the design.

What *is* a property of the design is what the number measures: today 16 GiB
bounds canvas × layer count, and under this it would bound content actually held.
That is the same kind of figure describing something an artist can act on. On the
reporting machine it is moot either way, because 10 GB of VRAM binds first —
which is the correct thing to be limited by.

### On a floor device the atlas holds *less*, and the figure belongs here

On a device reporting only `downlevel_defaults`' guaranteed 2048, the page is
`258 × 7 = 1806` and holds 49 tiles, so the whole atlas holds
256 × 49 × 65,536 × 4 = **3.29 GB** against the dense array's
256 × 2048² × 4 = **4.29 GB**. That is a 23% reduction in total document
capacity on exactly the device class `downlevel_defaults` exists to protect, and
the first draft worked the page arithmetic for that device and did not notice the
conclusion. It is a real finding and the figure should be carried.

Three things about it, and none of them is a dismissal:

- **The loss is entirely the pitch not dividing the limit.** 258 × 7 = 1806
  wastes 242 texels on each axis of a 2048 page, 22% by area, and 4.29 × 0.78 is
  the whole of the 3.29. At 4128 = 16 × 258 the waste is zero, which is why no
  real device sees this.
- **It cannot be tuned away without giving up §6.3's divisibility.** A pitch that
  divides a power-of-two limit must itself be a power of two, so the tile would be
  `pitch − 2·apron` — 254 at pitch 256 — which is not a multiple of
  `damage::TILE`'s 64. The two constraints are genuinely incompatible on a
  power-of-two limit, and if the floor device ever matters the right answer is to
  take 254 *there* and accept that a damage piece may straddle a tile row, which
  §6.2's banded fallback already handles. That is a device-dependent parameter,
  not a second code path.
- **The reduced ceiling is not reachable on that device class.** 4.29 GB is 256
  fully dense 2048² slices; `docs/mobile.md` describes a target that has never
  been built or run, and no phone has 4.29 GB of texture memory free for one
  document. A ceiling moving from unreachable to unreachable is still a ceiling
  moving downwards and is still worth writing down, which is why it is here.

### 3.2 The page table

A `texture_2d_array` of unsigned integers, sized `(tiles_x, tiles_y, MAX_SLOTS)`
— the canvas's tile grid, one slice per slot. One `u32` an entry: a sentinel for
"not backed", otherwise `page << 16 | tile_y << 8 | tile_x`. At the largest
canvas Umber will make, 32768 square, that is 128 × 128 × 256 × 4 =
**16.8 MB**; for the document in question, 1.6 MB.

**A texture array rather than a storage buffer**, and both would work. A storage
buffer is legal — `downlevel_defaults` guarantees four per stage and a 128 MiB
binding — and would be easier to update in scattered pieces. The array wins on
two counts that matter more here: it is indexed by **slot**, exactly as the
layer array is, so parking a deleted layer's slice, recycling a slot and
`slot_revisions` need no second scheme; and `textureLoad` on a `texture_2d_array`
is already the idiom `effect.wgsl` and `thumbnail.wgsl` use, including inside
non-uniform control flow, where a storage buffer read in a fragment shader is a
thing this codebase has never done.

**Whatever integer format is chosen, pin its guaranteed features in a test.**
`SEED_FORMAT` has `the_seed_format_is_a_render_target_on_every_device` for
exactly this reason, and the comment there is explicit that it was checked
rather than assumed. The table is only ever sampled with `textureLoad` and
written with `write_texture`, so it needs `TEXTURE_BINDING | COPY_DST` and not
`RENDER_ATTACHMENT`, which is a weaker demand than the seeds make — but weaker
is not none, and the same guard should exist.

### 3.3 The tile: 256, and Clip Studio's own blocks

Three sizes are worth considering, and the trade is internal fragmentation
against per-tile overhead:

| | 128 | 256 | 512 | 1024 |
|---|---|---|---|---|
| apron overhead at one texel | 3.1% | **1.6%** | 0.8% | 0.4% |
| tiles in one 20000×5000 layer | 6,240 | 1,580 | 400 | 100 |
| a signature in the corner costs | 66 KB | 262 KB | 1.0 MB | 4.2 MB |
| page-table bytes at 32768² × 256 slots | 67 MB | 16.8 MB | 4.2 MB | 1.0 MB |

256 is recommended, and the argument that settles it is not in that table.

**A `.clip` layer is already stored as 256-square blocks, and the file already
says which ones are absent.** `csblocks::BLOCK` is 256; `csblocks` reads one
zlib stream per block and hands back `Option<Vec<u8>>` per block, answering
`Some(None)` for an absent block *before* the `ZlibDecoder` is reached; and the
`InitColor` section states what an absent block holds. So for the documents that
prompted this, a residency *upper bound* at 256 falls straight out of the file
with no decode at all.

**An upper bound, not residency, and the first draft overstated this.** A present
block is not a non-empty block: Clip Studio stores a block it has touched, so a
stroke that entered a block and was later erased leaves it present and empty.
That cuts two ways and both matter. The survey in §10 is therefore
**conservative** — it will report occupancy no lower than the truth, so a good
answer from it is trustworthy and a bad one is not conclusive. And the *storage*
fed from presence alone would **over-back**, which is a real cost and is why
reclamation moves out of §9.4 and into stage 2.

256 is also `4 × damage::TILE`, which §6.3 turns into a `const` assertion.

### 3.4 Emptiness is per slot kind, and getting it wrong blanks a layer

A tile that is not backed has to read as *something*, and it is not the same
something for every slice:

- **A layer's** empty value is `vec4(0)` — transparent, premultiplied.
- **A mask's** empty value is **white**, because a mask multiplies the layer's
  alpha and a new mask reveals everything. `fill_layer_white` exists to say so.

Taking a mask's absent tile for zero hides the layer everywhere nobody painted,
which is precisely the bug `clipstudio.rs` records having to fix on the import
side: "a mask's `InitColor` states all-ones, because a Clip Studio mask begins
revealing everything — taking an absent block for zero blanks the layer
everywhere nobody painted". The same rule, arrived at from the other direction,
in the same format, at the same block size. That agreement is the strongest
single piece of evidence that this is the right shape.

The substitution is in the shader and costs nothing, because the layer read and
the mask read are already two separate lines with two separate uses: `lay` gets
`vec4(0)`, `m` gets `1.0`. It must be a `select` against the sentinel and not a
sample of a shared blank tile — a shared blank tile would itself need an apron
and would be a second thing to keep correct, and `no_selection_is_the_exact_
identity` is the precedent for wanting the identity to be exact rather than
merely accurate.

**Three operations become free.** `clear_layer` is "unback every tile of this
slot" — a table write, no GPU work, where today it clears 400 MB.
`fill_layer_white` on a new mask is the same table write, where today it clears
400 MB to white. `clear_all_layers` at start-up is a memset of the table. On the
large canvas those are seconds of the artist's time turning into microseconds,
and they come free with the residency model rather than being optimisations
anybody has to remember.

### 3.5 The apron, and why its width is a parameter

This is the part that decides whether the picture is pristine, and it has a
whole section of its own at §8. The storage consequence is that a tile is stored
as `(256 + 2A) × (256 + 2A)` texels: 256 of interior and an `A`-texel border
holding a copy of the *logical* neighbour's edge texels, or the neighbour's empty
value where the neighbour is not backed.

**`A = 1` is exactly sufficient for bilinear at LOD 0 and is not a margin.** A
bilinear tap reads the 2×2 neighbourhood of the sample position, so for a sample
anywhere in `[t, t+256)` it reaches at most texel `t-1` and texel `t+256`.
Bilinear does not widen under minification, because there are no mip levels
anywhere in Umber and `anisotropy_clamp` is at its default of 1. **Both of those
are settings nothing currently pins**, which §8.6 fixes: the apron's sufficiency
should be a `const` assertion or a test on the sampler descriptor rather than a
property that happens to hold.

**`A` is written as a named constant and not as a literal 1, because a
half-resolution proxy would raise it.** `composite-throughput.md` §4.5 prefers a
separate quarter-resolution proxy array over a mip chain on the layer array, and
that is the shape that composes with tiles — but if a proxy tile is generated
*tile-locally* from the atlas, producing one texel of proxy apron needs two texels
of source apron, and `k` reduction levels need `2^k`. At `A = 2` the overhead is
3.15% instead of 1.57%. Deciding the proxy decides `A`, and the constant is what
makes that a one-line change rather than a rederivation. §12 has the rest of that
reconciliation.

---

### 3.6 The upload path, which this document owns

**This is the interface the first draft assumed somebody else would provide, and
the one thing without which stage 2 saves nothing on an imported document.**
`install_import` calls `write_layer_rect` with `PixelRect { x: 0, y: 0, width:
size.x, height: size.y }` — the whole canvas — and `import-and-limits.md` §7.2's
streaming produces exactly that, one layer at a time. If `write_layer_rect` backs
a tile for every rectangle it is given, then opening a document backs every tile
of every layer and the atlas costs what the dense array costs.

Two entry points, and **both should exist**, because they are the fast path and
the floor rather than alternatives.

**The floor: `write_layer_rect` scans for emptiness.** Its signature does not
change.

```rust
pub fn write_layer_rect(&mut self, queue: &wgpu::Queue, slot: u32,
                        rect: PixelRect, bytes: &[u8])
```

Before uploading, it walks each tile of `rect` and skips one that is entirely the
**slot's own empty value** — zeroes for a layer, `0xff` in the red channel for a
mask, which is §3.4's rule and must not be written as "all zeroes". The cost is
one pass over a buffer the importer has already traversed twice (the blit and
`srgb::encode_buffer`, per `import-and-limits.md` §3), and it is `memchr`-shaped
work that stops at the first differing byte, so a busy tile pays almost nothing.

What this buys is the whole composition: **residency exists from the day the
atlas does, with no change to any reader, to `ImportedLayer`, or to
`install_import`.** It throws away the free residency a `.clip` already carries,
which is what the fast path is for — but it means stage 2 is not blocked on the
importer, and that is worth more than the scan costs.

**The fast path: a tile sink.** For the formats that already store tiles, the
dense buffer should never exist.

```rust
/// Begin replacing a layer's content. Every tile not named before `finish`
/// is left unbacked, i.e. reads as the slot's empty value.
pub fn begin_layer_upload(&mut self, slot: u32) -> LayerUpload<'_>;

impl LayerUpload<'_> {
    /// `bytes` is `TILE * TILE * 4`, tightly packed, in layer-texture form
    /// (sRGB, alpha premultiplied in linear light) — `ImportedLayer::pixels`'
    /// contract, at tile granularity.
    pub fn tile(&mut self, at: TileCoord, bytes: &[u8]) -> Result<(), AtlasFull>;
    /// This tile is entirely the slot's empty value. Backs nothing, uploads
    /// nothing, and exists so a caller can be exhaustive over the grid.
    pub fn empty(&mut self, at: TileCoord);
    pub fn finish(self);
}
```

Who feeds it:

- **`.clip`** — `csblocks`' per-block callback, one to one, because
  `csblocks::BLOCK` is 256 and so is the tile. An absent block is `empty`. No
  canvas-sized buffer at any point, which is `import-and-limits.md` §7.3 and
  `formats-and-host-memory.md` §8.3 agreeing.
- **`.kra`** — 64-square tiles, sixteen to a storage tile; the reader gathers a
  4×4 group or falls back to the dense path.
- **`.ora` and `.psd`** — a PNG must be decoded whole, so these decode one layer
  and shred it, which is `import-and-limits.md` §7.2's streaming with a tile sink
  instead of a dense upload. The shred is the emptiness scan above, reused.

**`AtlasFull` is a `Result` and not a panic**, because an import is exactly where
the atlas runs out and §9.5 is the refusal it feeds.

One thing the tile sink is *not*: a partial update. It replaces a layer's
content, so `write_layer_rect` remains the path for an undo patch, a cut's
write-back and a paste, which are rectangles inside an existing layer. Those
allocate the tiles they touch and never unback one, because a patch that writes
transparency is restoring transparency rather than declaring emptiness.

---

## 4. The composite

### 4.1 The loop

`composite.wgsl` today computes `uv = doc / doc_size` once and then, per layer,
does `textureSampleLevel(layer_tex, samp, uv, slot, 0.0)` and, if the layer has
a mask, a second sample at `mask_slot`. Everything else in the loop — the wet
stroke, the mask blend, the clip accumulator, the opacity, `composite_over` — is
arithmetic on values already in registers.

Tiled, the fragment computes **once, outside the loop**:

```wgsl
let tile   = vec2<i32>(floor(doc / TILE));       // which document tile
let inside = doc - vec2<f32>(tile) * TILE;       // 0 .. TILE, exactly
```

and then, per layer:

```wgsl
let entry = textureLoad(page_table, tile, slot, 0).r;
var lay = vec4<f32>(0.0);
if (entry != UNBACKED) {
    let origin = tile_origin(entry);             // atlas texels, interior corner
    lay = textureSampleLevel(atlas, samp, (origin + inside) / atlas_size,
                             page_of(entry), 0.0);
}
```

with the mask read taking the same shape and substituting `1.0`.

Two things fall out of that being the shape.

**The subtexel precision improves.** Today the sample coordinate is
`doc / doc_size` and the hardware multiplies it back up; at a canvas edge of
32768 an f32 has about nine fractional bits left, so a sample position is good
to roughly 1/512 of a texel. `inside` is a subtraction of two nearby values and
is exact, and it is bounded by 256, so the in-tile coordinate carries fifteen
fractional bits. Tiling makes large canvases *more* precise, not less. It is a
small thing and it is the opposite of what one would fear.

**Non-uniform array indexing is already what this loop does.** The `slot` passed
to `textureSampleLevel` today comes from a uniform array indexed by a loop
counter; a page index coming from a `textureLoad` is the same kind of value in
the same position. `textureSampleLevel` with an explicit LOD is what makes
sampling inside a loop and after a branch legal at all, and that does not change.

### 4.2 What it costs, and what it saves

**Cost.** One `textureLoad` per layer per fragment, from a table whose working
set for one screen is tiny — at zoom 1 a 4K viewport covers roughly 16 × 9 tiles,
so 144 entries a layer, 576 bytes — plus a handful of integer operations and one
multiply-add on the coordinate. It should sit in cache. **I have not measured
this and will not pretend to have**; §10 names the example that would.

**Saving, and it is likely to dominate.** A layer whose tile is not backed
contributes nothing and is skipped without sampling a single texel of the atlas.
Today a 54-layer document at 4K samples a 400 MB texture fifty-four times per
fragment — roughly 450 million texel fetches a frame — whether or not those
layers have anything under the pointer. The composite does the sampling in
proportion to occupancy.

**There is no median to put in that sentence and the first draft invented one.**
It said "if the median real layer covers a third of the page", which reads as a
figure and is not one; §10's `survey-residency` is what produces it, and until it
has run the size of this saving is unknown. What can be said without it: the
saving is linear in occupancy and the cost is not, so there is an occupancy at
which the two cross, and finding it is a measurement rather than an argument.
`measure-composite.rs` should report the crossing point rather than a verdict.

**A fully dense layer costs 5.2% more than today**, and that is the honest price
of tiling something that was not sparse: a 20000×5000 layer rounds up to
20224 × 5120 of tile interior (+3.5%) and each tile carries its apron (+1.6%).
A document where every layer covers every pixel gets slightly bigger. Nothing
can be done about that and nothing should be — it is the price of the case that
matters.

### 4.3 The one ordering that can be got wrong silently

A skipped layer must still update `clip_alpha`. The existing line is

```wgsl
clip_alpha = select(0.0, lay.a, visible && opacity > 0.0);
```

and with `lay = vec4(0)` it correctly yields zero, which is what a clipped layer
above a wholly transparent base must answer to. So the `continue` for an
unbacked tile has to come **after** the clip accumulator is written, not before.
It is right for every document with no clipping in it, which is most of them —
the same shape as `docs/group-compositing.md`'s warning that the group pop must
precede the visibility test. Write the skip as a `select` on `lay` rather than as
an early `continue` and the failure becomes unreachable rather than avoided.

**There is a louder bug in the same place and the first draft missed it.** An
early `continue` would also skip the wet-stroke block, which is the `i ==
v.active_index` branch that blends the in-progress stroke into the layer inside
the stack. The active layer is precisely the one whose tiles are most likely to
be unbacked — that is what painting on a blank layer *is* — so a `continue`
placed before the stroke block means the stroke does not preview at all on empty
canvas, and then appears at pointer-up when the commit backs the tiles. That is
the stroke-jump failure the whole composite/commit rule exists to prevent,
arriving through an optimisation. Two consequences: the skip must come after the
stroke block as well as after `clip_alpha`, and the active layer must never be
culled by §4.4.

### 4.4 Hoisting, and the hoist that is refused

The brief asks whether the indirection can be hoisted by compositing per screen
tile with a uniform tile set. **No, and the reason is geometric.** A screen tile
maps to one document tile per layer only when the screen tile is no larger than
a document tile *and* aligned to it, and zoom and pan give neither. At zoom 1 a
64-pixel screen tile straddles two document tiles on each axis in the general
case; at zoom below 1 it covers many. A per-screen-tile uniform could carry a
*set* of entries, and resolving which of the set applies is the same indirection
with an extra layer of bookkeeping.

Two hoists that are real:

- **Compute the tile coordinate once per fragment**, outside the loop, as §4.1
  does. It is the same for every layer. Free.
- **Cull whole layers on the CPU, per frame, against the visible rectangle.** The
  CPU owns the page table, so it knows which layers have no backed tile in view.
  Such a layer can be handed to the composite with `visible: false`, and that is
  *exactly* correct rather than approximately: an invisible entry contributes
  nothing and sets `clip_alpha` to zero, which is what a wholly transparent layer
  does. Two provisos — the active layer must never be culled, because the wet
  stroke blends into it, and this changes every frame, so it belongs in
  `Editor::layer_draws` beside the float's slot swap rather than in the renderer.

  **This is `composite-throughput.md` R1 and belongs to that document, not this
  one.** R1 is already designed there, with its own subtle rule, and is ranked
  first there on the grounds that it is exact and cheap. What tiling adds is a
  second, finer *reason* a draw contributes nothing — no backed tile in view,
  rather than hidden or zero-opacity — so the right shape is one culling rule in
  one place that consults residency when residency exists. Two documents
  proposing one change is how the two end up disagreeing about the edge case, and
  the edge case here is the clipped run.

A third, refused for now: a pass per screen tile with a per-tile draw list, which
would shorten the loop as well as skipping fetches. It multiplies uniform uploads
and render passes by the tile count and is the kind of change that wants a
measurement before it wants a design.

---

## 5. The stroke

### 5.1 The wet-layer invariants, one at a time

CLAUDE.md's stroke-pipeline invariants are absolute. Each is preserved, and the
reason in every case is the same: **this design changes where a texel is stored
and nothing about what its value is.**

- *`Brush::opacity` must never be folded into per-dab coverage.* The dab pass is
  untouched. `dab.wgsl` is not modified by a line.
- *The dab pass's `max` blend saturates coverage.* The scratch stays a
  canvas-sized `R8Unorm` texture in stage 1 and 2, so the dab pass writes exactly
  what it writes today. §5.5 covers tiling the scratch, which is later work and
  does not disturb this either.
- *`composite.wgsl` and `commit.wgsl` must implement identical blending maths.*
  `blend.wgsl` is untouched and is still concatenated in front of both.
  `commit.wgsl`'s `stroke_rgb`, `stroke_src`, `fs` and `fs_blend` are untouched.
  `composite.wgsl`'s stroke block is untouched, because the scratch is still read
  at `uv` in document space; only `lay` and `m` change how they are fetched, and
  the promise the two files make to each other is about `s`, the source, which
  neither touches.
- *Paint and erase need different blend state.* Unchanged: the same two
  pipelines, the same targets, now attached to a page instead of a slice.
- *The dab pass loads rather than clears.* Unchanged.
- *The scratch stays `R8Unorm`.* Unchanged.
- *`Brush::build_up` is a blend state, not a shader branch.* Unchanged.
- *There are four dab pipelines.* Unchanged.
- *A blended commit needs a copy of the layer.* Unchanged in principle; §5.4 has
  the one new trap.
- *`finish_stroke` must flush `StrokeBuilder::pending`.* Untouched, and it is
  what guarantees the damage mask is complete before §5.2 reads it.

### 5.2 Residency is decided at commit, and the input already exists

The brief asks how a stroke crossing a tile boundary allocates tiles on demand
mid-stroke. **It does not have to, and that is the whole reason this is
tractable.** During a stroke the layer is never written; the dab pass writes only
the scratch, and the layer is touched exactly once, at pointer-up, by
`commit_stroke`. So residency allocation happens at one place, on the CPU, with
the damaged region already known.

`StrokeBuilder` already accumulates `damage::TileMask`, and `pieces` already
turns it into the rectangles the commit is scissored to and the undo patch is
captured from. Mapping those to storage tiles is one function over integers, in
`umber-core`, testable without a device — the arrangement `CanvasCopy::plan`,
`Clip::place`, `ScrollSpan` and `band_rows` already keep.

The commit sequence becomes:

1. `pieces` → the set of storage tiles they touch.
2. **Read the undo patch first.** A piece over a tile that is not backed reads
   as the empty value, and no copy is issued at all — which is both correct and
   free, and is the whole of why undo on a blank layer becomes cheap.
3. Allocate the tiles that are not backed, and clear each to the slot's empty
   value. **This step can fail** — see §9.5, which is the case the first draft
   disposed of in a clause.
4. Commit, scissored per `piece ∩ tile`.
5. Mark the tiles written, and their backed neighbours, for the frame's apron
   refresh. Not a refresh here: §8.3 is why the refresh is one call on the frame
   path rather than a step inside every writer.

**Step 2 must precede step 3 and that is a trap worth naming.** Allocating first
and reading afterwards is also correct *provided* the clear in step 3 has been
submitted before the read — and interleaving a readback with an allocation on
the same encoder is exactly the class of ordering mistake `begin_float`'s "submits
twice" comment records. Reading first has no such subtlety and does less work.

### 5.3 The commit pass, and the scissor

A render pass may attach one page, so the commit becomes one pass per page
touched, with a scissored draw per `piece ∩ tile` inside it. A stroke usually
touches one or two pages, so this is not the pass-per-piece cost `commit_blended`
already reasons about.

`commit.wgsl`'s **vertex** shader gains the tile's document origin and the
tile's atlas origin, and maps `doc` into atlas clip space instead of document
clip space. Its **fragment** shaders are byte for byte what they are today. That
split is the design: the thing that must not drift from `composite.wgsl` is in
`fs`, and `fs` is not being edited.

The guarantee "no pixel outside the pieces the undo patch was captured from is
written" survives, and is strengthened: the scissor rectangle becomes a
sub-rectangle of what it was, so the set of written pixels is a subset of a
subset. What does *not* survive automatically is the apron, which is outside
every piece by construction and therefore cannot be written by the commit pass.
It is refreshed afterwards by a copy — see §8.

### 5.4 The blended commit

`commit_blended` copies the layer under each piece into a backdrop texture,
because a colour attachment may not also be sampled. Under tiling the copy source
is per tile, so a piece spanning two tiles becomes two copies into the one
backdrop texture at the right offsets. One new trap: a piece over a tile that is
**not backed** has nothing to copy from, so the backdrop must be cleared to the
empty value before the copies rather than being written entirely by them. Today
every texel of it is overwritten and no clear is needed, so this is a real
addition and it is silent if forgotten — the backdrop would hold whatever the
driver left, and Multiply against uninitialised memory is a stroke with garbage
in it.

A second trap in the same function, and it is the one that would survive review.
`fs_blend` indexes the backdrop with `vec2<i32>(floor(in.doc - u.rect_min))`, so
**`rect_min` must stay the *piece* origin and must not become the tile origin**
when the vertex shader learns about tiles. The two are the same number today and
diverge the moment a piece is split, at which point every blended commit on a
multi-tile stroke samples its backdrop at an offset. It is worth a named constant
or a differently named uniform field rather than a comment.

### 5.5 Tiling the scratch — later, and here is what it would take

The stroke scratch is 100 MB at this canvas and the colour scratch is 800 MB.
Tiling them would mean the dab pass emitting one instance per (dab, tile) pair
and allocating tiles as dabs arrive — genuine on-demand allocation, on the
hottest pass, mid-gesture. It is the one place in this design where that is
required, which is a reason to do it last rather than first. It is also where a
mistake would be most visible, because the `max` blend's saturation is a property
of the target and a dab split across two targets must saturate identically in
both, which it does, but only because `max` is idempotent — a build-up stroke,
whose blend is *not* idempotent, would need each fragment to land in exactly one
tile, which a scissor gives.

The cheaper independent fix for the colour scratch — give it back rather than
hold it for the session — is `slot-lifecycle-and-vram.md` §5 and §8.3's, not
this document's.

---

## 6. Undo

### 6.1 Nothing in the file changes, and `history::VERSION` does not move

`PixelPatch` names a slot and a document-space rectangle, and holds pieces that
are document-space rectangles of bytes. Every one of those is a statement about
the *document*, not about storage. So the in-memory patch, the saved history's
manifest, `SaveHistory`'s slot-to-position mapping and the `.ora` bytes are all
untouched. This design earns no version bump of any kind, in either format,
which by this codebase's own standard is the strongest available evidence that
it is a change to how something is held rather than to what is held.

Two behavioural notes that are internal:

- An undo writing a patch back into a region whose tiles are not backed must
  allocate them first, because the patch may hold paint. Same allocator, same
  place.
- An undo that writes transparency into a backed tile leaves it backed. That is
  waste, not damage, and reclamation is where it goes — which is now inside
  stage 2 rather than deferred, per §9.5.

### 6.2 What the readbacks become

`read_layer_pieces` splits each piece by storage tile, into
`ceil(width / 256) × ceil(height / 256)` sub-rectangles. The batching against
`readback_limit` is unchanged; there are simply more, smaller copies. Pieces over
unbacked tiles need no copy at all and are synthesised, which composes with the
existing "a piece whose pixels are all identical is held as that one pixel" rule
to make an undo patch on empty canvas cost nothing at either end.

**A piece is *not* always at most 64 pixels tall, and the first draft said it
was.** That is true of a piece derived from a stroke, and `TileMask::pieces`
opens with

```rust
if self.cells.is_empty() {
    return vec![rect];
}
```

for the reason its own doc comment gives: "a patch built from a file or by a test
has no mask at all and must still describe itself". Two live paths hit it.
CLAUDE.md states one explicitly — "**The cut's patch is the rectangle, not
cells.** There is no `TileMask` to have accumulated one from" — and a
revision-1 saved history is the other. `app.rs`'s undo path feeds
`patch.pieces()` straight into `read_layer_pieces`, so a full-canvas piece
reaches it today and under tiling spans every tile row. Not fatal: the existing
fall-through to the banded reader for a piece larger than `readback_limit` covers
it. But the claim has to be narrowed to stroke-derived pieces, and §6.3's
consequences narrow with it.

`read_layer_rect` is the same treatment without the batching.

### 6.3 The alignment that is real, and the one that is not

**Real, and worth a `const` assertion:** `storage tile % damage::TILE == 0`. With
256 and 64 that holds, and it buys two things. Every damage cell lies wholly
inside one storage tile, so a stroke-derived piece decomposes into whole
intra-tile rectangles with no partial-cell arithmetic. And "which storage tiles
does this stroke touch" is a shift of the cell coordinates the `TileMask` already
holds, rather than a second rasterisation of the stroke.

It does **not** buy "no readback ever spans tile rows", which the first draft
listed as the third thing. That would follow from every piece being at most 64
tall, and §6.2 is why that is false for a cut and for a loaded history. The
divisibility is about *cells*, and a rect-shaped piece has no cells.

**Not real, and worth refusing explicitly:** making the two numbers *equal*, at
64. It sounds like the elegant answer and it buys nothing. Adjacent logical tiles
are not adjacent in the atlas, so a piece spanning several tiles is several
copies whatever the tile size, and 64 makes it more of them. It costs 6.4% in
apron at one texel, and a 20000×5000 layer becomes 24,727 tiles, which is a
page-table of 25 MB per canvas and a lot of per-tile bookkeeping to save 3% of
internal fragmentation. The alignment worth having is divisibility, not equality.

---

## 7. Every other path that names a slice

The renderer's paths that reach `layers.texture`, `slot_views` or
`raw_slot_views` are the whole of the work in stage 1, and they are listed here
so nobody discovers the eighth one late.

| path | what changes |
|---|---|
| `composite` | §4. The one hot path. |
| `commit_stroke` / `commit_blended` | §5.3, §5.4. |
| `clear_layer`, `clear_all_layers` | unback the tiles. No GPU work at all. |
| `fill_layer_white` | unback the tiles. White *is* a mask's empty value. |
| `read_layer_rect`, `read_layer_pieces`, `read_batch` | §6.2. |
| `write_layer_rect` | §3.6. Scan each tile for the slot's empty value, skip it where it is empty, allocate and upload otherwise. The signature does not change. |
| `begin_layer_upload` | §3.6. **New**, and it is the deliverable the composition turns on. |
| `begin_capture` / `drive_capture` | a whole layer is `tiles_x × tiles_y` copies; unbacked tiles are synthesised into the host buffer with no copy. Banding changes from a row count to a tile-row count. Sparse layers get much cheaper. |
| `flip_layers` | two exact permutations: mirror the page table on the CPU, and mirror each backed tile's contents in place. Both are exact, so `a_flip_mirrors_the_canvas_and_flipping_twice_restores_it_exactly` still holds. Aprons are regenerated afterwards rather than mirrored, because regeneration is one code path and mirroring an apron is a second. |
| `resize` | the hard one. A translation by a non-multiple of the tile size re-cuts every tile. Recommended: one canvas-sized scratch, held once, layer by layer — a resize is a rare explicit operation and this keeps it obviously correct. It is also the natural moment to fix the known bug that `resize` carries the *old* canvas's slice count, which is written up in `resize`'s own docs and is worth 102 GB in the worst case. |
| `begin_float`, `render_float` | **not unchanged, and the first draft's table said they were.** See below — this was the eighth path. |
| `bake_effects` and `effect.wgsl` | the extract pass reads the layer array with `textureLoad`; it needs the page table, in the same shape as the composite. The working *set* stays dense in stage 1 — it is per document, not per layer — and the fix for its allocation belongs to `layer-effects.md`'s stage 3. The effect *result* slice's residency is not the layer's: see below. |
| `thumbnail.wgsl` | needs the page table for the same reason. It also gains something: the residency map is already a coarse content bounding box, so the bounds pass can be run over the backed tiles alone rather than the whole slice. It cannot *replace* the bounds pass — residency is tile-granular and `content_rect`'s argument for a maximum reduce over a fine grid still stands — but it can bound it. |
| `slot_revisions` / `touch_slot` | becomes the *marking* half of the apron rule and nothing more. §8.3 is why it cannot be the refresh. |
| `pick_colour`, `export_rgba`, `probe_canvas` | **untouched.** All three reuse the composite pass, which already resolves tiles. This is the existing "there is no second flattening path" decision paying out again. |
| `transform.wgsl`, `flip.wgsl` | bind single 2D views, so they need a view per *page* rather than per slot. **At most equal to today, not fewer** — `LayerStore::new` already builds `capacity` views and capacity is at most 256 — and note it builds *two* sets, `slot_views` and `raw_slot_views`, so an atlas needs both per page. |

### The float, which the table got wrong

`begin_float` is not "unchanged". It performs three whole-canvas
`copy_texture_to_texture` operations against `self.layers.texture` — the layer
slice into `base`, the layer slice into the floating copy on a lift, and `base`
into the preview slice — and `render_float` restores out of `base` into
`self.layers.texture` at a slot origin and then renders into
`self.layers.slot_views[slot]`. Under an atlas none of those is a single copy and
none of those views exists. **This was the eighth path the table was written to
prevent discovering late, and it was in the table saying it was fine.**

What it actually needs, in stage 1: every one of those copies becomes per tile,
and the preview target becomes a page attachment with a scissor. `base` and the
floating copy stay canvas-sized dense textures, because they are *not* layer
slices and nothing about them has to be tiled for the atlas to work — so the
1.2 GB figure stands for stage 1 and the work is the copies, not the storage.

And in stage 2 there is a requirement neither draft stated: **the preview slice's
residency is the layer's union the destination rectangle.** It is written by a
whole-canvas copy of `base` today, so under sparse residency it must back
whatever the layer backs *plus* wherever the picture has been dragged to, and the
destination moves every frame of a drag. On this canvas a float over a fully
backed layer is 1,580 tiles, 400 MB of atlas, the moment somebody presses T. The
copy-on-write `base` in §9.4 addresses `base`; it says nothing about the preview,
and the preview is the one that has to be written every frame.

### An effect's residency is not its layer's

An outline or a drop shadow reaches *outside* the pixels it derives from, so an
effect slice's backed set is the layer's content **dilated by the effect's
reach** — `effect_field`'s radius, in tiles, rounded outwards. Two consequences:
the allocator needs the dilation at the moment it backs an effect slice, and
§4.4's per-frame culling must not cull a layer whose own tiles are off screen but
whose shadow is not. Neither is hard; both are silent if missed, and the second
would show as a shadow that disappears when its layer scrolls out of view.

---

## 8. Quality: where a seam could come from, and why it does not

The overriding constraint is that stroke quality, layer fidelity and the
rasterised image stay pristine. Six hazards, each named and each answered — and
the third of them, the stale apron, is where the first draft's answer was wrong
rather than merely thin.

### 8.1 Bilinear across a tile boundary

**The hazard.** The composite samples the layer with a linear sampler at
arbitrary zoom. In an atlas the physical neighbour of a tile's edge texel is some
unrelated tile, so an unguarded bilinear tap at a boundary blends two layers'
pixels together — a one-texel bright or dark line on a grid every 256 pixels,
most visible under magnification, which is exactly when an artist is looking
closely.

**The answer, and it is now a close call rather than a preference.** An apron of
`A` texels, holding the *logical* neighbour's edge texels. Sufficiency at `A = 1`
is arithmetic, not a margin: a bilinear tap at position `p` reads texels
`floor(p - 0.5)` and `floor(p - 0.5) + 1`, so a sample anywhere in
`[t, t + 256)` reaches at most `t - 1` and `t + 256`. Bilinear taps a 2×2
neighbourhood at every scale, because Umber has no mip levels and no anisotropy —
which §8.6 turns from a fact into a pinned one.

**Refused alternative: clamp-to-edge within the tile.** It costs no memory and it
duplicates the edge texel, which is a visible ridge at every tile boundary of
every layer. It is the same mistake `dab.wgsl`'s `selection_mask` refuses ("a
1x1 texture sampled outside its own rectangle... clamping would smear the
boundary texels across the rest of the canvas") and `thumbnail.wgsl` refuses for
its own frame.

#### The alternative, re-ranked: reconstruct bilinear by hand

Four `textureLoad`s at integer texels, resolved through the page table, lerped in
the shader. The first draft called this "refused" and gave two objections. **One
of them was wrong and the other was stated against the wrong baseline**, so it is
re-ranked here to a near-peer, to be decided by measurement rather than by
argument.

- *"Eight fetches per layer per fragment instead of two"* — **wrong, and by a
  factor of four on the count that matters.** The four taps straddle a tile
  boundary only when the sample sits within half a texel of one, which is
  `1 - (255/256)²` ≈ **0.78%** of interior samples. The other 99.2% resolve
  through **one** page-table entry, so the common path is one table read and four
  loads, not four table reads and four loads. And four `textureLoad`s of an
  adjacent 2×2 hit the cache lines a bilinear tap would have fetched anyway, so
  the *bandwidth* is close to unchanged. What is genuinely worse is instruction
  count: four loads and three lerps over four channels, per layer, per fragment,
  up to 64 times — on the pass `composite-throughput.md` already identifies as
  the frame's dominant cost.
- *"A second implementation of bilinear filtering in the hot path"* — **weaker
  than it sounded.** The drift this codebase refuses is between two *texts it
  maintains*: `blend.wgsl` compiled twice, `render_float` called twice. The
  hardware sampler is not a text Umber maintains, so there is nothing for a hand
  lerp to drift *from*. Nor does it create a new divergence class: `commit.wgsl`
  already reads its backdrop with `textureLoad` at an integer texel while
  `composite.wgsl` samples the layer bilinearly, and that file's own comment
  already says so and explains why the promise is about `s` rather than about the
  result.
- **One thing that does hold, and it is worth stating because it is the question
  somebody will ask.** `textureLoad` on an `Rgba8UnormSrgb` view applies the
  transfer function, so a hand lerp of loaded values is a lerp of *linear* values
  — exactly what the hardware does on an sRGB texture. That is not an assumption:
  `flip.wgsl` goes out of its way to read through a **non-sRGB view**
  (`LAYER_FORMAT_LINEAR`, declared in the array's `view_formats`) precisely
  because a `textureLoad` through the sRGB view would decode. So the hand path is
  colour-correct by the same evidence that makes the flip exact.

**The ranking, corrected.** The apron is still preferred, on the mechanism in
§8.3 and not on the one the first draft proposed. The hand lerp is the fallback
and is now the thing to build if §8.3's mechanism cannot be made to hold — which
is §13.13's rule, unchanged, with a fairer view of what taking it costs. What
should decide between them is `measure-composite.rs` reporting both, not this
paragraph.

### 8.2 Where the neighbour is not backed

The apron then holds the neighbour's **empty value** — zero for a layer, white
for a mask — which is byte for byte what a dense slice held at that texel. So the
edge of a backed region fades into transparency exactly as it did before, and
there is no seam to prevent because there is no difference.

### 8.3 A stale apron, and the mechanism the first draft got wrong

**This is the real risk in the whole design.** The apron is a copy, so it can go
out of date, and a stale apron is a one-texel seam that appears only at
particular zooms on particular layers — the worst possible failure to reproduce
and to notice.

#### What the first draft proposed, and why it cannot work

It said to hang the refresh off `touch_slot`, on the strength of CLAUDE.md's
claim that `slot_revision` is bumped "inside every method that writes a slice".
Three things are wrong with that and all three are checkable:

1. **`touch_slot` has no device, no queue and no encoder.** Its signature is
   `fn touch_slot(&mut self, slot: u32)` and its body increments a counter and
   abandons a thumbnail. An apron refresh is GPU work. Rewriting it as
   `touch_tiles(slot, rect, encoder)` is not "the rule stated once at a narrower
   granularity"; it is a signature change at ten call sites, two of which —
   `touch_all_slots` from `resize`, and `clear_all_layers` — have no rect and no
   encoder to give it.
2. **`render_float` takes `&self`, writes a slice, and its two *callers* do the
   touching.** That is exactly the "enforced at N call sites" shape, and it is the
   very method CLAUDE.md records the "exhaustive by construction" claim being
   false for. The first draft quoted that correction in the paragraph above and
   then built on the claim anyway, which is the failure the correction is *about*
   happening a second time in the document that cites it.
3. **The ordering would be only accidentally right.** In `draw_float` the touch
   follows the render, so an appended apron copy would be correctly ordered by
   luck. A future writer that touches before it draws refreshes an apron from
   stale interiors — the stale-apron bug wearing a green test suite.

#### The mechanism that does work: mark with the writer, refresh with the frame

The repair is to split the rule, and the split is what makes it hold.

- **Marking needs no GPU**, so it can live where `touch_slot` lives. Widen it to
  `touch_tiles(&mut self, slot: u32, region: Option<PixelRect>)`, `None` meaning
  the whole slot, and have it insert into a dirty set alongside bumping the
  revision. `touch_slot` becomes `touch_tiles(slot, None)`, and the two call
  sites with no rect pass `None` honestly rather than being contorted. That
  answers (1) completely: no signature carries an encoder.
- **The refresh is one call on the frame path**, `refresh_aprons(&mut self,
  encoder)`, run once per frame before the composite, draining the dirty set.
  That answers (3) completely: there is no per-writer ordering left to get wrong,
  because every write in the frame has happened before the single refresh.
- **`render_float` should take `&mut self` and mark for itself.** It takes
  `&self` only because `draw_float` holds a borrow of `self.float` across the
  call, which is soluble by lifting the handles out first. That answers (2), and
  it incidentally closes the gap CLAUDE.md complains about rather than
  perpetuating it.

**The reason this is a better shape than a per-writer refresh is the failure
mode, not the tidiness.** Forgetting to mark in one writer is a seam on one
layer, at one zoom, sometimes — quiet, and the thing the whole section fears.
Forgetting the single frame-path call is *every* tile boundary of *every* layer
seaming immediately and universally, on the first frame, in front of whoever
made the change. A rule whose omission is loud is worth more than a rule whose
omission is subtle, and that is the whole argument for moving the GPU half out of
the writers.

#### What still has to be verified rather than argued

The marking is still N call sites and the enumeration is still somebody's job.
So: a **debug-mode verification pass** that recomputes every backed tile's apron
from its neighbours' interiors and asserts equality, run after every operation
the GPU suite performs. That is the guard, it is cheap, and it is what turns "we
enumerated the writers" from a claim into a test. If it cannot be made to pass —
or if the marking turns out to need a rect that some writer genuinely cannot
supply — §13.13 stands and the answer is the hand lerp in §8.1.

### 8.4 Rounding, and the exact-inverse promises

There are none to break. The atlas is the same `Rgba8UnormSrgb` format with the
same sampler and the same shader arithmetic. A texel moving from a dense slice to
a tile is a `copy_texture_to_texture`, which is a byte copy.
`saving_and_reopening_does_not_move_a_pixel` is about `docimport::srgb`'s
encode/decode pair and never touches storage.
`a_flip_mirrors_the_canvas_and_flipping_twice_restores_it_exactly` needs the flip
to stay an exact texel permutation, and §7 keeps it as two exact permutations.
`a_cut_takes_exactly_what_it_leaves_behind` is arithmetic in `umber-core` on
bytes that have been read back, and reads back the same bytes.

### 8.5 A half-texel shift over the whole picture

The one way to get §4.1 catastrophically and *uniformly* wrong is the atlas
coordinate: the interior texel at in-tile position `i` must land at atlas texel
`origin + i`, and `origin` must be the interior corner, past the apron. An
off-by-one-half here is a soft, uniform blur over every layer at every zoom,
which is subtle enough to ship.

The guard is a comparison, not an assertion about the formula:
`a_tiled_layer_composites_byte_for_byte_as_a_dense_one_did`, rendering the same
content through both paths and comparing. Per the testing rules it must use
content where coverage is only ever 0 or 1 — a hard-edged rectangle, the shape
`a_hard_edged_rectangular_lift_is_exact` already uses — so that the comparison
may promise bytes. A second case with an antialiased edge compares alphas within
one level. And the mutation that proves the guard: shift `origin` by one texel
and watch it fail.

### 8.6 The apron's sufficiency is incidental today, and should be pinned

The whole of §3.5's arithmetic rests on the sampler taking a 2×2 neighbourhood
and no more. That is true because `Shared::new` builds the layer sampler with
`mipmap_filter: Nearest`, no mip levels on the texture, and `anisotropy_clamp`
left at its default of 1. **Nothing pins any of the three.** An anisotropic
sampler takes up to sixteen taps along the minor axis; a mip level widens the
footprint by `2^k`. Either would turn a correct apron into a seam, silently, in a
change that had nothing to do with tiling.

So the cheap guard, and it is worth having whichever way §8.1 is decided: a test
on the sampler descriptor — `anisotropy_clamp == 1`, `mip_level_count == 1` on
the atlas — stated as "the apron is `A` texels because the filter reaches `A`
texels". That makes the dependency visible at the place a future change would
break it, rather than in a design document. It is also the cheapest form of the
structural guarantee §8.3 is reaching for, and it costs nothing.

---

## 9. Staging, and what each stage costs

Sizes are estimates of the code a stage touches. They are guesses in the spirit
of `docs/document-import.md`'s "roughly 200–300 lines" for the `.psd` mask fork,
and should be read as orders of magnitude.

### 9.1 Before any of it: cheaper wins, and none of them is this document's

The first draft listed these as work to do "before any of it", which is right,
and priced them and named their line counts, which reads as ownership. Listing
somebody else's recommendation as your own prerequisite is how four half-fixes
get built. **Every item below belongs to a sibling document, is better argued
there, and is named here only so the sequencing is visible.**

1. **`resize` carries the old canvas's slice count** — `slot-lifecycle-and-vram.md`
   §5.4. Already written up in `resize`'s own doc comment too. Worst case 102 GB
   at 10000². A signature change through one call site.
2. **The growth transient**, `slot-lifecycle-and-vram.md` §4.2: exact growth means
   `2c + 1` slices live, so a hand-built document stalls at eleven layers on a
   10 GB card and stalls into the crash box. **This is the half of the reported
   symptom neither this document nor `import-and-limits.md` had**, and it fires
   every time the artist adds a layer, where the import's staging doubling fires
   once. It should be read before either.
3. **The effect working set is allocated whatever the canvas** — up to 13 bytes a
   pixel, 1.3 GB here, as soon as one effect exists, with `EFFECT_LIVE_PIXELS`
   gating the bake's frequency and nothing gating the allocation. The fix is
   `docs/layer-effects.md`'s stage 3.
4. **The colour scratch is 800 MB held for the session** after one smudging
   stroke — `slot-lifecycle-and-vram.md` §5 and §8.3.
5. **A hidden layer needs no slice at all** — `layer-residency.md` §0(1), proven
   from `composite.wgsl` rather than argued, and worth 12 GB on a document with
   thirty roughs in it. Cheaper than everything in this document and it should
   ship first.

**And one correction to what the first draft said about the importer.** It listed
"trim the importer" as item 4, called it "the single highest-value item on the
list", and priced it at "the host peak from 21.6 GB to the size of the largest
layer". **That figure belongs to streaming, which is a different change and is
`import-and-limits.md` §7.2's.** The three are distinct and were being run
together:

| | what it produces | host peak | tile residency |
|---|---|---|---|
| **stream** (`import-and-limits.md` §7.2) | one dense canvas buffer at a time | **one layer** | none |
| **trim** (this document's old §9.1(4)) | `pixels` as a rectangle + origin, all layers still held | unchanged in the worst case | a bounding box |
| **tile sink** (§3.6, `import-and-limits.md` §7.3) | one block into one tile | one tile | exact |

Trimming changes what *one* layer costs and not how many are alive:
`ImportedDocument::layers` is built complete by every reader and `install_import`
iterates a borrow, so all of them stay resident until the loop ends. And trimming
alone may be worth very little on the document in question —
`docs/document-import.md`'s own measurement over 5,438 `.clip` bitmaps records a
worst **area of 15.93× the canvas**, so a trimmed layer clipped to the canvas is
routinely exactly canvas-sized, and one full-page paper or background layer keeps
the peak where it was. The first draft also left `ImportedLayer::mask` out of the
arithmetic entirely, which is another canvas × 4 per masked layer.

So: **stream** is the host-peak fix and is `import-and-limits.md`'s; **the tile
sink** is the residency fix and is §3.6's, here; **trim** is neither, and is only
worth doing as a step towards the tile sink for the readers that need shredding.

### 9.2 Stage 1 — the atlas, with every layer fully resident

The atlas, the page table, and every path in §7 ported. Residency is "every tile
of every layer", so **nothing is saved and nothing is observable**. That is the
point: the stage is guarded by an identity test against today's output, in the
shape `no_selection_is_the_exact_identity` and `grain_off_is_the_exact_identity`
already take, and it is where every seam, every readback and every ordering trap
gets found while there is a known-correct answer to compare against.

New logic: the residency model in `umber-core` (tile arithmetic, the piece
decomposition, the allocator's plan), which is testable without a device,
~250–350 lines. The atlas and page table in `umber-render`, ~400–600. Ported
paths in `canvas.rs`, ~800–1,200 lines changed. `composite.wgsl` ~30 lines,
`commit.wgsl` ~15, `effect.wgsl` and `thumbnail.wgsl` ~15 each. Call it
**1,500–2,500 lines touched, of which 700–1,000 is genuinely new.**

It is a large stage and it does not obviously want splitting further: the atlas
either is the storage or it is not, and a half-ported renderer has two storage
schemes to keep in step, which is the thing this codebase refuses everywhere.

### 9.3 Stage 2 — sparse residency

Tiles are backed only where there is content: **on load through §3.6**, at commit
from the damage mask, at float commit and at undo. This is where the memory comes
back, and it is small once stage 1 exists — the machinery is already there and
what changes is the initial state and the allocator being allowed to say no.
~300–500 lines, plus §3.6's emptiness scan and §9.5's refusal, plus reclamation,
which has moved into this stage.

**"On load" is load-bearing and had nothing behind it.** The first draft wrote
"from what the importer found", and the importer finds nothing of the kind: its
own design streams dense canvas buffers, and `install_import` hands
`write_layer_rect` the whole canvas. Without §3.6 this stage backs every tile of
every layer on import and saves nothing on the documents it exists for.

**And the claim that stages 1 and 2 are "the smallest increment that saves a
byte" is true for painting and false for opening.** A document painted from blank
in Umber gets its residency from commits, so stages 1 and 2 alone do save on it,
and `clear_layer`, `fill_layer_white` and `clear_all_layers` become free. An
*imported* document needs §3.6 as well. Since §3.6's floor — the emptiness scan
inside `write_layer_rect` — needs no importer change at all, the honest statement
is that stage 2 must include it, and it does.

### 9.4 Later

- **The float's base as a copy-on-write page table**, and the preview slice's
  residency rule, which §7 says is the layer's union the destination and which
  nothing yet addresses. Nearly all of the 1.2 GB a transform costs here.
- **Tiling the scratch and the colour scratch**, §5.5. 900 MB.
- **Paging to host memory.** The natural next feature and a different one: it
  turns residency from "has content" into "has content and is wanted", needs an
  eviction policy and a fault path, and makes editing latency depend on a cache.
  The design above leaves room for it — the page table already has a not-backed
  state and needs only a third, "backed elsewhere" — and it should not be
  attempted until §10's measurements exist. It is also the only thing on this
  list that helps a document whose *resident* content exceeds VRAM.
  `layer-residency.md` is the design; its §0(4) "full-resolution slices become a
  fixed-depth cache, with the slot as a name and a residency table mapping name
  to line" is **the page table one level coarser**, so the two should be built as
  one mechanism at tile granularity rather than two at different ones.

### 9.5 What happens when the atlas is full, which happens at pointer-up

The first draft disposed of this in a clause — "the allocator being allowed to
say no" — and that is not enough, because of *when* it happens. A commit that
cannot allocate a tile arrives after the undo patch has been read, with the
stroke on screen and the artist's hand already off the pen. Dropping the paint
silently is unacceptable; so is a crash box. And "not reclaiming is never worse
than today" is true of *memory* and false here: today a stroke on a canvas-sized
slice cannot fail to find storage, so this is a new failure mode that tiling
introduces and must therefore answer for.

Four things, in the order they should be tried:

1. **Refuse before the pen goes down, not after.** `LayerStack::refusal_at` is
   already the shape for this: one gate, one reason, failing closed. A stroke
   cannot be given a bound in advance — its damage is unbounded — but the atlas
   can be asked whether it is within a threshold of full, and a document that is
   should refuse a *new stroke* with a sentence rather than fail in the middle of
   committing one. That converts an unrecoverable mid-gesture failure into a
   refusal at the moment somebody can act on it.
2. **Reclaim, then retry.** This is why reclamation moves from "later, not
   urgent" into stage 2: a tiny reduce per candidate tile answering "is this
   entirely the slot's empty value", read back, tiles freed. It is also what
   recovers the over-backing §3.3 admits comes from trusting block presence.
3. **Grow the atlas**, which is §3.1's transient and can itself fail fatally.
   So it must be attempted through a catchable path — `push_error_scope`, which
   `slot-lifecycle-and-vram.md` §6 and `import-and-limits.md` §8.2 are both
   circling, and which this document should not independently design.
4. **If all three fail, keep the stroke and say so.** The scratch still holds the
   coverage and the composite still previews it, so the least-bad state is a
   stroke that is visibly not committed, with a notice naming the limit and what
   frees it — delete a layer, undo, close a tab. Nothing is lost silently, which
   is the standard; the artist is stuck, which is honest.

None of that is satisfying, and it should not be made to sound satisfying. It is
the price of storage that can run out, and it is the strongest argument in this
document for `layer-residency.md`'s paging being the eventual answer rather than a
sequel: a paging store cannot run out, it can only get slower.

---

## 10. What must be measured, and by what

Every figure in §4.2 that is not arithmetic is a guess and is labelled as one.
This codebase records repeatedly that guessed figures cause bugs — the text
budget's first table was 1.6× wrong because the machine was building; the
clipboard's first figures were three times too slow for the same reason; the
effect pass budget reasoned about a domain sixteen times smaller than the real
one. So:

- **`umber-core/examples/survey-residency.rs`** — the one that decides whether
  any of this is worth doing. Over a folder of real documents, report per layer
  how many 128/256/512/1024 tiles hold anything, against the dense cost. **It
  must not `import`**: `survey-documents`' own header records that reading one
  file in that folder costs 12.3 GB of host memory.

  **It must report two numbers per layer, not one, and the first draft asked for
  the wrong one.** *Present* blocks are readable from a `.clip` with no
  decompression at all — `csblocks` answers `Some(None)` for an absent block
  before the `ZlibDecoder` is reached — and presence is an **upper bound** on
  residency, because a block that was touched and later erased is stored and
  empty. *Non-empty* blocks need the inflate, one block at a time, which is
  bounded and slower. Both are wanted: the cheap figure says how much the storage
  would over-back if it trusted presence, and the expensive one says what the
  design is actually worth.

  **The reason this matters is that the two answers point at different
  documents.** At 15% occupancy the atlas is obviously right. At 60% — which is
  plausible if presence over-reports — the artist's 21.6 GB becomes 13 GB, which
  still does not fit a 10 GB card, and the right answer is `layer-residency.md`'s
  eviction rather than this. A survey that reports presence alone will answer the
  wrong question at exactly the occupancy where the answer matters.
- **`umber-render/examples/measure-composite.rs`** — frame time of the composite
  pass, dense against tiled, sweeping layer count (1, 8, 32, 64), canvas size and
  zoom (0.1, 1, 8), on the real adapter and under `UMBER_TEST_SOFTWARE=1`. What
  it has to answer is whether the skip pays for the indirection, and at what
  coverage the two cross.
- **`umber-render/examples/measure-atlas.rs`** — allocation, clear and
  apron-refresh cost per commit, over a stroke that stays inside one tile, one
  that crosses many, and a wash that covers the canvas. The wash is the case
  where the apron refresh is worst and it is the one to publish. It should also
  sweep the page side (§3.1 admits 16 tiles is a guess) and the growth policy
  against `grown_capacity`'s budget rule (§3.1's correction).
- **`slot-lifecycle-and-vram.md` §11's `measure-vram.rs` should be written
  first**, and this document should not duplicate it. Its second item — walking
  `ensure_slots` one slice at a time at a large canvas and reporting the
  allocator's peak — is the load-bearing measurement for §3.1's growth
  correction as well as for that document's own `2c + 1`. Its first item answers
  a question this document assumes throughout: **whether a 400 MB texture
  actually costs 400 MB**, which alignment and metadata surfaces may make false,
  and which every byte figure here rests on.

None of these may assert wall-clock time in a test on CI, for the reason
`a_capture_of_a_large_document_never_costs_a_frame` states it and v0.0.2 was
tagged broken for ignoring it.

---

## 11. What this does not fix

Stated plainly, because a design that quietly implies otherwise is worse than one
that refuses.

- **It does not by itself open the 21.6 GB document.** `MAX_TOTAL_BYTES` counts
  canvas × layers and would refuse it unchanged; and even with the refusal lifted,
  `docimport` builds 21.6 GB of host buffers before the GPU sees anything.
  **`import-and-limits.md` §7.2's streaming is the fix for the second** — not
  this document's trimming, which the first draft claimed and §9.1 corrects. The
  first then needs a different bound — a bound on *resident* bytes, which is only
  knowable during the decode, so the refusal has to move from a pre-flight check
  to a budget the reader consults as it goes, and the sentence has to change with
  it. That is its own piece of design and it belongs in `docs/document-import.md`.
- **It does not retire the growth transient**, and a sibling says it does.
  `slot-lifecycle-and-vram.md` §10 lists that document's §4 as "retired" by
  tiling, "on the grounds that a tiled layer grows by adding tiles, and there is
  no monolithic array to reallocate". **There is: the atlas.** §3.1 has the
  arithmetic. The transient is scaled down by the occupancy ratio and by the
  page-to-slice ratio, and it does not go away. Two siblings disagreed about this
  and neither knew; this is that resolved, in the direction of the sibling being
  right about the mechanism and wrong about the conclusion.
- **It does not open that document on the Dx12 backend at all**, because 20000
  exceeds `max_texture_dimension_2d`'s 16384 there. That is a canvas limit, not a
  storage one, and nothing here touches it.
- **It does not help a document whose resident content exceeds VRAM.** Only host
  paging does, §9.4.
- **The autosave still assembles the whole document in host memory**,
  canvas-sized per layer. It has the same shape as the importer and wants the
  same repair — and ORA layers carry `x`/`y` offsets, so a trimmed layer is
  baseline ORA and writes a *smaller* file rather than a different one.
- **It makes a genuinely full document about 5% larger.** §4.2.

---

## 12. Collisions

### 12.1 With the four siblings in this folder

The first draft reviewed five documents outside `docs/perf/` and **none of the
four sitting beside it**, three of which collide materially. That omission is
worth naming rather than quietly fixing, because it is the same failure as §0's
composition gap: a document that only looks outwards does not notice the design
being written next to it.

- **`composite-throughput.md` R7 proposes mip levels on the layer array** and
  calls them "the only thing that fixes zooming out", at ~9× the bandwidth of the
  fit-to-view composite. §13.11 here refuses mips. **The conflict is real but it
  is narrower than it looks, and that sibling has already chosen the shape that
  composes.** What a one-texel apron forecloses is a **mip chain on the atlas
  itself**: level `k` needs `2^k` texels of apron, and a mip of a *tile* is not a
  mip of the *layer* at the tile's boundaries — which is §13.11's own reason and
  is also, independently, that document's §4.4. What it does **not** foreclose is
  a **separate fixed-reduction proxy**, which is what that document's §4.5 says
  it prefers on memory grounds anyway (+8% against a full chain's +33%). A proxy
  is its own texture with its own tile grid and its own one-texel apron *at its
  own resolution*, so it composes with tiling without changing anything here —
  except that generating a proxy tile *tile-locally* needs two source texels per
  proxy texel, which is why §3.5 makes the apron width a named constant rather
  than a literal 1. **So the reconciliation is: no chain on the atlas, one proxy
  array beside it, `A = 2`.** That also happens to be where `layer-residency.md`
  §0(3) lands from the memory side, which is the third document to arrive at one
  proxy — and `composite-throughput.md` §4.5 already says the two must be
  reconciled towards one and not two.
- **`composite-throughput.md` R1 is §4.4's layer culling.** Already designed
  there, ranked first there, and this document should not propose it a second
  time. §4.4 now says so and hands it over; what tiling contributes is one more
  reason a draw contributes nothing, not a second rule.
- **`slot-lifecycle-and-vram.md` §4/§5** own the growth transient, the shrink,
  the colour-scratch release and the `resize` slice count. §9.1 now credits them
  rather than listing them as this document's prerequisites. And §11 records
  where that document's §10 is wrong about this one.
- **`formats-and-host-memory.md` §5.3** recommends packing three masks into one
  slice and **explicitly hands the work to this design**: "if the layer store
  becomes tiled, 'how wide is a mask tile' is one question asked once in a tile
  allocator, instead of six paths reworked". That is accepted, and it is a real
  requirement on the allocator: a tile carries a **class** — full RGBA for a
  layer, one channel for a mask — and the empty value is per class, which §3.4
  already needs. The one thing it adds is that a mask tile's emptiness test is on
  its own channel, and a slice is unbacked only when all three channels are
  empty.
- **`layer-residency.md`** is the closest thing to an alternative rather than a
  collaborator, and the honest statement is that **which of the two does the
  heavy lifting is decided by §10's occupancy number, not by argument**. Its
  §0(1) — a hidden layer needs no slice — is cheaper than anything here and
  should ship first regardless. Its §0(4) is the page table one level coarser.
  Its §2.6 already says that "scrolled off screen at high zoom is tiling's, not
  residency's", which is the division to keep.

### 12.2 With the standing designs outside this folder

- **`docs/group-compositing.md` rewrites the same loop.** It adds an accumulator
  stack and per-entry open/close markers to `composite.wgsl`, using the two spare
  uniform fields (`layers[i].z` as a `-1` sentinel and `extra[i].w`). This design
  adds a page-table lookup to the body of that loop. They do not conflict
  logically — one is about *which accumulator*, the other about *where the texel
  is* — but they are edits to the same fifty lines and should not be in flight at
  once. Note one interaction: group compositing writes `-1` into `layers[i].z` for
  a close entry, which is the slot field; the residency lookup is keyed on slot,
  so the close branch must `continue` before any page-table read. That is the
  same ordering hazard as §4.3.
- **`docs/roadmap-review.md` §1.1** already records that three designs rewrite
  `Editor::layer_draws` and one changes its type. §4.4's per-frame culling would
  be a fourth. It is optional and should be sequenced last of the four.
- **`docs/layer-effects.md` §6.1/§6.3** derives the effect budget from
  `MAX_SLOTS`. Under this design an effect slice is still a slot with a page
  table of its own, so the derivation stands unchanged — but its *meaning*
  softens, because a slot no longer implies a canvas of memory. The arithmetic
  should not be loosened on that basis without deciding what the new bound is;
  `roadmap-review.md` §1.3 already records `MAX_SLOTS` being the number two
  designs disagreed about.
- **`docs/structural-undo.md`** is unaffected: patches are document-space and
  §6.1 changes no format.
- **`docs/mobile.md`** benefits and is worth re-reading afterwards. A tile-based
  renderer loads and stores the whole attachment per pass, so the pass-per-page
  commit is cheaper there than the pass-per-piece one `commit_blended` already
  flags as the thing to revisit on such a device.

---

## 13. What I would refuse

Collected, because the refusals are the useful half.

1. **A texture-array slice per tile.** `MAX_SLOTS` is 256 and 1580 tiles is one
   layer. §2.1.
2. **A texture per layer, or a pass per layer.** Breaks the one-pass composite,
   which CLAUDE.md names explicitly, and needs bindless that
   `Features::empty()` does not have. §2.2.
3. **Sizing the array to the largest layer's bounds.** Saves nothing on the
   documents this exists for, which have one full-page layer. §2.2.
4. **Block compression.** Lossy, against the stated constraint, and not a render
   target. §2.4.
5. **A smaller pixel format.** `LAYER_FORMAT`'s comment already argues that eight
   bits is the floor and that even eight bits of *linear* is too few.
6. **Clamp-to-edge at tile boundaries instead of an apron.** A visible ridge on a
   grid, and the same mistake `selection_mask` and `thumbnail.wgsl` each refuse.
   §8.1.
7. **Manual bilinear reconstruction in the composite loop — downgraded from
   "refused" to "the fallback, and a near-peer".** The first draft's two
   objections do not survive checking: it is one page-table read and four loads
   for 99.2% of samples rather than eight fetches, and there is no maintained
   text for a hand lerp to drift from. What is left is instruction count on the
   hottest loop, which is a measurement rather than a refusal. §8.1.
8. **Making `damage::TILE` equal the storage tile.** Buys no copy merging,
   because adjacent logical tiles are not adjacent in the atlas, and costs 6.4%
   in apron plus 25,000 tiles a layer. Divisibility is the alignment worth
   having. §6.3.
9. **Compositing per screen tile with a uniform tile set.** Zoom and pan mean a
   screen tile straddles document tiles; the uniform would carry a set and
   resolving within it is the same indirection. §4.4.
10. **Raising `MAX_TOTAL_BYTES` without `import-and-limits.md` §7.2's
    streaming.** It would trade a sentence the artist can act on for an
    out-of-memory kill during the decode. (The first draft cited its own §9.1(4)
    here, which is the wrong change — see §9.1.)
11. **A mip chain on the atlas**, as a way to make the apron unnecessary or
    minification cheaper. Level `k` needs `2^k` texels of apron and a mip of a
    tile is not a mip of the layer at its boundaries. **This is narrower than the
    first draft's "mip levels", deliberately**: it does *not* refuse
    `composite-throughput.md` R7, whose preferred shape is a separate
    fixed-reduction proxy array, and that composes. §12.1.
12. **Host paging now.** It is the right next feature and it needs §10's numbers
    first. §9.4.
13. **An apron maintained by discipline.** Unchanged as a rule and **the
    mechanism under it has been replaced**: the first draft said "if it cannot be
    hung off the `touch_slot` hook", and that hook cannot carry it — no encoder,
    and `render_float` writes a slice from `&self` with its callers doing the
    touching. §8.3 has the split that does work (mark with the writer, refresh
    once on the frame path) and the `&mut self` refactor it needs. If *that*
    cannot be made to pass a debug-mode verification pass, take refusal 7. A
    one-texel seam that appears at some zooms on some layers is the worst bug
    this design can produce, and "we will remember to refresh it" is how it
    ships.
14. **Growing the atlas one page at a time.** It is `grown_capacity`'s exact
    growth at six times the frequency — ~3 TB copied and ~300 multi-gigabyte
    allocations for the artist's document — and the first draft presented it as a
    benefit. The atlas answers to the same byte budget the layer array does.
    §3.1.
15. **Trusting block presence as residency.** It is an upper bound: a `.clip`
    stores a block it has touched, so an erased block is present and empty. Fine
    as a conservative *survey* figure, wrong as a *storage* decision, and it is
    why reclamation is in stage 2 rather than deferred. §3.3, §9.5.
16. **Letting a commit fail silently when the atlas is full.** The new failure
    mode tiling introduces, arriving after the undo patch has been read with the
    artist's hand off the pen. §9.5 refuses before the pen goes down instead.
