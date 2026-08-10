# Opening a document: the import path and the limits that govern it

A painter brought real Clip Studio documents to Umber. One of them is refused:

> This document has 54 layers at 20000×5000, which comes to 21.6 GB of pixels.
> Umber holds at most 17.2 GB of a document at once. Flattening or removing some
> layers will bring it within reach.

The others open, and while they open the VRAM on a 10 GB RTX 3080 fills.

This document is the *admission* half of that: what happens between a
double-click and a canvas, what it allocates, what it costs, and whether the
bounds that decide it are the right bounds stated in the right terms.

**Revised after `critique-tiling-import.md`.** §14 records what was accepted,
what was rejected and why. Three of that review's findings changed a
recommendation here: the trial `create_texture` is withdrawn (finding 15), the
refusal's "this GPU has 10.0 GB" is withdrawn (finding 14), and the upload
banding is now the fix rather than a bare submit (finding 17). One deliverable
that neither this document nor `tiled-layer-storage.md` owned is **claimed here**
— §7.2, the piece contract — and it turns out to be much smaller than the review
supposed.

---

## 0. Recommendation

**The bound that refuses the artist's document is not the bound that is hurting
them, and there are now three separate mechanisms hurting them rather than one.**
`MAX_TOTAL_BYTES` is a *host* sanity bound against a malformed header, 16 GiB,
and it is correctly documented as that. Nothing anywhere asks whether the
machine's GPU can hold the document.

1. **Band `write_layer_rect`, and submit per band.** The upload path is the
   mirror image of a rule this codebase already treats as load-bearing — every
   *readback* bands, because `max_buffer_size` is 256 MB — and the upload does
   not. One 400 MB `write_texture` per layer, with no submit in the loop, is a
   full-document staging peak on top of the array. Banding fixes the unbanded
   write and the staging accumulation with the same change. §8.1.
2. **Fix the `ensure_slots` growth transient, or bound it.** Growing the layer
   array builds the new store while the old is live, and at 400 MB a slice the
   growth quantum is one — so adding a thirteenth layer to a twelve-layer
   20000×5000 document asks for 10.0 GB on a 10 GB card. This fires **every time
   the artist adds a layer**, where (1) fires once at open. `slot-lifecycle-and-vram.md`
   §4 owns it; it is named here because it is half of the reported symptom and
   this document previously attributed all of it to (1). §8.4.
3. **Adopt `slot-lifecycle-and-vram.md` §6.3's `try_reserve`** — a real
   allocation inside `push_error_scope(ErrorFilter::OutOfMemory)`, with
   `memory_budget_thresholds` set — and call it from `install_import` beside the
   `max_texture_dimension_2d` check that is already there. This document
   previously proposed a bare trial `create_texture`; that is **withdrawn**, for
   two good reasons the review gave. §8.2.
4. **Run the device gate before the worker, not after it.** Today a document
   refused for the card's dimension limit is refused *after* the full decode, so
   the artist waits the whole time for a "no". The canvas size is in the header
   and every reader reads it before decoding a pixel. §9.
5. **Rewrite the refusal.** It names the right bound and gives the right figure,
   which is what CLAUDE.md's rule demands, and then gives advice that is wrong
   twice: it omits the canvas — the other multiplicand in the very figure it
   prints — and, followed exactly, it produces a 42-layer document that Umber
   admits and this machine cannot draw. §5.
6. **Adopt the piece contract (§7.2), which this document claims.** One line of
   type change carries exact residency from every reader to the store, and the
   upload API it needs **already exists**. It subsumes trimming and streaming
   rather than sitting on top of them.
7. **Do not raise `MAX_TOTAL_BYTES`.** CLAUDE.md's argument for stopping
   somewhere is sound and unchanged by anything here. What changes is that with
   (3) in place the figure stops being the one the artist meets. §4.3.

Progress and blocking, which the brief asked about, is **already built** and is
good: `loading.rs` decodes on a worker, reports per layer, and draws an empty
track rather than a guessed one. §9.

---

## 0.1 Who owns what

`critique-tiling-import.md`'s most-wanted change was that the residency-carrying
upload path be named as an owned deliverable. Here is the whole split across the
folder, including the two rows this document is retracting and the one it is
claiming.

| change | what it buys | owner |
|---|---|---|
| Trim `ImportedLayer::pixels` to a rectangle plus an origin | one layer costs its content; **host peak unchanged** | `tiled-layer-storage.md` §9.1(4), `formats-and-host-memory.md` §8.3 |
| Stream layers, one alive at a time | host peak = one layer; **no residency** | `formats-and-host-memory.md` §8.2, which claims it explicitly |
| **Many pieces per layer, `write_layer_rect` per piece** | **host peak = one piece, and exact residency** | **this document, §7.2** |
| **Band each `write_layer_rect`, submit per band** | staging peak = one band | **this document, §8.1** |
| The atlas backs only the tiles a written rectangle touches | VRAM = content | `tiled-layer-storage.md` §7's `write_layer_rect` row |
| `try_reserve`, `memory_budget_thresholds`, the VRAM refusal's wording | a sentence instead of a crash box | `slot-lifecycle-and-vram.md` §6.2, §6.3, §7.2 |
| The `ensure_slots` growth transient | the other half of the reported symptom | `slot-lifecycle-and-vram.md` §4 |

**This document previously proposed streaming (old §7.2) and did not know
`formats-and-host-memory.md` §8.2 had claimed it. That is ceded**, and §7 is
rewritten around the piece contract instead — which is the part neither sibling
had.

`docs/perf/allocation-accounting.md`, which the first draft deferred to twice,
**does not exist**. Every such deferral now names `slot-lifecycle-and-vram.md`,
which is the document that was meant and which had already written both
deferrals down.

---

## 1. The open path, end to end

### 1.1 `.clip`

`app.rs::open_path` → `begin_open` → `loading::Loading::start`, which spawns a
thread running `docimport::import_reporting`. Then, on that thread:

| Step | Where | Allocates | Lives until |
|---|---|---|---|
| Read the whole file | `docimport::mod.rs`, `std::fs::read` | the file, entire | end of `import` |
| Split the chunk stream | `clipstudio::split` | nothing — `&[u8]` slices into the file | — |
| Open the SQLite blob | `sqlite::Database::open` | small | end of `import` |
| Read `Canvas`, walk the layer tree | `clipstudio::canvas`, `Tables::read` | small | end of `import` |
| **`check_bounds`** | `docimport::check_bounds` | nothing | — |
| Per layer: parse `Attribute` | `csblocks::parse_attribute` | small | — |
| Per layer: allocate the destination | `clipstudio::colour` | **canvas × 4 bytes** | end of `import` |
| Per block: inflate | `csblocks::decode_block` | one block, 320 KiB | next block |
| Per block: blit | `clipstudio::colour` | nothing | — |
| Per layer: sRGB-encode in place | `srgb::encode_buffer` | nothing | — |
| Per layer, if masked: the mask | `clipstudio::coverage` | canvas × 1, then **canvas × 4** | end of `import` |

then, back on the drawing thread in `collect_loading` → `install_import`:

| Step | Where | Allocates | Lives until |
|---|---|---|---|
| Device dimension check | `install_import` | nothing | — |
| `ImportedDocument::open` | `docimport::mod.rs` | nothing — the pixel `Vec`s are **moved** into `LayerUpload` | — |
| `Graphics::add_canvas` | `app.rs` | a 1-slice layer array, then a full one | the tab |
| `CanvasRenderer::ensure_slots` | `canvas.rs` | the **whole layer texture array** | the tab |
| `clear_all_layers`, `clear_stroke`, submit | `add_canvas` | — | — |
| One `write_layer_rect` per upload | `install_import` | **one staging buffer per layer, full size** | the next submit |

There is no submit inside that last loop. §8.1 is what that costs.

The container is already well behaved about the things that are easy to get
wrong. `csblocks::for_each_block` hands blocks over one at a time with an
explicit note that materialising the grid would be 1.3 GB, `decode_block`
`take`s the decoder at the block's declared size so a zip bomb costs one block,
and `read_bitmap` bounds a layer's bitmap by *area* against the canvas. The
per-block discipline is exemplary. What is not bounded anywhere is the number of
canvas-sized destinations held simultaneously, which is the whole of the
problem.

### 1.2 `.ora`

Same outer shape, different inner one:

- `std::fs::read`, then the `zip` crate over the bytes in memory.
- Per layer: decode that layer's PNG into a buffer of the **layer's own size**
  (ORA layers carry `x`/`y` and their own dimensions), allocate a canvas-sized
  destination, `container::blit` the one into the other, sRGB-encode in place.
- Same accumulation: every layer's canvas-sized buffer is held until `import`
  returns.

So the *transient* is larger for ORA — a decoded source rect plus a canvas-sized
destination — and the *accumulation* is identical. `.kra` is the same again with
64-square LZF tiles in place of a PNG, and `.psd` is the same again with the
`psd` crate handing back an already-canvas-sized buffer.

### 1.3 Every format stores layers sparsely, and Umber densifies all four

This is confirmed at each reader and it is the single most important structural
fact about the import path. It is also what makes §7.2's contract writable at
all: a signature for carrying residency is only worth having if every producer
can fill it, and this is the table that says they can.

| Format | Stored as | Densified at | What it could yield instead |
|---|---|---|---|
| `.clip` | 256-square zlib blocks; absent blocks are not stored, and `InitColor` says what they hold | `clipstudio::colour`, `vec![0u8; canvas × 4]` | one piece per present block, aligned to 256 |
| `.kra` | 64-square LZF tiles at their own `left,top`; only painted tiles exist | `krita.rs`, `vec![0u8; canvas × 4]` + `blit_tile` | one piece per stored tile, aligned to 64 |
| `.ora` | one PNG per layer at its own size and offset | `openraster.rs`, `vec![0u8; canvas × 4]` + `container::blit` | one piece: the layer's own rectangle clipped to the canvas |
| `.psd` | per-layer channel data over the layer's own rectangle | the `psd` crate's `Layer::rgba()`, already canvas-sized | one piece, canvas-sized — the crate gives nothing better |

A `.clip`'s block stream is *already* the tiled representation a tiled layer
store would want. Umber inflates it, spreads it over a dense canvas-sized
buffer, uploads that dense buffer, and stores it densely. Nothing in the pipeline
after `for_each_block` knows the source was sparse.

**Two of the four can do better than a bounding box and two cannot**, and that
asymmetry is why §7.2's contract is a *sequence* of pieces rather than one
rectangle. A single rectangle per layer is the trim `tiled-layer-storage.md`
§9.1(4) proposes, and for a `.clip` whose painted blocks are scattered across a
20000-pixel page it is barely better than the canvas.

**How much better is not known and it is the number to measure.** The `.clip`
reader records a measurement over 5,438 bitmaps in 33 real documents — of
*sizes*, worst area 15.93× the canvas — but nothing has measured **occupancy**.
§12 says how, and `critique-tiling-import.md` finding 11 adds a correction to it
that this document accepts: block *presence* is an upper bound on residency, not
residency, because Clip Studio keeps a block it has touched even if a later erase
emptied it.

---

## 2. The artist's document, priced

20000 × 5000, 54 layers. All figures decimal, as the refusal states them.

**Per layer:** 100,000,000 px × 4 = **400 MB**.

**Block grid:** `ceil(20000/256) = 79` columns, `ceil(5000/256) = 20` rows, so
**1,580 blocks** per canvas-sized bitmap.

**Block size, exactly rather than as a bound.** `critique-tiling-import.md`
finding 20 objects that `MAX_CHANNELS` is 5 and that a block need not be that
wide. The objection is right in general and narrower than stated here.
`clipstudio::colour` refuses anything but `packing.first == 1` with
`packing.second ∈ {1, 4}`, and `decode_block` refuses a block whose declared size
disagrees with the packing it was handed. So for a **BGRA colour layer** a block
is exactly `(1 + 4) × 65,536 = 320 KiB` and 518 MB a layer is an exact figure,
not an upper bound. A greyscale layer (`second == 1`) is 128 KiB a block and
207 MB a layer; a mask (`second == 0`) is 64 KiB and 104 MB. The **28 GB
document total is an upper bound**, and it is one because the colour/greyscale
mix is unknown, not because the per-block figure is soft. The correction is
accepted in that form.

The 518 MB exceeds the 400 MB destination because the grid pads the canvas out to
20224 × 5120 and carries five bytes per pixel where the destination carries four.

**Host peak, if it were admitted:** the file itself, plus 54 × 400 MB of layer
buffers = **21.6 GB**, plus one 400 MB destination in flight. Add 400 MB per
masked layer, which nothing counts — §4.2.

**GPU peak at open, if it were admitted:**

| | |
|---|---|
| Layer texture array, 54 slices | 21.6 GB |
| Staging buffers, one per upload, held to the next submit | 21.6 GB |
| Stroke scratch, `R8Unorm` | 0.1 GB |
| Stroke colour scratch | 1×1 until a smudging brush is used, then 0.8 GB |
| **Peak** | **≈ 43.3 GB** |

**And a second peak afterwards, on the next layer added.** `ensure_slots` builds
the grown store with the old one still alive, and at 400 MB a slice the growth
quantum is one, so going from *n* to *n+1* slices asks for `(2n + 1) × 400 MB`.
On a document already at 12 slices that is **10.0 GB**. §8.4.

Against 10 GB of VRAM. Even with the staging half fixed the open peak is
21.7 GB, and even at the admission bound of 42 layers it is 16.8 GB — see §5.2.

**Time:** an extrapolation, and it must be treated as one. `loading.rs` records
13.4 s measured on a real 15000×5000 `.clip`, of which 99.6% is decoding blocks;
if that file carried around 40 layers, the rate is roughly 230 million layer
pixels a second, and this document's 5.4 billion is **about 25 seconds**. The
"around 40 layers" is a guess inside an extrapolation and nothing here rests on
it. `cargo run --release -p umber-core --example measure-open -- <file>` settles
it in one command and should be run before any of this is quoted.

---

## 3. Where the time goes

Already measured and recorded in `loading.rs`: **reading the file off disk is
55 ms of 13.4 seconds, building the stack afterwards is nothing, and 99.6% is
decoding one layer's blocks after another.** That measurement stands and this
document adds nothing to it except a breakdown of the 99.6%, which is three
things per layer in this proportion:

1. **zlib inflate**, 1,580 streams a layer producing 518 MB. This is
   irreducible for the pixels actually stored, and it is the *only* part of the
   work that neither the piece contract nor a tiled store would reduce.
2. **The blit**, up to 100 million bounds-checked 4-byte writes a layer, in a
   doubly nested loop with an `Option` per row and per column. It touches 400 MB
   of freshly allocated memory, so nearly all of it is a page fault followed by
   a cache miss.
3. **`srgb::encode_buffer`**, a second full pass over the same 400 MB.

(2) and (3) are together a second and a third full traversal of a buffer that is
mostly zeroes on a sparse layer. They are exactly what disappears under §7.2:
a piece-yielding `.clip` reader encodes 320 KiB at a time, in cache, and never
blits at all.

---

## 4. The refusal, and whether it is the right bound

### 4.1 What the bound is

`ImportedDocument::MAX_TOTAL_BYTES` is 16 GiB, compared in `check_bounds`
against `width × height × 4 × painted`, where `painted` excludes folders. Its
own documentation is careful and correct about what it is for: a few kilobytes of
hostile header can ask for tens of gigabytes before a pixel is decoded, because a
layer's buffer is allocated canvas-sized whatever the source data weighs. A
finite figure turns that into a sentence instead of the process being killed.

**As a host-memory sanity bound it is doing its job and the figure is
defensible.** Nothing below argues for changing it.

### 4.2 It is not the bound that matters, and three things it cannot see

**Masks are not counted.** `check_bounds`' own documentation says so and gives a
sound reason — counting them would make the reader stricter than the writer, and
an Umber document with masks would fail to reopen. The consequence is that the
host peak can be twice `MAX_TOTAL_BYTES`, 34.4 GB, and that **the figure printed
in the refusal is not the figure Umber will allocate.** A `.clip` where every
layer carries a mask reports 21.6 GB and holds 43.2 GB. The refusal's sentence
should not be trusted as a memory statement, and today it reads exactly like one.

**The GPU is not consulted at all.** `install_import` asks the adapter for
`max_texture_dimension_2d` and refuses a canvas wider than a texture may be. It
never asks whether `slots × width × height × 4` can be allocated.

**How many layers a 10 GB card really holds, with the working shown**, because
this document, `slot-lifecycle-and-vram.md` §7.1 and the review gave three
different answers for one quantity and none of them showed it. An RTX 3080's
"10 GB" is 10240 MiB = 10,737,418,240 bytes. At 400 MB a slice that is **26.8
slices with zero headroom** — no surface, no egui atlas, no stroke scratch, no
desktop compositor, no driver reservation. Growing one slice at a time it is
`2c + 1 ≤ 26.8`, so **about 13**. Any figure below those two is a headroom
assumption, and no honest headroom figure is derivable, which is precisely why
§5.3's refusal does not print one. `slot-lifecycle-and-vram.md` §7.1's ~21 and
~11 are those same two numbers with headroom allowed for; this document's
earlier ~22 was the same figure and is superseded.

| | layers of 20000×5000 |
|---|---|
| `MAX_TOTAL_BYTES` admits | 42 |
| 10 GB card, array allocated in one go, no headroom | 26 |
| 10 GB card, grown one slice at a time, no headroom | 13 |
| 10 GB card, with today's staging doubling at open | ~13 |

So the bound the artist meets is between two and four times stricter than the
bound they are told about, and it is a property of their card rather than of
Umber.

**Nothing bounds the peak *transient*.** Two of them, in fact: the upload
staging (§8.1) and the array growth (§8.4).

### 4.3 Does the sibling work change the "not closable by tuning" argument?

CLAUDE.md's argument is that every figure admitting a real 25.6 GB document also
admits a malformed header asking for 25.6 GB, *because they are the same header*
— a layer's buffer is allocated canvas-sized whatever the source weighs.

**§7.2's piece contract retires that argument, and it does so without the
atlas.** The premise is "canvas-sized whatever the source weighs". Once a reader
yields the pieces it actually found, a malformed header claiming a huge canvas
yields no pieces and costs nothing, and a real document costs what its content
costs. The bound would then be stated against something the file can be held to
rather than against something it can merely claim.

This is a **correction to the first draft**, which credited that retirement to
tiled storage. Tiling is what makes the *GPU* side sparse; the host side is
retired by the producer, and the producer change is smaller.

**Until it lands, do not raise `MAX_TOTAL_BYTES`.** Raise the *quality of the
refusal* instead, by adding the check that names the machine — which is
`slot-lifecycle-and-vram.md` §6.3's, not this document's.

---

## 5. The message

CLAUDE.md is emphatic that a refusal must name the bound the file actually met,
and cites the case where a 15000×5000 document was told its canvas was too
large: "the sentence sent the artist to fix the thing that was not broken".
`StackTooLarge` exists because of that case and it is a real improvement. Judged
on the same standard, it is now half right.

### 5.1 What it gets right

It names the stack rather than the canvas. It gives the document's own figure and
the bound, both in decimal GB an artist reads. It does not wear the canvas
refusal's words, and there is a test pinning that
(`a_stack_refusal_names_the_stack_and_not_the_canvas`). All of that is correct
and should stay.

### 5.2 What it gets wrong

*Independently verified by `critique-tiling-import.md` finding 16, which
recomputed the arithmetic and kept the argument. It is unchanged from the first
draft.*

**It omits the canvas, which is a term in the arithmetic it just printed.** The
sentence says "54 layers at 20000×5000, which comes to 21.6 GB", and then offers
one of the two multiplicands as a remedy. Halving each edge quarters the whole
figure: the same 54 layers at 10000×2500 is 5.4 GB, which admits comfortably and
fits the card. Whether that trade is acceptable is the artist's to make — it
costs resolution, and this project's standing rule is that the picture stays
pristine — but not mentioning it is not protecting them, it is withholding the
lever with the most leverage.

**Followed exactly, the advice produces a document Umber cannot draw.** 17.2 GB
divided by 400 MB is 42 layers. An artist who merges twelve layers away and
tries again gets past `check_bounds` and meets 16.8 GB of texture array on a
10 GB card. What happens then is §8.2, and it is worse than the refusal: a long
wait, a swap-thrashed machine, and either a crash box or a canvas nothing can be
painted on. **A refusal whose advice leads to a worse failure is not a refusal
that has named the right bound**, whatever the sentence says about which
constant was compared.

**"Umber holds at most 17.2 GB" reads as a promise about this machine.** It is
literally a statement about a constant, and every reader will take it as a
statement about their computer.

### 5.3 What it should say

Two refusals rather than one, because there are two bounds and they are met by
different documents. **This wording is revised twice over the first draft**, on
`critique-tiling-import.md` finding 14, and the second sentence is now deferred
to the sibling that had already written it.

`StackTooLarge` keeps its structure, loses the advice that leads somewhere
worse, and gains the canvas as a lever:

> This document has 54 layers at 20000×5000, which comes to 21.6 GB of pixels.
> Umber reads at most 17.2 GB of layers from one file.
>
> Merging layers together in the application that made it will bring that down,
> and so will a smaller canvas: each halving of the width and height quarters
> the figure.

**"in the application that made it", not "in Clip Studio Paint".** The review is
right: `ImportError::StackTooLarge` carries `{ width, height, layers, bytes }`
and no format, and its `Display` is generic, so a named application would be
printed over a `.kra` and a `.psd`. Taking `SourceFormat` into the variant is the
alternative and is a better sentence — `format.label()` already exists and every
caller has one in hand — but it is a change to a public error type for one word,
so it is offered rather than recommended.

The second refusal is **`slot-lifecycle-and-vram.md` §7.2's, not this
document's**, and this document adopts it verbatim rather than proposing a
rival:

> This document needs 21.6 GB of graphics memory for its 54 layers at
> 20000 × 5000, and this graphics card could not provide it. Flattening or
> removing some layers, or working at a smaller canvas, will bring it within
> reach.

**The first draft printed "and this GPU has 10.0 GB". That is withdrawn.** wgpu
exposes no total-memory query; `Device::generate_allocator_report` reports
Umber's own allocations and not the card's capacity, and the only route to a real
figure is `Adapter::as_hal`, which costs `ash` and `windows` as direct
dependencies and is untestable on a runner with no card. The sibling had already
worked this out and deliberately omitted the figure; the first draft hedged in
prose and then printed it in the draft, which is the failure mode where a draft
in a design document is what gets implemented. The "needs" figure is exactly
computable and stays; the "has" figure is not and goes.

---

## 6. Should a large document open anyway?

*Compressed on `critique-tiling-import.md` finding 23, which is right that the
constraint settles each of the first three in a sentence and that the section
read as though the decision were open. It is not open.*

- **Dropping layers — refused.** A document that opens missing layers is one
  whose next Save destroys the artist's file, warning or no warning.
- **A proxy resolution — refused.** Painting on a downsampled canvas and
  resampling back is exactly the "subtly wrong pixels" this codebase refuses
  everywhere, and there is no honest version that also lets somebody paint.
- **Read-only, showing the embedded `mergedimage`** — honest and cheap, and
  still not worth building: Umber has no read-only document mode, adding one
  touches every path that writes, and the artist's need is to paint. It is the
  right answer for a file manager and the wrong one for a canvas.

**The middle case is the one that matters and it is not this document's.** A
document too large for the *GPU* but not for the host could open with only the
visible layers resident. That is residency rather than admission, it belongs to
`layer-residency.md`, and it is the only option on this list that gives the
artist their 54-layer document at full fidelity. Everything else here either
refuses it or damages it.

---

## 7. The piece contract

*This section replaces the first draft's §7.2 (streaming) and §7.3 (a
hypothetical `write_layer_tile`). Streaming is ceded to
`formats-and-host-memory.md` §8.2, which claims it. What is claimed here is the
thing `critique-tiling-import.md` finding 1 found in neither backlog.*

### 7.1 Where the review is right, and where its framing is not

The review's blocking finding is that neither document delivers a way for the
importer to tell the store which tiles hold content, and that each author relied
on a change the other did not undertake. **The diagnosis is exactly right and the
framing overstates the deliverable.**

The first draft's §7.3 was accused of assuming a `write_layer_tile` API that
`tiled-layer-storage.md` §7's table does not contain. Re-reading that table: it
does contain it. The `write_layer_rect` row says the path becomes "allocate, then
one `write_texture` per tile" — which *is* the residency-carrying upload, stated
from the store's side. And `write_layer_rect` already takes an arbitrary
rectangle today (`PixelRect { x, y, width, height }`, `write_rect` uses it as the
copy origin).

So there is no missing API. **What is missing is a call-site contract.**
`install_import` calls that function once per layer with `PixelRect { x: 0, y: 0,
width: size.x, height: size.y }` — the whole canvas — so under the atlas it
backs every tile of every layer and stage 2 saves nothing on an import. Tiling
waves at the importer for residency; the importer hands it a rectangle that says
"everything". Neither document is missing a deliverable; both are missing one
sentence about what the caller passes.

That reframing matters because it makes this much smaller than either shape the
review proposed, and because a caller contract is unambiguously the *importer's*
to own.

**One thing the review's fallback gets wrong, and it is worth arguing.** Finding
1's cheap join is "a CPU emptiness scan inside `write_layer_rect`". That is the
wrong home for it: `write_layer_rect` is also the undo path
(`app.rs::swap_patch` calls it once per `PatchPiece`), and those pieces are
recorded damage and are known non-empty, so a scan there is wasted work on every
undo. The scan belongs in the **importer**, which already traverses the buffer
twice (§3) and can do it in the same pass. And the codebase has already made this
decision once, in the direction argued here: `PieceBytes` holds an all-identical
piece as one pixel, with the scan on the *capture* side, and its own comment says
the scan stops at the first pixel that differs. Put the scan where the buffer is
already hot.

### 7.2 The contract, with its signature

**Claimed by this document.** Two type changes in `umber-core`, one loop change
in `umber-app`, and no new renderer API.

```rust
// umber-core::docimport

/// One rectangle of a layer's pixels, in canvas coordinates.
///
/// `bytes` is `rect.area() * 4`, tightly packed RGBA8, sRGB-encoded with alpha
/// premultiplied in linear space — the same form `ImportedLayer::pixels` has
/// always been in, over a smaller rectangle.
pub struct PixelPiece {
    pub rect: PixelRect,
    pub bytes: Vec<u8>,
}
```

and `ImportedLayer::pixels` becomes `Vec<PixelPiece>` (and `mask` likewise),
under three rules:

1. **Every piece lies inside the canvas.** Readers already clip; `clipstudio::canvas_at`
   and `krita::for_each_visible` are where.
2. **Pieces do not overlap.** Block and tile grids give this for nothing; ORA and
   PSD yield one piece, so it is trivially true there too.
3. **A pixel covered by no piece is the slot's empty value.** Not "transparent" —
   *the slot's* empty value, because a mask slice's is white. That convention is
   `tiled-layer-storage.md` §3.4's and is deferred to it by name; getting it
   wrong blanks a layer, which is what that section is about.

The consumer is **unchanged**:

```rust
for piece in &upload.pieces {
    canvas.write_layer_rect(&queue, upload.slot, piece.rect, &piece.bytes);
}
```

That is byte for byte the shape `app.rs::swap_patch` already uses on the undo
path. Nothing in `umber-render` changes for this at all today; under the atlas,
`tiled-layer-storage.md` §7's `write_layer_rect` row already says what it becomes.

**`add_canvas` already clears every slice before the loop**, so a slice that
receives no piece is empty by construction and there is no "this layer is
finished" call to add. Under the atlas, `clear_all_layers` "unbacks the tiles"
(tiling §7 again), so an unwritten region is *unbacked* rather than backed-and-
empty, which is exactly the residency signal wanted. **The contract needs no
completion signal, and that is worth noticing rather than discovering.**

Two things this defers to `tiled-layer-storage.md` and does not decide:

- **The tile side.** If it is 256, a `.clip`'s pieces align to it exactly and
  the store never re-cuts one. `formats-and-host-memory.md` §8.3 makes the same
  argument and this is a second vote for it, not a decision.
- **The empty value per slot kind**, rule 3 above.

### 7.3 What it is worth, and in what order

| built | host peak | tile residency | opens the artist's document? |
|---|---|---|---|
| nothing (today) | every layer | — | no |
| trim alone (tiling §9.1(4)) | every layer, each smaller | bounding box | no |
| stream alone (formats §8.2) | one layer | none | no |
| **pieces (§7.2)** | **one piece** | **exact** | not on its own |
| pieces + atlas stage 2 | one piece | exact | **yes, if occupancy is low enough** |

**The piece contract subsumes trim and stream rather than sitting on top of
them.** A reader that yields pieces is trimmed by construction (a piece is a
rectangle) and streamed by construction (a piece is dropped after it is
uploaded). So if §7.2 is built there is no separate trim step and no separate
stream step, and `tiled-layer-storage.md` §9.1(4)'s "single highest-value item"
and `formats-and-host-memory.md` §8.2 are the same item seen from two sides. That
is a claim worth checking rather than accepting: the reason it holds is that all
three are changes to the same field, `ImportedLayer::pixels`, and only one of
them can be made.

**It does not open the document on its own**, and the review's finding 3 is right
about the ordering: stages 1 and 2 of the atlas save nothing for an *imported*
document without a residency signal, and a residency signal saves nothing without
somewhere sparse to put it. The two have to land together, which is the argument
for writing the contract down now even though nothing consumes it yet.

**And it may not open it even then.** If occupancy is 60% rather than 15%, a
21.6 GB document becomes 13 GB and still does not fit a 10 GB card, and the
answer is `layer-residency.md` instead. §12's survey is what decides that, and
per the review's finding 11 it must report **non-empty** blocks as well as
present ones, or it answers the wrong question at exactly the occupancy where the
answer matters.

---

## 8. Defects found

### 8.1 The upload path is unbanded and holds a staging buffer per layer — live

*Confirmed independently by the coordinator (no submit in or after the loop at
`app.rs` ~4450) and reframed on `critique-tiling-import.md` finding 17, which
found the better fix.*

`install_import` issues one `write_layer_rect` per layer and never submits.
`write_layer_rect` calls `write_rect`, which is one `queue.write_texture` for the
whole rectangle — 400 MB here. In wgpu 29, `queue_write_texture` allocates
`StagingBuffer::new(&self.device, stage_size)` — a fresh mappable buffer the full
size of the copy — copies the data into it, records a `copy_buffer_to_texture`,
and calls `pending_writes.consume(staging_buffer)`, which pushes it into
`PendingWrites::temp_resources`. Those are released by `mem::take` at **submit**
and at no other time. There is no size threshold and no auto-flush.

`add_canvas` submits *before* the loop. Nothing submits during or after it. So
all N staging buffers coexist.

**Where that memory lives is worse than it first looks.** `wgpu-hal`'s Vulkan
backend maps a `MAP_WRITE` buffer to `gpu_allocator::MemoryLocation::CpuToGpu`,
and `gpu_allocator` looks first for `HOST_VISIBLE | HOST_COHERENT |
DEVICE_LOCAL` — the Resizable BAR heap, which is **VRAM** — falling back to plain
host-visible system memory only if no such type exists. Resizable BAR is on by
default on most modern boards.

**There is a second defect in the same line, and it is the mirror of a rule this
codebase already treats as load-bearing.** CLAUDE.md: "*Every readback goes in
bands, because a document can be larger than the largest buffer the device will
make*", and `max_buffer_size` stays at `downlevel_defaults`' 256 MB because
`using_resolution` raises the three texture dimensions only. `read_layer_rect`
bands through `band_rows`. `write_layer_rect` does not band at all, and
`install_import` hands it 400 MB. It works today only because wgpu's internal
`StagingBuffer` is created below the validated `create_buffer` path — a wgpu
implementation detail, not a guarantee.

**So the fix is to band the upload, not to add a bare submit**, and one change
answers both:

```
write_layer_rect:  split the rect into bands by band_rows(readback_limit, …),
                   one write_texture per band, one submit after each
```

The staging peak becomes **one band** rather than one layer, the upload obeys the
same buffer bound the readback does, and the change lands in `umber-render` beside
the reader it mirrors rather than as a loop tweak in `umber-app`. It composes with
§7.2 rather than competing: a piece smaller than a band needs no banding at all,
and a `.clip` piece is 320 KiB.

**Not reproduced against a running Umber.** The chain is read out of wgpu
29.0.4's own source and is short; `slot-lifecycle-and-vram.md` §11's
`measure-vram.rs` is the sweep that settles both the doubling and the heap, and
this document defers to it rather than proposing a second measurement.

### 8.2 Nothing refuses a document the GPU cannot hold — live, and the first draft's fix is withdrawn

`install_import` checks the canvas against `max_texture_dimension_2d` and stops
there. `LayerStore::new` then asks for `width × height × 4 × capacity` bytes with
no further gate. When that fails, wgpu reports an uncaptured device error,
`crash::device_error` panics, and the artist gets a crash box after a minute of
waiting. On Windows it may not even fail: WDDM permits over-commitment and pages
to system RAM, so the likelier outcome is a document that opens and then drags
its whole array across PCIe every frame.

**The first draft recommended a trial `create_texture` of the full array followed
by a drop, and called it "the version that needs no budget reading and no new
platform knowledge, and it asks precisely the right question". That is withdrawn
on `critique-tiling-import.md` finding 15, which is right on both counts:**

1. **It kills the process.** `create_texture` returns no `Result`; failures reach
   `on_uncaptured_error`, wired at `app.rs:4886` to `crash::device_error`, which
   panics. A failing trial allocation produces exactly the crash box the gate
   exists to prevent, one step earlier. It needs
   `push_error_scope(ErrorFilter::OutOfMemory)` around it — an idiom already in
   this repo at `gpu_pipeline.rs:3683` — so "no new platform knowledge" was
   false.
2. **It is a no-op in the case this same section calls more likely.** Under WDDM
   over-commitment the trial *succeeds*, so the gate answers "yes" precisely
   where the artist is about to get an unusable canvas.

One nuance worth keeping rather than conceding whole: a *scoped* trial is not
worthless even without a budget threshold, because today's failure is a panic
rather than a refusal, so converting the hard-failure case into a sentence is a
real improvement. It is simply insufficient, and there is no reason to build it
separately, because:

**`slot-lifecycle-and-vram.md` §6.3 has already designed the right mechanism and
this document adopts it rather than proposing a rival.** `try_reserve(device,
queue, needed) -> Result<(), Vram>`: `push_error_scope(OutOfMemory)`, the same
body `ensure_slots` has, `pop`, and on OOM drop the new store and leave
`self.layers` exactly as it was. With `memory_budget_thresholds.for_resource_creation`
set — **after `with_env`, which resets the field, and that is the trap** — the
over-commitment case becomes a catchable error too, which is the half a trial
allocation could never reach.

What this document contributes to it is only the **call site**: `install_import`,
beside the `max_texture_dimension_2d` check that is already there, before the
document exists, so a refusal leaves the session exactly as it was. And the note
that the check must count **the transient**, §8.4, or it admits a document that
then dies growing into itself.

### 8.3 One stale comment

`install_import` says "The importer bounds itself at 16384 px".
`ImportedDocument::MAX_DIMENSION` is `Document::MAX_EDGE`, which is **32768**.
The check below it is correct; only the comment is out of date. Verified
independently by the review.

### 8.4 The `ensure_slots` growth transient — live, and the other half of the symptom

*Neither this document's first draft nor `tiled-layer-storage.md` carried it.
Raised by `critique-tiling-import.md` finding 21 and verified independently by
the coordinator. `slot-lifecycle-and-vram.md` §4 owns it; it is recorded here
because the first draft attributed the whole reported symptom to §8.1, and that
diagnosis was incomplete.*

`ensure_slots` builds the grown `LayerStore`, records a
`copy_texture_to_texture` from the old one, and only then assigns — so old and
new are both live across the copy. `grown_capacity`'s doubling loop is gated on
the resulting array fitting in `GROWTH_DOUBLING_BUDGET_BYTES` (256 MiB), and at
400 MB a slice it never runs, so `growth_quantum` is **1** and the array grows
exactly one slice at a time.

Peak for a growth from *c* to *c+1* slices is therefore `(2c + 1) × slice_bytes`.
At this canvas:

| document is at | growing to | peak |
|---|---|---|
| 8 slices | 9 | 6.8 GB |
| 12 slices | 13 | **10.0 GB** |
| 20 slices | 21 | 16.4 GB |

**This fires every time the artist adds a layer**, where §8.1 fires once at open.
For a painting application that is the ordinary gesture, so it may well be the
larger half of what the artist is seeing — and it is invisible in a
diagnosis that only looks at the open.

Two consequences for *this* document rather than for the sibling that owns the
fix. The device gate in §8.2 must be stated against `c + n` slices and not `n`,
or it admits a document that opens and then dies on the first layer added. And
§11's advice has to say so: merging down to twelve layers and then adding a
thirteenth is a worse position than opening with thirteen.

---

## 9. Progress and blocking — already built, and built well

The brief asked whether anything on screen says a large document is opening.
**It does, and it was done to this project's own standard.**

`loading.rs` runs `import_reporting` on a worker and wakes the loop with an
`EventLoopProxy`. `docimport::Progress` counts **layers, not bytes** — the
correct unit, chosen from the measurement that the wait is one CPU-bound loop
over layers. `tabs::loading` draws a modal with a bar, and `Loading::fraction`
answers `None` until the reader has counted the layers, so the track draws empty
rather than at a guessed position. The reader reports *before* each layer rather
than after. `done` and `total` are packed into one `AtomicU32` so a torn pair
cannot put the bar past its own end. Reports are throttled to changes, because a
wake is a whole frame. The modal has no Cancel, and says why: the decode cannot be
interrupted without polling a flag through four readers, so stopping is not
offered rather than offered and ignored.

Three suggestions, of which the third is promoted into §0 on the review's
finding 19:

1. **The detail line could name the layer.** "Layer 31 of 54" is what it says;
   "Layer 31 of 54 — Rough colour" is available at zero cost, because the reader
   has the name in hand before it decodes.
2. **A long open should say how large the document is.** "20000 × 5000, 54
   layers" under the bar costs nothing and is what an artist wants to see when a
   wait is unexpected.
3. **Run the device gate before the worker starts, not after it finishes.** Today
   a document refused for the card's dimension limit (§10) — or, once §8.2 lands,
   for its capacity — is refused *after* the full decode, so the artist waits
   twenty-five seconds to be told no. The canvas size is in the header:
   `clipstudio::canvas` and `check_bounds` both run before a pixel is decoded.
   Handing the device's limits into `import` as a ceiling, or reporting the size
   through a first `progress` call and gating on the main thread, moves the
   refusal to the front. **This is the artist-visible half of the whole
   document** and it is the cheapest thing in it.

### The embedded thumbnail, and where it does belong

`docimport::preview` already reads the flattened picture every format carries —
`mergedimage.png` for `.ora` and `.kra`, `CanvasPreview.ImageData` for `.clip`,
the composite section for `.psd` — with no layer walk, no canvas allocation and
no GPU.

**There is a role for it and it is smaller than it looks.** Showing it behind the
loading modal would tell the artist they have the right file. What it must not do
is become the canvas or be mistaken for it: the module's own docs say it is
whatever the writing application last saved, and that **nothing that decides
pixels may read it**. If it is drawn it must be visibly a preview — dimmed,
behind the modal, and labelled.

The stronger use is the one in §6: a document too large to open could *show* its
preview beside the refusal, so the artist can see which file they were refused.
That is honest, costs one PNG decode, and needs no read-only document mode.

---

## 10. The device ceiling and the backend

The artist's canvas is 20000 wide. `Document::MAX_EDGE` is 32768.
`measure-limits` recorded an RTX 3080 reporting **32768 on Vulkan and 16384 on
Dx12**, and 16384 is a hard limit of both the D3D12 specification and Metal — so
this is a Vulkan ceiling, not a card one.

**Which backend Umber picks was verified against the vendored wgpu source**, not
inferred. `critique-tiling-import.md` finding 19 could not check it because the
reviewer did not fetch wgpu; the trace is recorded here with its citations so
nobody has to take it on internal consistency. In
`~/.cargo/registry/src/*/wgpu-core-29.0.4/src/instance.rs`:

- `Instance::new` (line 101) calls `try_add_hal` in a fixed order — **Vulkan
  (118), Metal (120), Dx12 (122), GLES (124)** — pushing each into
  `instance_per_backend`, whose own doc comment (line 75) says "the ordering in
  this list implies prioritization and needs to be preserved".
- `request_adapter` (line 481) iterates `instance_per_backend` in that order and
  extends one `adapters` vector (line 548).
- It then `sort_by_key`s on device type alone (lines 551–583). `sort_by_key` is
  **stable**, so among adapters of equal `DeviceType` the earlier backend wins,
  and takes `adapters.into_iter().next()`.
- `Gpu::create_instance` uses `InstanceDescriptor::new_without_display_handle_from_env()`,
  whose `with_env` (wgpu-types `instance.rs:106`) narrows the backend set only if
  `WGPU_BACKEND` is set.

**So on Windows with a Vulkan driver present, Umber gets the Vulkan adapter and
the 32768 ceiling, and the artist's 20000-wide canvas is admissible.** Not a live
bug. `slot-lifecycle-and-vram.md` §6.4 reaches the same conclusion independently.

It remains a live *hazard*:

- **`WGPU_BACKEND=dx12` silently halves the largest openable canvas**, and
  CLAUDE.md itself recommends that variable for chasing driver bugs. So does a
  machine with no Vulkan ICD, where the Vulkan backend enumerates nothing. The
  resulting refusal names the GPU when the cause is the API.
- **The refusal arrives after the whole decode**, §9's third note.

Two cheap recommendations:

1. **Name the backend in that message.** `adapter.get_info().backend` is already
   logged at startup. "…larger than 16384 pixels on a side (Direct3D 12)" turns a
   wrong diagnosis into a lead.
2. **Log the ceiling and the backend together at startup**, so a bug report
   carries it without anyone having to ask.

Preferring the backend with the higher `max_texture_dimension_2d` is
deliberately **not** recommended: backend choice affects far more than one limit,
`request_adapter` is where wgpu's own policy lives, and picking a backend for a
limit most documents never approach pays once and costs for ever.

---

## 11. What the artist should do today

Given the code exactly as it stands, and holding to "nothing loses a pixel":

**First, check which backend is in force.** Run with `RUST_LOG=umber_render=info`
and read the `GPU:` line. If it says `Dx12`, unset `WGPU_BACKEND` — on Vulkan the
20000-wide canvas is legal.

**Then merge down in Clip Studio, and merge down further than looks necessary.**
The target is not the 42 layers `MAX_TOTAL_BYTES` admits, and it is not the 26 a
10 GB card holds if the array is allocated in one go. It is set by §8.4: a
document grown to *c* slices peaks at `(2c + 1) × 400 MB` the next time a layer is
added, so **ten to twelve layers** is the number, and the reason is not the open
but everything after it.

- In Clip Studio, merge each group down to one raster layer (`Layer → Merge
  selected layers`). A merge of layers that are all Normal at 100% is exact;
  where a blend mode or an opacity is involved the *composite* is preserved
  exactly and the separation is what goes.
- **Do the merging in Clip Studio, not in Umber, and do not add layers
  afterwards.** Opening with twelve and then adding a thirteenth in Umber asks
  the card for 10.0 GB (§8.4), which is a worse position than opening with
  thirteen. Until that transient is fixed, treat the layer count a document opens
  with as its ceiling.
- Keep the original `.clip` untouched. This is a working copy.

**Or split the document.** Export each group as its own `.clip` or `.psd` at full
resolution and open them separately. Nothing is lost at all — not even the
separation — and it is the right answer if the layers are genuinely needed apart.
The cost is that they cannot be seen composited in Umber.

**Reducing the canvas works and costs resolution.** 10000 × 2500 is a quarter of
the pixels and would let all 54 layers open. It is the only option here that is
not lossless, so it is the last resort rather than the first suggestion, and the
artist should decide it rather than be steered into it.

**What will not help:** exporting to `.psd`. Umber's PSD reader densifies exactly
the same way (the `psd` crate hands back a canvas-sized buffer per layer), so the
memory is identical and 8-bit RGB is the only depth accepted — and per §1.3 it is
the one format that could not benefit from the piece contract either. Nor will
exporting each layer as a PNG, because Umber has no "place a file as a new layer
in the current document"; that is a recorded known gap in
`docs/document-import.md`.

---

## 12. What to measure, and with what

**Occupancy is the one measurement this document turns on**, and per
`critique-tiling-import.md` finding 11 it must report two numbers rather than
one. It is `tiled-layer-storage.md` §10's `survey-residency.rs`, and this
document's contribution is the correction and the reason:

- **Present blocks / grid blocks.** Free: `decode_block` answers `Some(None)` at
  line 339, before the `ZlibDecoder` at line 366, so counting presence needs no
  inflate and no allocation.
- **Non-empty blocks / grid blocks.** Needs the inflate, because Clip Studio
  keeps a block it has touched even after an erase emptied it. Still bounded by
  one block at a time.

The gap between the two is what §7.2's importer-side emptiness scan would
recover and what `tiled-layer-storage.md` §9.4's reclamation would recover later.
**If the two numbers are close, presence is a good residency signal and the piece
contract gets the whole win for free. If they diverge, the scan is load-bearing
and §9.4's reclamation is not "not urgent".** Nothing currently knows which.

And the decision this settles: at 15% occupancy the atlas is obviously right; at
60% a 21.6 GB document becomes 13 GB, still does not fit a 10 GB card, and the
answer is `layer-residency.md` instead.

Already written, and worth running on the artist's own files:

```sh
cargo run --release -p umber-core --example measure-open -- *.clip
cargo run --release -p umber-core --example survey-documents -- <folder>
```

`measure-open` will print `refused: …` for the 21.6 GB one, which confirms the
refusal is `StackTooLarge` and not something else; run it on the files that *do*
open, which is where the reported symptom is. `survey-documents` gives canvas,
entries, folders and painted bytes — the four figures `check_bounds` compares —
and its own warning applies: it decodes, so it needs the host memory the
documents cost.

**The VRAM sweep is `slot-lifecycle-and-vram.md` §11's `measure-vram.rs`**, which
already proposes the `generate_allocator_report` sweep this document and its
sibling both asked for separately. It should carry §8.1's staging doubling and
§8.4's growth transient as two of its cases. This document does not propose a
second one.

**The card's real capacity** is not measurable through wgpu's safe API.
`measure-limits` prints the shape limits and attempts a real allocation at
`MAX_EDGE`; extending it to sweep slice counts and report the largest array that
succeeds would give an empirical figure, and it is still not a figure a refusal
may print, for §5.3's reason.

---

## 13. What could not be settled from the code

- **Occupancy, present *and* non-empty.** §12. Everything about whether tiling
  and the piece contract help *this* artist turns on it.
- **The real open time** for a 20000×5000×54 document. §2's ~25 seconds is
  extrapolated from a 13.4 s measurement on a differently shaped file, using a
  guessed layer count.
- **Whether the staging buffers land in VRAM or in system memory** on the
  artist's machine. The code path prefers the ReBAR heap; whether that heap can
  satisfy a 400 MB request depends on a board setting nothing here can read.
- **What actually happens when the array does not fit** on that machine — a fatal
  `create_texture` error, or a WDDM over-commitment that opens and crawls. The
  two look completely different to the artist, want different gates, and only
  running it settles which. This is now more important than the first draft made
  it, because finding 15 showed the two cases need different mechanisms.
- **Whether the other documents that "make VRAM skyrocket" pass `check_bounds`
  comfortably or narrowly**, and whether the artist saw the symptom at open or on
  adding a layer. `survey-documents` answers the first in one run; the second
  decides whether §8.1 or §8.4 is the more urgent fix, and only the artist can
  answer it.

---

## 14. Response to `critique-tiling-import.md`

Recorded so the next reader does not have to diff two drafts.

### Accepted, and what changed

| finding | change |
|---|---|
| **1** (blocking) — no residency signal | §7 rewritten. The deliverable is **claimed** here as the piece contract, with its signature. See the rebuttal below on its size. |
| **14** (blocking) — "this GPU has 10.0 GB" | §5.3. The figure is withdrawn; `slot-lifecycle-and-vram.md` §7.2's wording is adopted verbatim. |
| **14** — "Clip Studio Paint" in a generic error | §5.3. Now "the application that made it", with taking `SourceFormat` into the variant offered as the better alternative. |
| **14** — three figures for one quantity | §4.2 now shows the working (26.8 and ~13 with zero headroom) and reconciles all three. |
| **15** (blocking) — the trial `create_texture` | §8.2. **Withdrawn.** `slot-lifecycle-and-vram.md` §6.3's `try_reserve` is adopted; this document contributes only the call site and the transient. |
| **17** — upload unbanded while readback bands | §8.1. The fix is now banding rather than a bare submit, which is a better shape and the same code. Genuinely new and this document's to carry. |
| **18** — `allocation-accounting.md` does not exist | Every deferral retargeted to `slot-lifecycle-and-vram.md`. |
| **20** — §11's advice ignores the growth transient | §11 now sets the target from §8.4 and says not to add layers afterwards. |
| **21** — the `ensure_slots` growth transient | New §8.4, and it is in §0 and §2. The first draft's diagnosis of the reported symptom was incomplete. |
| **23** — §6 is too long | Compressed to three bullets plus the residency paragraph, which was the useful one. |
| **3** — what the two together buy | §7.3's table. |
| **11** — presence over-reports residency | §1.3 and §12; the survey must report both numbers. |

Also accepted from outside the review: **streaming is
`formats-and-host-memory.md` §8.2's**, which claims it explicitly. The first
draft proposed it without knowing. §0.1.

### Rejected or narrowed, with reasons

**Finding 1's framing — that §7.3 assumed an API `tiled-layer-storage.md` §7's
table does not contain.** Narrowed. That table's `write_layer_rect` row already
says the path becomes "allocate, then one `write_texture` per tile", and
`write_layer_rect` already takes an arbitrary rectangle. There is no missing API;
there is a missing **call-site contract**, because `install_import` passes the
whole canvas. The gap is real and the review found it; it is one sentence rather
than a new deliverable, and that matters because it makes the work small enough
to do now. §7.1.

**Finding 1's fallback — a CPU emptiness scan inside `write_layer_rect`.**
Rejected in that location, accepted in another. `write_layer_rect` is also the
undo path (`app.rs::swap_patch`, once per `PatchPiece`), whose pieces are
recorded damage and known non-empty, so the scan would be wasted on every undo.
It belongs in the importer, which already walks the buffer twice — and the
codebase has already made this call once in that direction, in `PieceBytes`,
whose all-identical scan is on the capture side. §7.1.

**Finding 20's objection to 320 KiB.** Narrowed. `MAX_CHANNELS` is a bound, but
`colour()` refuses any packing but `(1, 1)` and `(1, 4)`, and `decode_block`
refuses a block whose declared size disagrees with the packing. So 320 KiB and
518 MB a layer are **exact** for a BGRA colour layer. The document total is an
upper bound because the colour/greyscale mix is unknown, which is a different
reason from the one given. §2.

**Finding 15, in one respect only.** The trial allocation is withdrawn, but not
because it "answers no only where the current behaviour already refuses loudly" —
today's behaviour is a *panic*, not a refusal, so a scoped trial would still have
converted a crash box into a sentence. It is withdrawn because it is insufficient
against over-commitment and because a better mechanism was already designed
next door, not because it was worthless. §8.2.

**Finding 19's "could not verify" on the backend trace.** Not a disagreement, but
the trace was verified against the vendored wgpu source in the cargo registry,
and §10 now carries the file and line numbers so it stops resting on internal
consistency. The review is right that it is not load-bearing.

### Not this document's, and left to the owners

Findings 2, 4, 5, 6, 7, 8, 9, 10, 12, 13 and 22 are about
`tiled-layer-storage.md`. Two of them change what this document may rely on and
are noted where they bite: **finding 2** (trim does not bound the host peak;
streaming does) is why §0.1's table separates the three changes, and **finding 5**
(the atlas is itself a `texture_2d_array` and pays `ensure_slots`' transient) is
why §8.4 does not treat tiling as retiring that transient.
