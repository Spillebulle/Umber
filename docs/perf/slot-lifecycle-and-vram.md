# The lifecycle of a GPU allocation, and how much memory Umber thinks it has

What Umber allocates, when it gives it back, and the answer to "how much
graphics memory is left" — which today is that nobody asks.

The report that started this is a folder of real Clip Studio documents at
20000 × 5000. One of them was refused at import for holding 21.6 GB of pixels;
the ones that opened filled a 10 GB RTX 3080. Both halves are worth taking
seriously, and they are **different failures**: the first is a host-memory bound
that fired correctly, and the second is a graphics-memory bound that does not
exist.

This document owns the accounting — how allocations are made, grown, parked,
recycled and given back. It does not own the layout: whether a layer should be
canvas-sized at all is `docs/perf/tiled-layer-storage.md`'s question. Everything
here is written to be true either way, and §10 says which of it tiling would
retire — a claim this revision had to correct.

Nothing recommended here costs a pixel. Where a recommendation trades latency
for memory, §9 says how much latency and where it lands.

---

## 0. What revision 1 got wrong

`docs/perf/critique-allocation-formats.md` reviewed this document against the
code and found five blocking defects. I have checked every one against the
source myself and **accept all five**. Recorded here rather than quietly fixed,
because two of them were wrong in the direction that damages a picture, and
because the reason the first one happened is instructive.

1. **The shrink recommendation was the headline and it was wrong.** It targeted
   `LayerStack::slot_capacity_needed()`, on the argument that no slot number
   moves. That argument is true and it was never the binding objection: two
   producers put slices *above* that figure without going through `SlotPool` at
   all, so shrinking to it silently deletes every baked effect slice and the
   float's preview. **Withdrawn** — §5.
2. **Its predicate fired on ordinary documents**, and `bake_effects` runs every
   frame, so it would have reallocated and copied the whole layer array twice a
   frame. A 5.2 GB per-frame transient at 100 Mpx, shipped as a memory saving.
3. **"Give the float's spare back at `end_float`, the safest shrink there is"**
   was unreachable: effect slices sit above the spare, so it is a hole in the
   middle.
4. **`try_reserve` as specified would still panic**, one line after the check
   written to prevent the panic. §6.3.
5. **The largest peak multiplier on the reported path was in neither document**:
   `install_import` accumulates a canvas-sized staging buffer per layer, and
   that path's OOM is fatal by construction. §4.4.

The instructive part is (1). I read `EffectCache`'s module comment — it states
in as many words that effect slices are handed out above everything `LayerStack`
has claimed — quoted `effect_slot_base` correctly in §4.3's own table, and then
did not carry either into §5. The failure was not a missing fact. It was
reasoning about `SlotPool` in a section whose subject was the *array*, and never
asking whether the two had the same top. **`slot_capacity_needed()` bounds the
pool's claims; it does not describe what the array holds**, and this document
now says so wherever the distinction bites.

---

## 1. The recommendations, in the order they are worth doing

1. **Put `queue.submit` inside `install_import`'s upload loop.** One line, no new
   API, no policy. Today the loop holds a full canvas-sized staging buffer per
   layer until the *next frame's* submit, which roughly doubles peak graphics
   memory on the exact operation being reported — and does it on a path where
   wgpu **loses the device** on OOM, so no refusal, no error scope and no budget
   threshold can rescue it. §4.4.
2. **Count the *transient*, not the steady state.** `ensure_slots` allocates the
   new array while the old one is still alive, so growing from `c` slices to `n`
   holds `c + n` at once. With (1) unfixed the import path is no better off than
   the hand-built one; with (1) fixed, a hand-built document still stalls at
   about eleven slices on a 10 GB card. §4.2.
3. **Refuse, in `umber-app`, before the allocation** — and cover the upload path
   as well as the array. `install_import` already refuses a canvas the device
   cannot hold, in the right place with the right wording; the byte check belongs
   beside it. The mechanism needs no new dependency: `MemoryBudgetThresholds` on
   the instance plus `push_error_scope(ErrorFilter::OutOfMemory)`. **The scope
   must be popped between the texture and the first view** or it catches nothing
   — §6.3.
4. **Thread `slot_capacity_needed()` into `resize`.** Already prescribed by
   `CanvasRenderer::resize`'s own doc comment. It is the only shrink in this
   document that survives §5's withdrawal, and it survives because a resize
   allocates a whole new array anyway — so it has **no transient at all**, and it
   is safe from §5.1 because a resize calls `EffectCache::forget_all` and ends
   any float first.
5. **Make "hold it in case they want it again" read the canvas size.**
   `slice_bytes(doc_size) > GROWTH_DOUBLING_BUDGET_BYTES` is already this
   codebase's own test for "too large to speculate on", and only `grown_capacity`
   and `initial_slots` consult it. The per-dab colour scratch (800 MB at 100 Mpx)
   and the effect working set (up to 1.3 GB) are held for the document's life on
   reasoning that is right at 2048². §8.3.
6. **Release a *background tab's* working set, never its layer array.** Up to
   1.3 GB a tab at 100 Mpx with no readback and no pixel cost — smaller than
   revision 1 claimed, because the effect *slices* in that figure cannot be given
   back until a correct shrink exists. §9.3.
7. **Stop reserving the float's spare on documents that have no float.**
   `effect_slot_base` is `slot_capacity_needed() + 1` unconditionally, so a
   document with any effect and no float permanently holds one unused
   canvas-sized slice: 400 MB at the reported canvas, for a gesture nobody has
   started. §5.3.
8. **Write `examples/measure-vram.rs` before believing any figure in §3.** The
   arithmetic here is arithmetic. §11.

Withdrawn from revision 1: the general shrink policy. See §5.

---

## 2. What is allocated, and who owns it

Three scopes, and conflating them is how a figure ends up wrong by the number of
open tabs.

**Per process, once.** `Shared` — three shader modules, the pipelines, the bind
group layouts and two samplers. `CanvasRenderer::for_document` clones it, which
is a handful of atomic increments, so a second document costs none of it. Nothing
in it is canvas-sized. The surface swapchain is also per process: at
`desired_maximum_frame_latency: 2` a driver typically holds three images, so a
4K window is about **99 MB** that has nothing to do with any document. egui's
font atlas, its brush-preview textures and the layer thumbnails it uploads are
per process too, and are **not measured anywhere** — §11.

**Per document.** Everything else. Each open tab owns a `CanvasRenderer` and
therefore its own layer array, its own stroke scratch and its own working sets.

**Per gesture.** The float, the flip scratch, the export target, the commit
backdrop, the readback staging buffers and — the one revision 1 missed — the
**upload** staging buffers. These decide the *peak* rather than the resting
figure.

---

## 3. The inventory

Byte figures at three canvases. `20000 × 5000` is the reported document; note it
is **exactly the same pixel count as 10000²** — 100 megapixels — so the two share
a column. The edge is not the same, and §6.4 has what that costs.

### 3.1 Per document, held for the document's life

| | bytes/px | 2048² (4.19 Mpx) | 100 Mpx |
|---|---|---|---|
| layer array, **per slice** (`Rgba8UnormSrgb`) | 4 | 16.8 MB | **400 MB** |
| stroke scratch (`R8Unorm`) | 1 | 4.2 MB | 100 MB |
| dab instance buffer (65,536 × 40 B) | — | 2.6 MB | 2.6 MB |
| view uniforms (191 draws × two `vec4` arrays) | — | 6.2 KB | 6.2 KB |
| dab / commit / transform uniforms | — | ~1 KB | ~1 KB |
| smudge probes, 2 × 8² | — | 4 KB | 4 KB |
| thumbnail target + staging | — | 32 KB | 32 KB |
| tip, tip colour, grain, selection (1×1 stand-ins) | — | ~16 B | ~16 B |

The stand-ins are the point of that last row: a document with no bitmap tip, no
paper and no selection allocates four single-texel textures rather than four
canvas-sized ones. That is already the right shape and nothing here changes it.

### 3.2 Per document, allocated on demand and then **kept**

| | bytes/px | 2048² | 100 Mpx |
|---|---|---|---|
| per-dab colour scratch (`Rgba16Float`), first smudging stroke | 8 | 33.6 MB | **800 MB** |
| effect working set, four `R8Unorm` planes | 4 | 16.8 MB | 400 MB |
| … plus a centred outline's band plane | +1 | +4.2 MB | +100 MB |
| … plus the flood's seed pair (`Rg16Uint`) | +8 | +33.6 MB | +800 MB |
| effect uniform blocks (48 × 127 × 256 B) | — | 1.49 MiB | 1.49 MiB |
| **one baked effect's slice** (a slice of the layer array) | 4 | 16.8 MB | 400 MB |
| **the float's reserved spare, whether or not a float exists** | 4 | 16.8 MB | 400 MB |

Worst effect working set is **1.3 GB** at 100 Mpx, held for as long as the
document has any effect at all — `EffectScratch` is dropped only by
`forget_all`, which runs on a resize.

The last row is §5.3's finding and it is new: `Editor::effect_slot_base` is
`slot_capacity_needed() + 1` unconditionally, so any document with a baked
effect allocates the slice at index `next` and never writes to it.

### 3.3 Per gesture

| | 2048² | 100 Mpx |
|---|---|---|
| float: base + floating copy + preview slice | 50.3 MB | **1.2 GB** |
| … plus its selection mask, up to canvas-sized | +4.2 MB | +100 MB |
| flip scratch, one canvas in layer format | 16.8 MB | 400 MB |
| export: offscreen target | 16.8 MB | 400 MB |
| export / capture: one readback staging band | ≤ 16.8 MB | **268 MB** |
| autosave capture: flattened preview target | 16.8 MB | 400 MB |
| blended commit backdrop (widest × tallest piece) | ≤ 4.2 MB | ≤ 5.1 MB |
| undo readback: one batch of pieces | ≤ 256 MiB | ≤ 256 MiB |
| **upload staging, per unsubmitted `write_layer_rect`** | 16.8 MB | **400 MB** |

The readback band is `readback_limit` = `device.limits().max_buffer_size`,
256 MiB under `downlevel_defaults`. At 20000 wide a padded row is 80,000 bytes,
so a band is 3,355 rows and the buffer is 268,400,000 bytes, just inside.
`band_rows` working exactly as designed.

**The upload staging row obeys none of that** and is §4.4.

### 3.4 What the reported document actually costs

At 20000 × 5000, per slice, 400 MB. Layers and masks each take one.

| slices | steady state | growing into it (`c + n`) | **importing (array + staging)** |
|---|---|---|---|
| 10 | 4.0 GB | 7.6 GB | 8.0 GB |
| **11** | 4.4 GB | **8.4 GB** | **8.8 GB** |
| 20 | 8.0 GB | 15.6 GB | 16.0 GB |
| 21 | 8.4 GB | 16.4 GB | **16.8 GB** |
| 42 (`MAX_TOTAL_BYTES`'s admission) | 16.8 GB | 33.2 GB | 33.6 GB |
| 128 (64 layers, each masked) | 51.2 GB | — | — |
| 256 (`MAX_SLOTS`) | 102.4 GB | — | — |

Add 100 MB for the stroke scratch and about 100 MB for the swapchain, and note
that a 10 GB card is not 10 GB to Umber: the desktop compositor and the driver
have their own. Call it 9 GB usable.

**So the ceiling is about eleven slices whichever way the document arrives** —
by import or by hand — and revision 1's claim that an import reaches twenty-one
was wrong because it counted only the array. Every bound Umber has is one or two
orders of magnitude above eleven.

---

## 4. Growth, and the two transients

### 4.1 The growth policy is right and the docs are accurate

Read the real code before believing the folklore. `grown_capacity` doubles only
while the resulting array stays inside `GROWTH_DOUBLING_BUDGET_BYTES` (256 MiB),
and past that rounds up to a whole `growth_quantum`. The mutation CLAUDE.md warns
about (`current.max(1).next_power_of_two()`) would indeed waste 102 GB at 10000²,
and the shipped code does not do it.

| canvas | slice | `initial_slots` | quantum | behaviour |
|---|---|---|---|---|
| 256² | 256 KB | 4 | 1024 | doubles freely |
| 2048² | 16.8 MB | 4 | 16 | doubles to 16, then 16 at a time |
| 4096² | 67.1 MB | 3 | 4 | doubles to 4, then 4 at a time |
| 100 Mpx | 400 MB | **1** | **1** | never doubles; grows exactly |

At the canvas in question the policy degenerates to exact growth and allocates
**one** slice up front. That is correct: at 400 MB a slice there is no
speculation that could be afforded.

### 4.2 And that is exactly where it hurts

Exact growth means a growth *per layer*. Each allocates the whole new array while
the old one is still alive, copies every slice across, and only then drops the
old — `self.layers = grown` runs after `queue.submit`, and wgpu keeps a texture
alive for any submission naming it. So the peak is `c + n` slices, and with exact
growth that is `2c + 1`.

On 9 GB usable at 400 MB a slice, `2c + 1 ≤ 22` gives `c ≤ 10`. **A document
built up by hand stalls at eleven layers**, and it does not stall gracefully: the
failing `create_texture` reaches `on_uncaptured_error` → `crash::device_error`,
which panics on purpose.

**The transient is inherent to a single texture array and cannot be optimised
away.** A texture cannot be partly freed, the composite binds one array, and a
binding array of independent textures needs a feature `downlevel_defaults` does
not carry. Rounding up to a larger quantum reduces the *count* of growths, not
the peak of any one — and at 400 MB a slice a larger quantum makes the peak
worse. The only real answers are to allocate once at the size the document needs,
and to refuse before the allocation rather than after.

Two small consequences:

- **`CanvasRenderer::for_document` should take the slot count**, so a document
  being opened is built at its final capacity rather than at 1 and then grown.
  `add_canvas` already has the number in hand.
- **`ensure_slots` should skip the copy when the old array holds nothing.** This
  is traffic, not memory, so it is the lesser of the two.

One thing this cannot settle by reading: whether the driver has actually released
the old texture by the time the *next* growth asks. wgpu keeps it alive for the
submission naming it, so a rapid sequence of growths may hold three arrays rather
than two, and the hand-built ceiling would be nearer seven than eleven. §11.

### 4.3 Where growth is asked for, and what makes it stick

| caller | asks for | sticks? |
|---|---|---|
| `Graphics::add_canvas` | the document's own `slot_capacity_needed()` | yes |
| `App::add_layer` (and mask add, undo restore) | `slot_capacity_needed()` | yes, until the slice is given back |
| `CanvasRenderer::begin_float` | `slot_capacity_needed() + 1` | **yes, for ever** |
| `CanvasRenderer::bake_effects` | one past the highest effect slice | **yes, for ever** |

The last two rows are the ones §5 turns on: both index **above**
`slot_capacity_needed()`, and neither goes through `SlotPool`.

The often-quoted "delete-then-add cycles reach `MAX_SLOTS` and 102.4 GB at
10000²" needs one correction. `StackShape::byte_len` charges a parked slice to
the undo budget, so parked slices are bounded by `budget / slice_bytes` — at
100 Mpx and the default 512 MB budget, **one**; at the budget's 32 GB ceiling,
eighty. So at that canvas the drifting figure is not reachable through parking.
What reaches 51.2 GB there is 64 live layers each with a mask, and 102.4 GB is
that plus 127 effect slices. At 2048² the parking route is real.

### 4.4 The upload staging accumulation — the largest peak multiplier, and it is fatal

`install_import` loops `write_layer_rect` per layer, and **there is no
`queue.submit` in the loop or after it** (verified: `app.rs` ~4450–4467, and no
`submit` anywhere between there and the end of the function). `write_layer_rect`
→ `write_rect` → `Queue::write_texture`.

In wgpu 29, `queue_write_texture` allocates a fresh `StagingBuffer` of the whole
copy size and hands it to `PendingWrites::consume`, which pushes it into
`temp_resources`. `PendingWrites`' own doc comment: "The commands accumulated
here are automatically submitted to the queue the next time the user submits a
wgpu command buffer." There is no size threshold and no auto-flush.
`StagingBuffer::new` calls the hal's `create_buffer` directly, so
`max_buffer_size`'s 256 MiB does not apply and a 400 MB write succeeds — which is
why this works today and why it accumulates.

**So opening a 21-slice document at 100 Mpx is 8.4 GB of layer array plus 8.4 GB
of live staging.** That is the fourth column of §3.4 and it collapses revision 1's
distinction between the import path and the hand-built one.

And it is worse than a peak. `StagingBuffer::new` maps its errors through
`handle_hal_error` — **not** `handle_hal_error_with_nonfatal_oom` — and
`handle_hal_error` calls `self.lose(&error.to_string())` on `OutOfMemory`. The
device is lost, unrecoverably, and no error scope helps. A refusal placed only at
`ensure_slots` guards the allocation that is *not* where the reported document
fails.

**The fix and its exact reach.** `queue.submit([])` flushes: `pre_submit` runs
inside `submit` unconditionally, whatever the command-buffer count. So one empty
submit per layer inside the loop is the whole change.

What it does **not** do is bound staging at one canvas, and it is worth being
precise rather than optimistic. `consume` pushes the buffer into the
submission's `temp_resources`, released when that submission's fence signals. So
submitting per layer bounds the live staging by **the number of submissions the
GPU has not yet retired**, not by one. Under a stream of 400 MB uploads that may
still be several. To bound it hard, poll every few layers:

```
for upload in &uploads {
    canvas.write_layer_rect(…);
    gfx.gpu.queue.submit([]);            // flush this layer's staging
    // and, where slice_bytes is large, every few layers:
    // let _ = gfx.gpu.device.poll(wgpu::PollType::wait_indefinitely());
}
```

The poll blocks, which is exactly why it belongs here and nowhere near the
drawing loop: this is the open path, a file dialog has just closed, and the
alternative is losing the device. Whether the bare submit is enough on its own is
a measurement, not a reading — §11.

**A second site, smaller and real.** `swap_patch` also loops `write_layer_rect`
with no submit, over the pieces of one undo patch. The pieces sum to at most the
patch, which the undo budget bounds, so an undo of a full-canvas stroke at
100 Mpx accumulates 400 MB of staging rather than 8 GB. The general rule is what
matters: **any loop of `write_layer_rect` accumulates canvas-scale staging until
something submits.**

---

## 5. Giving capacity back — the general shrink is withdrawn

### 5.1 Why: `slot_capacity_needed()` does not describe what the array holds

Revision 1's §5.1 claimed that shrinking to `LayerStack::slot_capacity_needed()`
changes no slot number, because that figure is one past the highest slice ever
claimed.

The premise is true. `SlotPool::next`, `take`, `give_back`'s tail compaction and
`slot_capacity_needed` all hold: nothing issued through the pool is ever at or
above `next`, and a parked slice holds a `SlotClaim` exactly as claimed.

The conclusion is false, because **two producers of slice numbers do not go
through `SlotPool` and both sit above `next`**:

```
0 .. next-1       layers, masks, parked slices        (SlotPool)
next              the float's preview                 (begin_float)
next+1 .. next+k  k baked effect slices               (effect_slot_base + bake_effects)
```

`begin_float` takes `preview_slot = reserved = slot_capacity_needed()` — index
`next`, one past the end of a depth-`next` array. `Editor::effect_slot_base` is
`slot_capacity_needed() + 1`, and `bake_effects` grows the array to
`highest + 1`. `EffectCache`'s module comment states this in the same file.

**What shrinking to `slot_capacity_needed()` would do.** It deallocates every
effect slice and the float's spare. `CachedEffect` is keyed on
`(source, mask, kind, revisions, params, live)` and `is_fresh` would still say
the entry is fresh, so the composite's `LayerDraw` names a slice past the new
array's depth. That is not a validation error — WGSL defines an out-of-range
array layer as returning zero — so **the drop shadow silently disappears** and
nothing is reported. `EffectScratch::bound_capacity` rebuilds the *working set*,
not the entries.

The argument the recommendation was built to defeat — "a layer's slot never
changes" — was never the binding objection. The binding one is occupancy, and
the two are different questions that revision 1 ran together.

### 5.2 And the predicate would have thrashed every frame

Revision 1 proposed `shrink when needed * 2 <= capacity`, with `needed` being
`slot_capacity_needed()`. Four layers (`next == 4`) and four enabled effects:
`base` is 5, effect slices land at 5–8, `bake_effects` calls `ensure_slots(9)`,
capacity is 9. `4 * 2 = 8 ≤ 9` — **it fires**, taking all four effect slices with
it. `render` calls `bake_effects` on the next frame, which calls
`ensure_slots(9)` again.

Allocate 4, copy 9→4, drop 9; allocate 9, copy 4→9, drop 4. Every frame. At
2048² that is ~440 MB of allocation and copy traffic per frame; at 400 MB a slice
it is a **5.2 GB transient every frame** on the canvas this whole document is
about. That is the ordinary state of any document with a shadow and a stack
shorter than its effect count — not a contrived case.

### 5.3 What a correct shrink would target, and the smaller finding underneath it

The target that genuinely covers every producer is a renderer-side maximum of
three numbers the renderer already holds:

```
max(slot_capacity_needed(),
    float.preview_slot + 1,
    highest live effect slice + 1)
```

Two of those three terms are not renumbering hazards — nothing outside the
renderer names them — but they are **occupancy** hazards, and only the first
term is covered by "no slot number moves". Any future attempt must also carry an
explicit "not while a float is up" and a hysteresis argument that names
`bake_effects` as the thing that will immediately undo it.

That is a much smaller reclaim than revision 1 advertised, for much more care. It
is not worth building now, and §10 says tiling would retire the question. **The
general shrink is withdrawn rather than deferred.**

**But there is a real 400 MB sitting in the same place, and it is separable.**
`effect_slot_base` reserves index `next` for the float *unconditionally*, whether
or not a float exists. So a document with any baked effect and no float allocates
one canvas-sized slice that nothing ever writes to — 400 MB at the reported
canvas, held for the session, for a gesture nobody has started. Revision 1's
"give the float's spare back at `end_float`" was unreachable because effect
slices sit above it; this is the same waste approached from the side that works.
Two ways out, and the second is better:

- Make the reservation conditional — `effect_slot_base` returns
  `slot_capacity_needed()` when no float is in flight. This shifts every effect
  slice down by one whenever a float starts or ends, which invalidates the whole
  effect cache twice per transform gesture. Cheap in memory, expensive in bakes.
- **Have the float take its spare from `EffectCache`'s free list** instead of
  from a reserved index. Effect slices are already freed rather than parked — the
  model can never hand one to a layer, so no `PixelPatch` can ever name one —
  which is exactly the property a float's preview needs. It removes the
  reservation, removes the hole `end_float` leaves in the middle, and makes the
  array's occupied depth a single contiguous run. It is a change to
  `EffectCache`, `effect_slot_base` and `begin_float`'s `reserved`, and it is the
  design worth writing up.

### 5.4 `resize` is the one shrink that survives, and it survives cleanly

`CanvasRenderer::resize` rebuilds the array at `self.layers.capacity` — a figure
decided against the canvas being left behind. Its own doc comment states the
failure and the fix: a 512² document legitimately holding 256 slices becomes
4.29 GB at 2048² and **102.4 GB** at 10000², arrived at through a dialog rather
than through the growth rule, and the fix is to thread the live slot count in
from `App::apply_canvas` and rebuild at
`grown_capacity(0, live, slice_bytes(new_size))`.

Three things make this the best benefit-to-risk item in the document:

- It is the **only shrink with no transient at all**, because a resize allocates
  a whole new array anyway. Shrinking makes that peak *smaller*.
- It is **safe from §5.1**, and not by luck: `resize` calls
  `EffectCache::forget_all` and `end_float` before it rebuilds, so at the moment
  it allocates there are no effect slices and no float above `next`. It is the
  one moment in the program when `slot_capacity_needed()` really does describe
  the array.
- The copy depth should be `min(old capacity, new capacity)`, not the old one.
  Copying slices about to be discarded is the same waste in traffic.

A signature change through one call site.

---

## 6. Does Umber know how much memory it has? No.

### 6.1 What it does ask the device

Two limits, both correctly:

- `max_texture_dimension_2d`, in `install_import` and via
  `CanvasLimit::of_device`. A bound on **shape**.
- `max_buffer_size`, as `readback_limit`, honoured by `band_rows`. A bound on one
  **buffer** — and note §4.4: the *upload* staging path does not go through it.

Neither says anything about how much memory exists or is left. `AdapterInfo`
carries no memory figure. An allocation that cannot be satisfied reaches
`on_uncaptured_error` → `crash::device_error` → panic → the crash box. Right for
a device error, wrong for a document that is merely too big, and today there is
no way to tell them apart.

`Device::set_device_lost_callback` exists in wgpu 29 and Umber sets none.

### 6.2 What wgpu 29 offers

Checked against the vendored source rather than remembered.

**`InstanceDescriptor::memory_budget_thresholds`** — a
`MemoryBudgetThresholds { for_resource_creation: Option<u8>, for_device_loss:
Option<u8> }`, as a percentage of the OS-reported budget. On Vulkan,
`error_if_would_oom_on_resource_allocation` reads `VK_EXT_memory_budget`'s
`heap_usage` and `heap_budget` and returns `DeviceError::OutOfMemory` when
`heap_usage + size >= heap_budget / 100 * threshold`. D3D12 does the equivalent
through its sub-allocator. **This is the mechanism**: it turns "the driver pages
to system memory and the canvas becomes a slideshow" into an ordinary, catchable
`Error::OutOfMemory` on the exact allocation that would not fit.

Four things to know:

- **`InstanceDescriptor::with_env()` resets the field to default.**
  `Gpu::create_instance` calls `new_without_display_handle_from_env()`, which
  ends in `with_env`. The threshold must be set *after* that call or it is
  silently discarded.
- **Vulkan does nothing without `VK_EXT_memory_budget`** — the check returns
  `Ok(())` when the extension is absent. Metal and GL support none of it. Best
  effort, and it must never be described as a guarantee.
- **It runs on every buffer and texture creation**, including per-frame ones:
  `probe_canvas`'s 8² target on every frame of a smudging stroke, `drive_thumb`'s
  uniform buffer, `commit_blended`'s backdrop, `pick_patch`'s target once a frame
  under the eyedropper. Each becomes one
  `vkGetPhysicalDeviceMemoryProperties2`. Probably noise, and **not measured**.
- **`for_device_loss` must stay unset**, and for a stronger reason than revision
  1 gave: setting it makes `check_if_oom` *deliberately* lose the device on the
  next poll, which is precisely the unrecoverable outcome the refusal exists to
  avoid.

**`Device::push_error_scope(ErrorFilter::OutOfMemory)`** and
`ErrorScopeGuard::pop()`. wgpu's own documentation: "The pop takes effect
immediately; the future does not need to be awaited before doing work that is
outside of this error scope." So `pollster::block_on` on it is not a GPU stall.

**`Device::generate_allocator_report()`**, with `total_allocated_bytes` and
`total_reserved_bytes`. Not feature-gated. Reports what **this device** has
sub-allocated — not the card's capacity, not other processes. It builds a `Vec`
of every live allocation, so it is a diagnostic, not a drawing-path call. Ideal
for a Settings readout and for `measure-vram.rs`; useless as the figure a refusal
is stated against.

**`Adapter::as_hal::<hal::api::Vulkan>()` / `::Dx12`.** `wgpu` re-exports
`wgpu_hal` as `wgpu::hal`, so no new *wgpu* dependency;
`vulkan::Adapter::raw_physical_device()` and `shared_instance()` are public, as
is `dx12::Adapter::raw_adapter()`. The only route to the card's real heap sizes.
Costs `ash` and `windows` as direct dependencies of `umber-render`, is `unsafe`,
is per backend, and is untestable on a CI runner with no card. Worth it only to
put a real figure in a refusal message, and only after the cheaper half is in
place.

**The question revision 1 could not settle is now answered, and the answer is
split.** `create_texture` uses `handle_hal_error_with_nonfatal_oom`, which returns
the error *without* losing the device; so does `create_buffer`. But
`handle_hal_error` calls `self.lose(…)` on `OutOfMemory`, and that is what the
staging path of §4.4 uses. **So the device survives a refused array allocation and
does not survive a refused upload.** That asymmetry is the whole reason
recommendation 1 is a submit and not a refusal.

### 6.3 The mechanism, with the ordering that makes it work

Revision 1 specified `try_reserve` as "the same body `ensure_slots` has" inside
an OOM scope. **That would still panic**, one line after the check.

`ensure_slots`'s body begins `let grown = LayerStore::new(device, self.doc_size,
capacity);` and `LayerStore::new` creates the texture and then, before returning,
`1 + 2 × capacity` texture views from it — the array view, `slot_views` and
`raw_slot_views`. When `create_texture` fails, wgpu hands back an error object.
Creating a view of one resolves the texture id to `InvalidResourceError`, which
converts into `CreateTextureViewError::InvalidResource`, which classifies as
**`ErrorType::Validation`**. An `ErrorFilter::OutOfMemory` scope does not catch
it, so the first `create_view` reports to `on_uncaptured_error` — which is
`crash::device_error`, which panics on purpose.

Revision 1 even wrote the mechanism down in §6.2's caveat ("the returned
`Texture` from a failed create is an error object; it must be dropped, never
used") and then specified an implementation that used it immediately. That was
the gap.

```
Gpu::create_instance     set memory_budget_thresholds AFTER with_env
                         for_resource_creation: Some(~90)
                         for_device_loss: None

CanvasRenderer::try_reserve(device, queue, needed) -> Result<(), Vram>
    push_error_scope(OutOfMemory)
    create the TEXTURE only                     <- split out of LayerStore::new
    pop, and check                              <- BEFORE any create_view
    on Some(OutOfMemory): drop the error texture, leave self.layers untouched
    otherwise: build the views, copy, clear, swap in

App::install_import      beside the max_texture_dimension_2d check already there
App::add_layer           the same call, refusing with a notice
```

Pushing a Validation scope as well is the tempting alternative and is worse — it
would swallow genuine validation errors that must stay fatal.

One caveat this uncovered and which nothing else records: `create_texture` itself
creates internal **clear views** for a `RENDER_ATTACHMENT` texture, using the
*fatal* `handle_hal_error`. Those are small objects and an OOM on one is
unlikely, but it means even the nonfatal-OOM texture path has a fatal sub-step
under real memory pressure. It is a reason to leave headroom in the threshold
rather than to set it at 99.

`ensure_slots` keeps its infallible signature for the callers that genuinely
cannot fail. The fallible one is the new door, and the *policy* — what headroom
to leave, whether to count the transient — is a pure function of two integers
beside `grown_capacity`, testable without a device for the reason `band_rows` and
`Clip::place` are.

**The transient must be in the arithmetic**, and after §4.4 the transient on an
import is `c + n` slices **plus** the staging for every layer written before the
next submit. A check that counted only `n` would pass a document that then died
during the copy; one that counted `c + n` would pass a document that then died
during the upload.

### 6.4 One thing the reported document hits that has nothing to do with memory

20000 exceeds 16384, a hard limit of the D3D12 specification and of Metal.
`measure-limits` reports an RTX 3080 at **32768 on Vulkan and 16384 on Dx12**. So
the same document opens or is refused depending on which backend wgpu picked, and
`WGPU_BACKEND=dx12` refuses it outright. `install_import`'s existing message is
correct and names the real figure — but check the backend before concluding
anything about memory.

---

## 7. The bound that is missing, and how a refusal should read

### 7.1 The gap, precisely

`ImportedDocument::MAX_TOTAL_BYTES` is 16 GiB and it is a bound on **host**
memory: `ImportedLayer::pixels` is a canvas-sized RGBA8 buffer per layer, all
held at once. The 21.6 GB refusal was that bound firing correctly on a 54-slice
document, and its message — "Flattening or removing some layers will bring it
within reach" — is actionable and names the right thing.

The gap is underneath it:

| | at 20000 × 5000 |
|---|---|
| `MAX_TOTAL_BYTES` admits | 42 layers, 17.2 GB of host memory |
| a 9 GB usable card reaches, **with the submit fix** | ~21 slices, 8.4 GB |
| a 9 GB usable card reaches, **as shipped today** | ~11 slices |

Between eleven and forty-two layers the import passes every check Umber has and
the GPU is what fails — today as a lost device or a panic, in neither case as a
sentence. That band is where the report lives, and **recommendation 1 alone
roughly doubles its lower edge.**

It is not only layers: a mask is a slice, a baked effect is a slice, the float
takes one, and §5.3's reservation takes one more.

### 7.2 What the refusal must say

CLAUDE.md's rule is that a refusal names the bound the file actually met, and it
records what happens when it does not: a 15000×5000 document told its canvas was
too large when the problem was layer count "sent the artist to fix the thing that
was not broken". Four bounds now exist and they are genuinely different.

| bound | what it is about | what the artist can do |
|---|---|---|
| `CanvasTooLarge` | one edge past `MAX_DIMENSION` | a smaller canvas |
| device `max_texture_dimension_2d` | one edge past **this card's** limit | a smaller canvas, or another backend |
| `StackTooLarge` | every layer's pixels in host memory | flatten or remove layers |
| **new** | the layer array will not fit on this card | flatten or remove layers, or a smaller canvas |

The new one belongs in `umber-app`, not `umber-core`, because `umber-core` may
not learn about the adapter. `install_import`'s existing device-limit refusal is
the precedent and the place.

> **Could not open "sketch.clip"**
>
> This document needs 8.4 GB of graphics memory for its 21 layers at
> 20000 × 5000, and this graphics card could not provide it. Flattening or
> removing some layers, or working at a smaller canvas, will bring it within
> reach.

The "needs" figure is computable exactly — `slices × slice_bytes`, plus the
transient — so it is a real number, not a guess. The "has" figure is not, without
§6.2's hal route, and the sentence deliberately does not claim one.

A version belongs on `add_layer` too, and there the sentence is different:
nothing has failed to open, one layer has failed to appear, and the figure that
matters is the transient.

### 7.3 The refusal cannot be the only defence

§4.4's staging OOM loses the device before any scope sees it. So the ordering is:
**fix the submit first, then add the refusal.** A refusal shipped without the
submit fix would be a control that guards the smaller of the two failures and
reads as though it guarded both — which is the class of half-applied guard this
codebase records elsewhere as worse than none.

---

## 8. Recycling, clearing, and holding things "in case"

### 8.1 Clearing is done, and it is correct

CLAUDE.md's rule is that a recycled slot still holds the old layer's pixels and
must be cleared on the GPU. It holds at all four sites: `App::add_layer` clears
the returned slot; `Graphics::add_canvas` calls `clear_all_layers` and
`clear_stroke`; `ensure_slots` clears every slice **above** the old capacity, and
only those; `resize` clears the whole new array before the copy.

A new mask goes through `fill_layer_white` instead, and its comment gets the sRGB
question right: 1.0 encodes to 255 either way, so a mask really does arrive at
`0xff`.

**One redundancy, free to fix.** `add_canvas` calls `ensure_slots(slots)` — which
already clears every slice above the old capacity — and then `clear_all_layers`,
which clears every slice again. On a 21-slice import that is 21 redundant render
passes on the frame the document opens. Individually both call sites are right;
together they duplicate. It belongs beside §4.2's other two small items.

### 8.2 Is clearing 400 MB a cost worth managing?

Probably not, and this is a place to be honest about not knowing. A clear is a
render pass with `LoadOp::Clear` and no draw, which on modern hardware is a
fast-clear rather than a write of 400 MB. `clear_all_layers` records one pass per
slice. Very likely microseconds and **not measured**.

`resize` clears the whole array at the old capacity even when most of it is
unused, which §5.4's `min(old, new)` change tidies up for free.

### 8.3 What is kept, and the rule that should decide it

Three things are allocated lazily and then held for the document's life, each for
a reason sound at 2048² and expensive at 100 Mpx:

| | reason given | cost at 100 Mpx |
|---|---|---|
| per-dab colour scratch | "a painter who reaches for a blender once will reach for it again" | 800 MB |
| effect working set | "an effect whose spread is being dragged crosses zero repeatedly" | 400 MB – 1.3 GB |
| thumbnail target + buffer | "cheaper than the allocation churn of a per-job pair" | 32 KB |

The third is right at any size. The first two are speculation on the artist's
behalf, which is precisely what `GROWTH_DOUBLING_BUDGET_BYTES` exists to bound —
and neither consults it.

> **`slice_bytes(doc_size) > GROWTH_DOUBLING_BUDGET_BYTES` is this codebase's own
> test for "this canvas is too large to speculate on". Everything that holds an
> allocation in case it is wanted again should ask it.**

Under that rule nothing changes at 2048². Above about 8192² the colour scratch is
released when a stroke that used it ends, and the effect working set when the
last effect is switched off. The cost is one reallocation on the next such
stroke or edit — latency, not a pixel.

The float already follows this rule without being told to: everything it owns is
allocated at `begin_float` and released at `end_float`, "two canvas-sized
textures and a slice of the layer array is not something to hold for a session in
case somebody presses T". That comment is the argument. (Its *slice* is the
exception, and §5.3 is why.)

---

## 9. Several documents at once

### 9.1 What is shared, and it is the right set

`Shared` holds the pipelines, shaders, layouts and samplers, is `Clone` by
reference count, and `for_document` hands it over — so a second document costs no
shader compilation. Nothing canvas-sized is shared and nothing canvas-sized
should be: the layer array *is* the document. Closing a tab drops the renderer,
and `close_document` says why it has to reach the GPU at all.

### 9.2 What is not shared and could be

Nothing important. The dab instance buffer (2.6 MB) and the smudge probes (4 KB)
are per document and could be per process, since only one document is painted on
at a time. Not worth the coupling.

### 9.3 A background tab's working set should go; its layer array must stay

`switch_document` calls `finish_transform()` and `finish_stroke()` before the
swap. So the moment a document stops being active, its stroke scratch has been
committed and cleared, its per-dab colour scratch likewise, its float has been
committed and `end_float` has run, and its effect working set is derived from
pixels that are not going anywhere.

None of that describes the document. Releasing it costs one reallocation on the
switch back and reclaims, at 100 Mpx:

| | reclaimed |
|---|---|
| stroke scratch | 100 MB |
| per-dab colour scratch | 800 MB |
| effect working set | 400 MB – 1.3 GB |
| **total, no readback, no pixel cost** | **up to 2.2 GB** |

**The correction revision 1 needed**: it went on to say the effect *slices* were
"the interesting part of that: the only reclaim that touches the layer array".
They are not reclaimable. `EffectCache::forget_all` returns them to the cache's
own free list; it does **not** shrink `LayerStore`, and `ensure_slots` never
shrinks. So on a background tab that memory is not given back at all until a
correct shrink exists — which §5 has just withdrawn. The 2.2 GB stands; the
effect slices are extra and are blocked behind the thing that was wrong.

The latency is a reallocation and a rebake on the frame the tab returns. At
2048² that is under a millisecond of allocation and `measure-effects`'s
0.3–1.8 ms of bake. At 100 Mpx the bake is 4–34 ms — but at that canvas
`EFFECT_LIVE_PIXELS` already defers the bake to the commit, so this changes
nothing an artist would notice.

### 9.4 Releasing a background tab's *layer array* — declined, with the numbers

`resumed` already rebuilds storage for every open document, so the machinery
exists. But it rebuilds **empty** storage: "their contents are gone", which is
right for the Android surface-loss path where the pixels genuinely have not
survived. Reusing it for a tab switch means keeping the pixels, which means:

- a blocking `read_layer_rect` per slice on the way out — through
  `poll(wait_indefinitely)`, on the frame somebody clicked a tab — and a
  `write_layer_rect` per slice back, which by §4.4 needs its own submits;
- 400 MB of **host** memory per slice while the tab is away, so a 20-slice
  document trades 8 GB of VRAM for 8 GB of RAM;
- at a practical 10 GB/s each way, 8 GB down and 8 GB up is about **1.6 s** of
  transfer, plus the mapped-buffer memcpy that `Capture::copy_chunk` measures at
  about 5 ms per 16 MB — 125 ms per 400 MB slice, so another 2.5 s each way.

Five seconds to switch tabs, to move a problem from one kind of memory to
another. The honest answer to "two large documents do not fit" is that they do
not fit, and the fix is tiling.

---

## 10. What tiling would retire — corrected

Revision 1 listed §4's growth transient as retired by tiling, "because a tiled
layer grows by adding tiles, and there is no monolithic array to reallocate".
**That is wrong**, and `docs/perf/critique-tiling-import.md` §5 is right: the
atlas in `docs/perf/tiled-layer-storage.md` is itself a `texture_2d_array` of
pages — it has to be, because the composite indexes pages from a loop — and
growing a `texture_2d_array` means creating a new one and copying with the old
alive. That is the same transient in a different costume, and at one page at a
time it is the same policy `grown_capacity` measured and refused, at six times
the frequency.

So the corrected table:

**Retired by tiling.** §3's per-slice figure, §3.4's table, and most of §7.1's
gap. Also §5's shrink question, which is why withdrawing it costs little.

**Not retired.**

- **§4.2's transient.** It moves from the layer array to the atlas. Its
  *magnitude* falls, because only populated and resident pages exist — but the
  shape is unchanged, and the fix is the same one: the atlas must answer to
  `grown_capacity`'s byte budget, with pages allocated from a capacity that grows
  in quanta. Revision 1's "allocate once at the size needed" survives verbatim.
- **§4.4's staging accumulation.** Tiling changes what is uploaded, not whether
  `write_texture` holds a staging buffer until a submit. Streaming an import
  fixes the *host* side and does nothing about staging unless the loop also
  submits. Recommendation 1 is untouched by any of it.
- **§6's mechanism.** A budget check and a catchable OOM are wanted whatever the
  layout — tiles run out too, just later.
- **§7.2's wording rule** and the fourth error variant.
- **§8.3's "speculation should read the canvas size" rule.** The effect working
  set is not tiled by that design and stays canvas-sized.
- **§9.3's release of a background tab's working set.**

Two of the eight recommendations are retired by tiling, and they are the two that
have already been withdrawn or are cheapest. The rest stand on their own.

---

## 11. What is not settled, and what would settle it

Everything below is arithmetic or reading, not measurement. This project has a
standing record of guessed figures causing bugs, so they are named rather than
asserted.

**Write `crates/umber-render/examples/measure-vram.rs`.** On the real adapter:

1. `generate_allocator_report()`'s `total_allocated_bytes` and
   `total_reserved_bytes` after each of: a fresh renderer, `ensure_slots` to N,
   a smudging stroke, an effect bake, a float, an export. In particular **whether
   a 400 MB texture costs 400 MB** — alignment, tiling modes and metadata
   surfaces mean it may not. If it is 1.3× then "25 slices on a 10 GB card" is
   19, and every conclusion here survives, because they are all "far above what a
   card can hold". This is the figure most often demanded and least consequential.
2. Walk `ensure_slots` from 1 to N one slice at a time at a large canvas,
   reporting the allocator's peak. That is §4.2's `2c + 1` confirmed or refuted —
   and specifically **whether a rapid sequence holds three arrays rather than
   two**, since wgpu keeps a texture alive for the submission naming it. If it
   does, the hand-built ceiling is nearer seven than eleven and the case for
   `for_document` taking a slot count gets stronger.
3. **The staging accumulation, before and after the submit fix.** Its labels are
   in the allocator report ("(wgpu internal) Staging"). This is the largest
   unmeasured multiplier on the exact path being reported, and it also answers
   whether the bare `submit([])` bounds it acceptably or whether the periodic
   poll of §4.4 is needed.
4. Whether `VK_EXT_memory_budget` is present on this machine's adapters, and what
   `heap_budget` and `heap_usage` say. Without it §6.3's mechanism is inert on
   Vulkan and nothing on screen would say so.
5. One `create_texture` with and without `for_resource_creation` set, so §6.2's
   third caveat is a figure rather than a shrug. The per-frame allocation sites
   are the ones at risk.
6. A `clear_view` over a 400 MB slice, which §8.2 guesses is free.

**Specifically not settled by reading the code:**

- **What egui holds.** The font atlas, the brush-preview cache and the layer
  thumbnails are per process and nothing counts them. This is the one figure that
  could change a *recommendation*: if it is 500 MB rather than 50 MB, §9.3's
  per-tab reclaim is competing with a per-process cost nobody has looked at, and
  the thumbnail cache's own policy becomes worth a section.
  `generate_allocator_report`'s labels answer it directly.
- **What the swapchain really costs.** Three images is usual; the driver decides.
- **Whether the reported document was refused on memory or on the DX12 edge
  limit.** §6.4. Worth asking before anything else.
- **How many layers the reported documents actually have.** Every figure in §3.4
  is per slice, and masks and effects each take one.

**Answered since revision 1**, and recorded so nobody re-derives them: the device
survives a refused `create_texture` (`handle_hal_error_with_nonfatal_oom`) and
does **not** survive a refused view or a refused upload (`handle_hal_error` calls
`lose`). A view of an error texture is a **Validation** error, not an OOM one. An
empty `queue.submit([])` does flush pending writes, because `pre_submit` runs
unconditionally inside `submit`.
