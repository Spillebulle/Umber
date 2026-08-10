# A critique of the allocation and format designs

Reviewing `docs/perf/slot-lifecycle-and-vram.md` (the allocation document) and
`docs/perf/formats-and-host-memory.md` (the formats document), against the
code rather than against the prose.

Everything below was checked. Where I could not settle something I say so and
name what would settle it. Nothing here was built or run — no `cargo` command
was issued — so every finding is a reading of source, of `wgpu` 29.0.4's
vendored source, or arithmetic over constants I read.

**Verdict in one line each.** The OOM mechanism is very nearly right and its
API research is exact — one implementation detail would make it panic on the
path it exists to protect. The shrink claim is **wrong**, in a way that damages
a picture, and it is wrong for a reason both documents' own sibling text
already contains. The formats document's two self-declared load-bearing facts
are both **true**, its BC7 refusal is **correct**, and its packing proposal
under-counts what it touches by about four paths.

---

## Summary of findings

| # | Severity | What |
|---|---|---|
| 1 | **BLOCKING** | `slot_capacity_needed()` is not an upper bound on occupied slices. Shrinking to it destroys baked effect slices and the float's spare. |
| 2 | **BLOCKING** | The §5.2 shrink predicate thrashes the array **every frame** on any document with effects. |
| 3 | **BLOCKING** | Recommendation 4 — give the float's spare back at `end_float` — is unreachable as stated: effect slices sit *above* it. |
| 4 | **BLOCKING** | `try_reserve` as specified still panics. `LayerStore::new` builds views from the error texture, and a view of an error texture is a **Validation** error, which an `OutOfMemory` scope does not catch. |
| 5 | **BLOCKING** | Neither document counts `install_import`'s GPU staging accumulation — the largest single peak multiplier on open — and that path's OOM is **fatal by construction**, so a refusal at `ensure_slots` does not cover the reported failure. |
| 6 | SUBSTANTIVE | Everything else about the OOM mechanism verified exactly, including the `with_env` trap. Credit where due. |
| 7 | SUBSTANTIVE | Packed masks need `SlotPool` to become a `(slot, channel)` pool. "Park a slice when all three channels are free" is the wrong granularity and reopens the corruption parking exists to prevent. |
| 8 | SUBSTANTIVE | `clear_layer` and `fill_layer_white` clobber whole slices. Both are on the mask feature's own path; neither is named. |
| 9 | SUBSTANTIVE | `slot_revision` is per slice, so three packed masks share one revision — cross-invalidating thumbnails and the effect cache. |
| 10 | SUBSTANTIVE | `ColorWrites` is pipeline state, not dynamic. Packing costs pipeline permutations, not "free at run time". |
| 11 | SUBSTANTIVE | §9.3's "up to 2.2 GB per background tab" is right, but the effect *slices* in it are not reclaimable without finding 1 being fixed first. |
| 12 | SUBSTANTIVE | The load-bearing figures, and which ones would change a recommendation if wrong by 2×. |
| 13 | MINOR | Formats §10.1(3) contradicts formats §5.3: two thirds, not three quarters. |
| 14 | MINOR | `add_canvas` re-clears slices `ensure_slots` has already cleared. |
| 15 | MINOR | Ordering: the two documents do not conflict, but one item should be dropped rather than sequenced. |
| 16 | — | Claims I checked and found correct, listed so nobody re-checks them. |

---

## 1. BLOCKING — `slot_capacity_needed()` is not an upper bound on occupied slices

The allocation document's §5.1 is its boldest claim and it is stated twice, in
recommendation 3 and again in the section:

> Shrinking to `LayerStack::slot_capacity_needed()` changes **no slot number at
> all**, because that figure is already one past the highest slice ever
> claimed.

> That figure is `SlotPool::next`, which is by construction one past the
> highest number any claim holds — live *or* parked, since a parked slice is
> still a `SlotClaim`. An array of depth `next` therefore holds every claimed
> slice at its own index, unchanged.

The first sentence of the second paragraph is **true**. I checked
`SlotPool::next` (`crates/umber-core/src/layer.rs:163`), `take`
(`:194`), `give_back`'s tail compaction (`:242`) and
`slot_capacity_needed` (`:1315`). Nothing issued through the pool is ever at or
above `next`, and a parked slice holds a `SlotClaim` exactly as the document
says.

The conclusion drawn from it is **false**, because two producers of slice
numbers do not go through `SlotPool` at all, and both sit *above* `next`:

**The float's preview slice.** `CanvasRenderer::begin_float`
(`crates/umber-render/src/canvas.rs:4495`):

```rust
let preview_slot = reserved;
self.ensure_slots(device, queue, preview_slot + 1);
```

`reserved` is `Editor::float_reserved()` → `slot_capacity_needed()`
(`crates/umber-app/src/editor.rs:1966`). So the preview occupies index `next`.
An array of depth `next` holds indices `0..next-1`; the preview is one past
the end of exactly the array the document proposes shrinking to.

**Every baked effect slice.** `Editor::effect_slot_base`
(`crates/umber-app/src/editor.rs:1965-1967`):

```rust
pub fn effect_slot_base(&self) -> u32 {
    self.layers.slot_capacity_needed() + 1
}
```

and `bake_effects` grows the array to fit them
(`crates/umber-render/src/canvas.rs:6035-6037`):

```rust
if let Some(highest) = slots.iter().copied().max() {
    self.ensure_slots(device, queue, highest + 1);
}
```

So on a document with `k` baked effects the array's occupied depth is
`next + 1 + k`, and slices `next` through `next + k` hold, respectively, the
float's spare and `k` effect results the composite is about to sample.

`EffectCache`'s own module comment says this in as many words
(`canvas.rs:2021-2029`): "Effect slices are handed out from
`[base, base + capacity)` where `base` is one past everything `LayerStack` has
claimed — the `+ 1` being the slice a floating transform previews into". The
allocation document quotes `effect_slot_base` correctly in its §4.3 table
("`CanvasRenderer::bake_effects` | one past the highest effect slice") and then
does not carry that fact into §5.

**What shrinking to `slot_capacity_needed()` actually does.** It deallocates
every effect slice and the float's spare. `bake_effects` does not re-derive
freshness from capacity — `CachedEffect` is keyed on `(source, mask, kind,
revisions, params, live)` and `is_fresh` will say the entry is fresh — so the
composite's `LayerDraw` for an effect names a slice index past the new array's
depth. That is not a validation error; a `textureSampleLevel` with an
out-of-range array layer is defined to return zero in WGSL, so the effect draws
as nothing and the picture is silently missing a shadow. `EffectScratch::
bound_capacity` (`canvas.rs:1939`, checked at `:6569`) would rebuild the
*working set*, not the entries.

The document's §5.1 list of what a shrink reclaims is fine — the doubling
overshoot, the float's spare, effect slices after switch-off, the tail above a
delete, the resize gap are all real. It is the **target figure** that is
wrong. The correct target is a renderer-side maximum of three numbers the
renderer already holds:

```
max(slot_capacity_needed(), float.preview_slot + 1, top_live_effect_slice + 1)
```

That is a smaller reclaim than the document advertises, and it retires the
"renumbers nothing at all" argument only for the first term. The other two
terms are not renumbering hazards — nothing outside the renderer names them —
but they are *occupancy* hazards, and the document's argument conflates the
two. It should say so explicitly, because "a layer's slot never changes" is the
objection it set out to answer and that objection was never the binding one.

## 2. BLOCKING — the shrink predicate thrashes every frame on a document with effects

§5.2 proposes:

```
shrink when  needed * 2 <= capacity
```

with `needed` being `slot_capacity_needed()`. Take a document with four layers
(`next == 4`) and four enabled effects. `base` is 5; effect slices land at 5, 6,
7, 8; `bake_effects` calls `ensure_slots(9)` so capacity is 9.

`needed * 2 = 8 ≤ 9`. **The predicate fires.** The array shrinks to depth 4,
taking all four effect slices with it. `render` calls `bake_effects` on the very
next frame (`crates/umber-app/src/app.rs:3654-3668` — this is the per-frame
call, not `baked_draws`, which `render` deliberately does not use), which calls
`ensure_slots(9)` again.

So: allocate 4 slices, copy 9→4, drop 9; allocate 9, copy 4→9, drop 4. Every
frame. At 16.8 MB a slice that is ~440 MB of allocation and copy traffic per
frame at 2048². At 400 MB a slice it is a 5.2 GB transient every frame on the
canvas the whole document is about, and the machine dies.

This is not an edge case that needs contriving — it is the ordinary state of any
document with a shadow on a layer and a stack shorter than its effect count.

Whatever else survives, the predicate must be evaluated against the *occupied*
depth of finding 1, not against `slot_capacity_needed()`, and it needs an
explicit "not while a float is up" and a hysteresis argument that names
`bake_effects` as the thing that will immediately undo it.

## 3. BLOCKING — recommendation 4 (the float's spare at `end_float`) is unreachable as stated

§5.3 calls this "the safest shrink there is — the slice is above every claim by
construction" and "the one shrink with no hysteresis question in it".

It is above every *`SlotPool`* claim. It is not the top of the array. From
finding 1, with a float up and `k` effects baked the layout is:

```
0 .. next-1     layers, masks, parked slices
next            the float's preview
next+1 .. next+k  effect slices
```

The float's spare is **in the middle**. Ending the float leaves a one-slice hole
that cannot be given back without moving every effect slice down — which is
compaction, which is the thing §5.1 exists to argue is unnecessary. On a
document with no effects the recommendation works; on one with any effect at
all it reclaims nothing, and a shrink to `next` performed anyway is finding 1's
silent damage.

The claim "one use of the transform tool costs 400 MB for the rest of the
session" is still **true** and still worth fixing. But the fix is conditional
on there being no effect slice above it, and the document should say so — or
propose that the float take its spare from `EffectCache`'s free list instead,
which is a different design and a better one.

## 4. BLOCKING — `try_reserve` as specified will still panic

§6.3 prescribes:

```
CanvasRenderer::try_reserve(device, queue, needed) -> Result<(), Vram>
                        push_error_scope(OutOfMemory)
                        the same body ensure_slots has
                        pop; on Some(Error::OutOfMemory) drop the new store,
```

"The same body `ensure_slots` has" begins `let grown = LayerStore::new(device,
self.doc_size, capacity);` (`canvas.rs:3172`). `LayerStore::new`
(`canvas.rs:2088-2126`) creates the texture and then, before returning,
`1 + 2 × capacity` texture views from it: the array view, `slot_views` and
`raw_slot_views`.

When `create_texture` fails, wgpu hands back an error object. Creating a view
of an error texture produces `CreateTextureViewError::InvalidResource`, and
that classifies as **`ErrorType::Validation`**, not `OutOfMemory` — verified in
the vendored source at `wgpu-core-29.0.4/src/resource.rs:1884-1908`, where
`InvalidResource(_)` is in the arm that returns `ErrorType::Validation`.

An `ErrorFilter::OutOfMemory` scope catches only OOM
(`wgpu-29.0.4/src/backend/wgpu_core.rs:652`). So the first `create_view` after
a refused allocation reports a Validation error to `on_uncaptured_error`, which
is `crash::device_error`, which panics on purpose. The artist gets the crash box
— from the function written to prevent it, one line after the check.

The document *anticipates the mechanism* in §6.2's fourth caveat ("The returned
`Texture` from a failed create is an error object. It must be dropped, never
used") and then specifies an implementation that uses it immediately. That is
the gap.

The remedy is small and should be written into the recommendation rather than
discovered: split the texture creation out of `LayerStore::new`, create the
texture inside the scope, **pop and check before building any view**, and only
then construct the store. Pushing a Validation scope as well is the tempting
alternative and is worse — it would swallow genuine validation errors that must
stay fatal.

`generate_allocator_report`'s own `Vec` build is not on this path, so the
diagnostic half is unaffected.

## 5. BLOCKING — the upload staging accumulation is in neither document, and its OOM is fatal

The coordinator's fact 1 is confirmed at `crates/umber-app/src/app.rs:4450-4467`:
`install_import` loops `write_layer_rect` per layer with no `queue.submit` in or
after the loop. What neither document costs is what that accumulates **on the
device**.

`CanvasRenderer::write_layer_rect` (`canvas.rs:7210`) calls `write_rect`, which
is `Queue::write_texture`. In wgpu 29 that allocates a fresh `StagingBuffer` of
the whole copy size and hands it to `PendingWrites`, which is flushed at the
next submit — `wgpu-core-29.0.4/src/device/queue.rs:876-960`, and
`PendingWrites`' own doc comment at `:296-316` ("The commands accumulated here
are automatically submitted to the queue the next time the user submits a wgpu
command buffer"). `StagingBuffer::new`
(`wgpu-core-29.0.4/src/resource.rs:1120-1143`) calls the hal's `create_buffer`
directly, so `max_buffer_size`'s 256 MiB does not apply and a 400 MB write
succeeds — which is why this works today and why it accumulates.

So opening a 21-slice document at 100 Mpx is 8.4 GB of layer array **plus
8.4 GB of live staging buffers**, both alive until the next frame's submit. That
roughly doubles the peak the allocation document's §3.4 table reports, and the
table is the basis of §7.1's "a 10 GB card holds ~21 slices, allocated in one
go". With staging counted the real figure is closer to **ten or eleven** slices
for an *import*, which collapses the document's own distinction between the
import path (peak `1 + N`) and the hand-built path (peak `2c + 1`). They are
much closer together than §4.2 claims.

Worse for §6's design: `StagingBuffer::new` maps its errors through
`handle_hal_error`, **not** `handle_hal_error_with_nonfatal_oom`
(`resource.rs:1129-1132`). `handle_hal_error` calls `self.lose(...)` on
`OutOfMemory` (`wgpu-core-29.0.4/src/device/resource.rs:702-716`). So an OOM
during the upload loop **loses the device**, unrecoverably, and no error scope
can help. `try_reserve` around `ensure_slots` protects the allocation that is
*not* where the reported document fails.

Two consequences for the allocation document:

- The transient arithmetic in §6.3 ("`try_reserve` from `c` to `n` needs `c + n`
  slices") is not the peak. The peak on an import is `c + n` **plus** the
  staging for every layer written before the next submit.
- The single cheapest fix in either document is not in either document:
  `queue.submit(&[])` (or an empty encoder) inside `install_import`'s loop,
  every layer or every few layers. It costs nothing, bounds the staging at one
  or two canvases, and needs no new API, no refusal and no policy. It should be
  recommendation 1.

For the formats document this also sharpens §8.2: streaming the import fixes
the *host* side (16.8 GB → 400 MB) and does nothing at all about the staging
unless the upload loop also submits. Streaming without submitting swaps one
16.8 GB peak for another.

## 6. SUBSTANTIVE — the OOM mechanism research is exact, and that is worth saying

Every API claim in §6.2 checked out against `wgpu` 29.0.4, which is the version
in `Cargo.lock` (`wgpu = "29"` in the root `Cargo.toml`, resolved to 29.0.4).
This is unusually good work and it should not be lost in the findings above.

- `InstanceDescriptor::memory_budget_thresholds` exists, typed
  `MemoryBudgetThresholds { for_resource_creation: Option<u8>, for_device_loss:
  Option<u8> }` — `wgpu-types-29.0.4/src/instance.rs:47` and `:325-338`.
- **`with_env` really does silently discard it.**
  `wgpu-types-29.0.4/src/instance.rs:106-118` rebuilds the struct with
  `memory_budget_thresholds: MemoryBudgetThresholds::default()`, and
  `new_without_display_handle_from_env()` is `new_without_display_handle()
  .with_env()` (`:87-89`). `Gpu::create_instance`
  (`crates/umber-render/src/gpu.rs:101-103`) calls exactly that. The trap is
  real and the document is right to lead with it.
- The Vulkan check is `heap_usage + size >= heap_budget / 100 * threshold`,
  verbatim, at `wgpu-hal-29.0.4/src/vulkan/device.rs:860-863`, inside
  `error_if_would_oom_on_resource_allocation`, which is called from
  `create_texture` (`:1059`) and `create_buffer` (`:904`).
- **It returns `Ok(())` when `VK_EXT_memory_budget` is absent** —
  `vulkan/device.rs:792-799`. The document's stated doubt is correct: on such an
  adapter the mechanism is inert and silent. `wgpu-types`' own doc comment says
  "Currently only the D3D12 and (optionally) Vulkan backends support these
  options" (`instance.rs:322`). D3D12 goes through the sub-allocator
  (`wgpu-hal-29.0.4/src/dx12/suballocation.rs:559-560`), as claimed.
- `ErrorScopeGuard::pop`'s "The pop takes effect immediately; the future does not
  need to be awaited before doing work that is outside of this error scope" is
  quoted verbatim from `wgpu-29.0.4/src/api/device.rs:860-866`.

And the one thing the document could not settle — "whether the *device* carries
on cleanly after one" — I can now answer, and the answer is **yes for the
texture, no for everything around it**. `create_texture` in wgpu-core uses
`handle_hal_error_with_nonfatal_oom` (`device/resource.rs:1629`), which returns
the error *without* losing the device; so does `create_buffer` (`:1100`). But
`create_texture_view` uses the fatal `handle_hal_error` (`:1668`), and so does
the staging path of finding 5. So the device survives a refused array
allocation and does not survive a refused view or a refused upload.

Leaving `for_device_loss` unset is also right, and for a reason stronger than
the document gives: setting it makes `check_if_oom`
(`wgpu-hal-29.0.4/src/vulkan/device.rs:2678-2726`) *deliberately* lose the
device on the next poll, which is precisely the unrecoverable outcome
`try_reserve` exists to avoid.

**Verdict on the OOM mechanism: adopt it, with finding 4's ordering fix, and
with the refusal moved to cover finding 5's path as well as `ensure_slots`.**

## 7. SUBSTANTIVE — packed masks need a `(slot, channel)` pool, not a per-slice parking rule

The formats document's §5.3 says:

> `LayerStack` gains an allocator: a mask claims a `(slot, channel)` pair, and a
> slice is parked only when all three of its channels are free. The parking rule
> that keeps the undo history valid still applies, at the slice.

The last sentence is wrong and it is the corruption the parking machinery exists
to prevent. Consider layer A's mask in `(7, R)` and layer B's mask in `(7, G)`.
Delete layer B. Its mask's claim must be held so that a `PixelPatch` recorded
against B's mask is never replayed into a mask that inherits that storage
(`crates/umber-core/src/layer.rs:1425-1434`, and `CLAUDE.md`'s "Removing a mask
parks its slice too"). Under packing, slot 7 is still live — layer A holds it —
so a slice-granular parking rule parks nothing, `(7, G)` goes back on the free
list, the next mask added takes it, and B's patch replays into that mask. That
is exactly the failure `SlotClaim` was built to make unreachable.

So the unit of claiming, parking and giving back has to become the channel.
That is not "an allocator" bolted beside `LayerStack`; it is a change to
`SlotPool::take`/`give_back`/`has_headroom`, to `SlotClaim`, to
`slot_capacity_needed`, to `live_slot_ceiling`, and to `begin_float`'s
`reserved` — every one of which is currently a `u32` slice number with a
documented invariant about being one past the top. It also interacts with
finding 1: `effect_slot_base` is `slot_capacity_needed() + 1`, and if
`slot_capacity_needed` starts meaning "one past the highest *slice* any channel
claim touches" it still works, but that has to be stated rather than assumed.

The formats document's own table row "a slot identifies a texture: yes" is true
and is not the property that matters. The property that matters is "a slot
identifies a thing that can be parked", and packing breaks that.

This does not sink the proposal. It does mean the "arrays: 1, six paths
unchanged" column of the §5.3 table is understating the change by the single
most invasive piece — which is the same criticism §5.1 correctly levels at the
*dedicated array* alternative. The two options are closer in cost than the table
shows.

## 8. SUBSTANTIVE — `clear_layer` and `fill_layer_white` clobber whole slices

§5.3 names `write_layer_rect` as "the one structural change of any size". It is
not the only one. Two more methods write a whole slice and are directly on the
mask feature's own path:

- `CanvasRenderer::fill_layer_white` (`canvas.rs:4243-4259`) — a render pass
  with `LoadOp::Clear(Color::WHITE)` over the whole slice. **This is what adding
  a mask calls.** Under packing, adding a second mask to a slice would wipe the
  first one to white, silently revealing everything a painter had hidden.
- `CanvasRenderer::clear_layer` (`canvas.rs:4225-4230`) — same shape, and
  reached by `App::add_layer` on a recycled slot.

Both need the same per-channel treatment `write_layer_rect` does, and a clear
cannot express one: `LoadOp::Clear` writes every channel of the attachment.
They become draws with a write mask, which is finding 10.

`fill_layer_white`'s doc comment is also worth reading before this lands: it
argues carefully that 1.0 encodes to 255 "in every channel", and the composite
"reads the red one". Under packing that comment becomes false for two of the
three masks in a slice, and the reasoning it records — that a mask arriving at
`0xfe` would dim its layer by a level nobody asked for — is exactly the class of
error a hand-written per-channel clear could reintroduce.

## 9. SUBSTANTIVE — `slot_revision` is per slice, so packed masks cross-invalidate

`CanvasRenderer::slot_revision` and `touch_slot` (`canvas.rs:5428-5447`) count
writes **per slice index**. Two consumers key off it:

- `Thumbs` — the layer list's whole invalidation rule.
- `CachedEffect::mask_revision` (`canvas.rs:6080`), which is what decides
  whether a layer's effects are rebaked.

With three masks in one slice, a single dab on mask A moves the revision of
masks B and C. Every effect on the layers owning B and C is then stale and
rebakes. `measure-effects` puts a bake at 4–34 ms at 100 Mpx by the allocation
document's own §9.3 citation — so a stroke on one mask could rebake several
unrelated shadows on every frame of that stroke. Their thumbnails also
re-render.

Neither is a correctness failure, and both are fixable (a per-channel revision
vector, or keying the effect stamp on `(slot, channel)`), but it is a fourth
path the §5.3 table lists as "unchanged" and it lands on the drawing loop.

## 10. SUBSTANTIVE — `ColorWrites` is pipeline state, not dynamic state

§5.3 says a pass with `ColorWrites::RED` "is free at run time". At run time,
yes. But `write_mask` lives in `wgpu::ColorTargetState`, which is baked into the
render pipeline at creation — there is no `set_write_mask` on a render pass.

So every pass that writes a mask needs three pipeline variants (or four, if
alpha is ever used):

- the new `write_layer_rect` pass §5.3 proposes,
- `fill_layer_white`'s replacement (finding 8),
- `clear_layer`'s replacement (finding 8),
- **the commit**, because `StrokeStyle::on_mask` means an ordinary stroke
  commits into a mask slice, and `commit.wgsl` writes four channels.

That last one is the important one and the document does not mention it at all.
`CLAUDE.md` records that a stroke on a mask "needs **no new pipeline**: a mask
is an ordinary slice". Packing retires that sentence. It is not fatal — the
shader is unchanged, only the target state — but "the one structural change of
any size" becomes four call sites and a pipeline permutation dimension in
`Shared`, which is the thing `Shared` is built once per process to avoid
multiplying.

## 11. SUBSTANTIVE — §9.3 and §9.4 are both called correctly; one number needs a caveat

**§9.4, declining to swap a background tab's layer array — agree, and the
arithmetic is if anything conservative.** `switch_document`
(`crates/umber-app/src/app.rs:4031-4040`) does call `finish_transform()` and
`finish_stroke()` first, as claimed, so the premise holds. The 1.6 s of PCIe
plus 2.5 s each way of memcpy is the right order; and the readback would also
have to go through `read_layer_rect`'s **blocking** `poll(wait_indefinitely)`
(`canvas.rs:7085`), on the frame somebody clicked a tab. Five seconds is if
anything optimistic. The conclusion — that the honest answer is tiling — is
right.

**§9.3, releasing a background tab's working set — agree, with one correction.**
The 2.2 GB figure is the scratch, the colour scratch and the effect working set,
and those three are genuinely reclaimable with no readback and no pixel cost.
But the section then says the effect *slices* are "the interesting part of
that: they are the only reclaim that touches the layer array". `forget_all`
(`canvas.rs:7583-7587`) returns effect slices to `EffectCache`'s free list — it
does **not** shrink `LayerStore`, and `ensure_slots` never shrinks. So on a
background tab those slices' memory is not given back at all until finding 1's
shrink exists and is correct. The 2.2 GB stands; the effect slices are extra and
are blocked behind the thing that is currently wrong.

## 12. SUBSTANTIVE — the load-bearing figures, and which would change a recommendation

Both documents are, as they admit, arithmetic over format constants. Neither has
an allocation dump. Here is what I could verify from constants, and what remains
a guess.

**Verified from the source, safe to rely on:**

| Figure | Where |
|---|---|
| `MAX_TOTAL_BYTES` = 16 GiB | `crates/umber-core/src/docimport/mod.rs:619` |
| `Document::MAX_EDGE` = 32768, and `ImportedDocument::MAX_DIMENSION` **is** it | `document.rs:171`, `docimport/mod.rs:590` |
| `MAX_SLOTS` = 256, `MAX_EFFECT_SLICES` = 127, `MAX_DRAWS` = `MAX_LAYERS + MAX_EFFECT_SLICES` | `canvas.rs:88`, `:161`, `:181`, asserted at `:8286-8288` |
| `GROWTH_DOUBLING_BUDGET_BYTES` = 256 MiB, and the degeneration to `initial_slots == growth_quantum == 1` at 100 Mpx | `canvas.rs:536`, and the pinned `growth_quantum(slice_of(8192)) == 1` at `:8517` |
| 42 layers admitted at 400 MB a slice | 16 GiB ÷ 400 MB = 42.9 |
| `read_texture_rows` hard-codes `width * 4` | `canvas.rs:7119`, and `read_layer_pieces` again at `:7088` |
| `write_layer_rect` asserts `rect.area() * 4` | `canvas.rs:7188` |
| `DocumentCapture` is `Vec<Vec<u8>>` plus `merged`; `Capture::results` accumulates | `canvas.rs:1152-1157`, `:1058` |
| `encode_coverage` writes `[g, g, g, 255]` | `crates/umber-core/src/docimport/srgb.rs:132-135` |

**Load-bearing and unmeasured. Marked with what changes if it is wrong by 2×:**

1. **Whether a 400 MB texture costs 400 MB.** Named by the allocation document
   as unsettled and it is the multiplier under every row of §3 and every row of
   the formats document's §2 table. If it is 1.3× — metadata surfaces, DCC
   planes, alignment — then "25 slices on a 10 GB card" is 19, and both
   documents' *conclusions* survive because they are all "far above what a card
   can hold". **Nothing changes at 2×.** This is the figure most often demanded
   and least consequential here.
2. **The `2c + 1` transient.** Named as "the load-bearing claim of this whole
   document". I confirmed the *code* — `ensure_slots` builds `grown`, copies,
   submits, and only then assigns (`canvas.rs:3172-3212`) — so the claim is
   structurally true. What is unmeasured is whether the driver has actually
   released the old texture by the time the next growth asks; wgpu keeps it
   alive for the submission naming it, so a rapid sequence of growths may hold
   *three* arrays, not two. If so the hand-built ceiling is nearer seven layers
   than eleven and the case for `for_document` taking a slot count gets
   stronger, not weaker.
3. **What egui holds.** Named as unsettled, correctly, and it is the one figure
   that could change a *recommendation*: if the font atlas plus brush previews
   plus layer thumbnails come to 500 MB rather than 50 MB, then §9.3's per-tab
   reclaim is competing with a per-process cost nobody has looked at, and the
   thumbnail cache's own policy becomes worth a section. `generate_allocator_
   report`'s labels answer it directly.
4. **The staging accumulation of finding 5.** Not named anywhere and it is the
   largest single unmeasured multiplier on the exact path the user reported.
   Wrong by 2× in either direction and the "eleven versus twenty-one slices"
   framing of §7.1 changes shape.
5. **Formats §3's colour-scratch question** — whether `Rgba8UnormSrgb` would do
   for the per-dab colour scratch. Worth 400 MB per smudging stroke. The
   document is right to refuse to answer it by reading, right that the
   build-up accumulation study is the instrument, and right not to recommend the
   change. I would add one thing it does not: the colour scratch is *also* read
   by `probe_canvas`'s smudge pickup, so a narrower scratch changes what a
   blender picks up as well as what it lays down, and the measurement should
   cover a scrubbed-back-and-forth stroke and not only a single pass.
6. **Formats §10.1's 10 GB autosave peak.** This one I would trust: it is
   `results.len() × canvas × 4` and I verified both the type and the
   accumulation. It is the largest honest number in either document and it has
   no quality trade in it whatever.

## 13. MINOR — the formats document contradicts itself on the mask readback saving

§5.3's table says packed RGB is **1.33** bytes per mask pixel, correctly: three
masks in a four-byte slice. §10.1's fix (3) then says "§5.3's packing removes
three quarters of the readback as well as three quarters of the slice". Three
quarters is the four-masks-in-RGBA figure the document explicitly refuses two
pages earlier. The saving is two thirds. In a document that is entirely
arithmetic this matters more than it would elsewhere.

## 14. MINOR — `add_canvas` re-clears what `ensure_slots` has already cleared

`Graphics::add_canvas` (`crates/umber-app/src/app.rs:182-196`) calls
`ensure_slots(slots)` — which clears every slice above the old capacity
(`canvas.rs:3198-3200`) — and then `clear_all_layers`, which clears every slice
again (`canvas.rs:4268`). On a 21-slice import that is 21 redundant render
passes in the frame the document opens.

The allocation document's §8.1 lists both call sites as correct, which they
individually are; it does not notice that together they are duplicated. §8.2 is
honest that a clear is probably a fast-clear and that this is unmeasured, so the
cost may well be nothing. It is free to fix and belongs beside §4.2's other two
"both are small" items.

## 15. MINOR — ordering: the two documents do not conflict, but one item should be dropped

The formats document says its mask work must not be built before the tiling
design, and gives a good reason (masks are the 20%; a tile allocator can carry a
width per tile class and none of the six paths ever learn about it). Findings
7–10 strengthen that: packing touches more than the document counts, and every
one of those touches is a path tiling is going to rewrite anyway. **Its ordering
recommendation is right and should be followed.**

The allocation document's `resize` change (§5.4) and the tab-parking change
(§9.3) do not collide with it. `resize` is a signature change through one call
site that `CanvasRenderer::resize`'s own doc comment already prescribes
(`canvas.rs:3245-3270`, and I confirmed the comment says exactly what the
document quotes, including the `grown_capacity(0, live, slice_bytes(new))`
form). It is the best benefit-to-risk item in either document and nothing in
either sibling design touches it. Note one thing the document gets right and is
worth repeating: because a resize allocates a whole new array anyway, this is
the one shrink with **no** transient at all — and it is also the one shrink that
is safe from finding 1, because a resize calls `EffectCache::forget_all` and
ends any float first.

What should change is not the *order* of the allocation document's item 3 but
its *presence*. A general shrink policy is not a deferred good idea; as
specified it is a picture-damaging change, and the version that is safe
(finding 1's three-term maximum, plus a float guard, plus hysteresis against
`bake_effects`) is a different and much smaller proposal that reclaims much
less. I would replace item 3 with `resize` (currently item 5) and let the
general shrink be retired by tiling, which §10 already says it would be.

## 16. Claims I checked and found correct

Recorded so nobody re-derives them.

- `grown_capacity`'s policy, the `initial_slots`/`growth_quantum` table in §4.1,
  and that the shipped code does not contain the `next_power_of_two` mutation
  `CLAUDE.md` warns of. `canvas.rs:496-650`.
- `ensure_slots` holds both arrays alive across the copy — `self.layers = grown`
  at `:3212`, after `queue.submit` at `:3201`.
- `add_canvas` builds at `initial_slots` and immediately grows, so an import
  peaks at `1 + N`. `app.rs:176-182`.
- The four growth callers in §4.3's table, all four correct.
- The DX12 16384 point (§6.4). `MAX_EDGE` is 32768 and `install_import` refuses
  against the device's own `max_texture_dimension_2d` at `app.rs:4402-4413`,
  with wording that already names the real figure. Checking the backend before
  concluding anything about memory is good advice.
- §8.1's four clearing sites, all four correct.
- The commit backdrop really is bounded at `canvas width × 64`, and
  `band_rows`'s 3,355-row band at 20000 wide is right.
- Formats §4: `Rgba8UnormSrgb` is the only useful member of the renderable,
  blendable, 4-channel, `Features::empty()` set. `Rgb10a2Unorm` really does give
  alpha two bits.
- **Formats §5.2's load-bearing fact is TRUE.** I enumerated every `*Srgb`
  variant of `TextureFormat` in `wgpu-types-29.0.4/src/texture/format.rs`: the
  only uncompressed ones are `Rgba8UnormSrgb` (`:184`) and `Bgra8UnormSrgb`
  (`:194`); everything else sRGB is BC, ETC2 or ASTC. There is no
  `R8UnormSrgb` and no `Rg8UnormSrgb`. The author flagged this as the fact it
  was confident about but could not confirm — it is confirmed, and the quality
  argument built on it stands.
- **Formats §6's BC7 refusal is correct on both of its mechanical grounds.**
  `Bc7RgbaUnorm` requires `Features::TEXTURE_COMPRESSION_BC` (`format.rs:887`)
  and its guaranteed usage is `(none, basic)` (`format.rs:1025`) — no
  `RENDER_ATTACHMENT`, no storage. Against `required_features: Features::empty()`
  it is unavailable, and even with the feature it could not be a layer slice.
  The §6.1 argument that a zoomed-out proxy is dominated by eviction is also
  sound and I would not reopen it: the set of layers a proxy could compress is
  the set eviction pages out entirely, and eviction costs no colour error. A
  clean refusal, correctly reasoned, and it protects the artist's constraint.
- Formats §9: undo is not the problem at this scale; the existing
  `measure-history.rs` figure should be cited rather than re-measured. Agreed on
  both. The copy-on-write-tiles observation is the most valuable single
  paragraph in that document and it is correctly handed over rather than
  designed.
- Formats §8.3's alignment argument for a 256-pixel tile — `csblocks::BLOCK` is
  256 and `damage::TILE` is 64 — is real and, as the author says, not decisive.
  It is worth putting on the tiling design's scales.
- Nothing in either document breaks `saving_and_reopening_does_not_move_a_pixel`
  or the exact-inverse `srgb` pair. Packing does not touch either: a packed mask
  is still sRGB-typed storage read through the hardware decode, and
  `docformat` already writes masks as greyscale PNG through a channel
  extraction. The undo budget's *meaning* is unchanged by everything except
  finding 7's channel-granular parking, which would need `StackShape::byte_len`
  to charge a third of a slice for a parked mask channel — a small thing, but
  one nobody has written down.

---

## What I would most want changed

**Withdraw the shrink recommendation, and replace it with the missing submit.**

Item 3 is the allocation document's headline and the thing a reader will
implement first because it is presented as costless. As specified it deletes
baked effect slices, breaks the float's preview, and — with the §5.2 predicate —
reallocates the entire layer array twice a frame on any document carrying a
shadow. The argument it was built to defeat ("a layer's slot never changes") was
never the binding objection; the binding one is that `slot_capacity_needed()`
does not describe what the array holds, which `EffectCache`'s own module comment
says in the same file.

In its place, the cheapest fix in either document is one line neither of them
contains: `install_import` must submit inside its upload loop. Today it holds a
canvas-sized staging buffer per layer until the next frame, roughly doubling
peak GPU memory on the exact operation the user reported failing — and it does
so on a path where wgpu loses the device on OOM, so no refusal, no error scope
and no budget threshold can rescue it. It costs nothing, it needs no new API,
and it is the difference between opening a twenty-layer document and not.

Second, if only one more thing can be taken: **finding 4's ordering fix inside
`try_reserve`**. The OOM mechanism is otherwise correct in every detail I could
check, and it would panic on its first real use.
