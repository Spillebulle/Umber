# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo run --release            # run the app
cargo run                      # dev build; deps are still optimised (see below)
cargo test                     # everything
cargo test -p umber-core       # engine only, pure CPU, instant
cargo test -p umber-render --test gpu_pipeline   # headless GPU tests
cargo test dabs_are_evenly     # a single test by name substring
cargo clippy --workspace --all-targets
cargo fmt --all
```

CI runs `fmt --check`, `clippy` and `test` on Linux, Windows and macOS with
`RUSTFLAGS: -D warnings`. Clippy warnings fail the build, so run clippy before
declaring work finished.

`RUST_LOG` controls logging, e.g. `RUST_LOG=umber_app=debug,wgpu_core=info`.
`WGPU_BACKEND=vulkan|dx12|metal` forces a backend when chasing driver bugs.

`[profile.dev]` builds *dependencies* at `opt-level = 3` while keeping our own
crates cheap to rebuild. Do not "simplify" this away — an unoptimised wgpu makes
the canvas too slow to evaluate by hand.

## Architecture

Four crates, layered so each can be tested in isolation:

| Crate | Contains | Must not depend on |
|---|---|---|
| `umber-core` | document, brush, dab generation, camera, layers, undo, file formats | wgpu, winit, egui |
| `umber-render` | textures, pipelines, WGSL shaders | winit, egui |
| `umber-app` | event loop, input translation, egui panel | — |
| `umber-desktop` | binary entry point | — |

Keeping `umber-core` free of GPU types is what makes the brush engine testable
without a device; keeping `umber-render` free of windowing types is what makes
the headless GPU tests possible. Preserve both boundaries.

### The stroke pipeline — read this before touching rendering

A stroke is a dense row of overlapping stamps ("dabs"). Compositing dabs
directly onto the layer is **wrong**: overlaps compound, so strokes come out
blotchy, more opaque than requested, and darker where a stroke crosses itself.

Umber uses a wet-layer scheme instead:

1. **Dab pass** (`dab.wgsl`) — new dabs stamp into an `R8Unorm` scratch texture
   with a **`max` blend**, so coverage saturates at 1.0. All dabs in a frame are
   instances of one 4-vertex quad: N dabs, one draw call.
2. **Composite pass** (`composite.wgsl`) — layer + scratch drawn to the surface
   under the camera transform, one fullscreen triangle.
3. **Commit** (`commit.wgsl`) — at pointer-up the scratch bakes into the layer
   once, over the stroke's damaged rect only, then the scratch is cleared.

Invariants that are easy to break:

- **`Brush::opacity` must never be folded into per-dab coverage.** Stroke
  opacity is applied exactly once at commit. Folding it in reintroduces the
  compounding bug.
- **`composite.wgsl` and `commit.wgsl` must implement identical blending
  maths.** If they diverge the stroke visibly jumps at pointer-up, when the
  preview is replaced by the committed result.
- **Paint and erase need different blend state, not just different shader
  output.** Erase uses `src_factor: Zero` so alpha is scaled down
  (`a = dst.a * (1 - cov)`). With `One` it *adds* opacity — an eraser that
  paints. This was a real bug; `erasing_removes_coverage` guards it.
- **The dab pass loads rather than clears.** The scratch accumulates across
  frames for the whole stroke; only new dabs are drawn each frame.
- **The scratch stays `R8Unorm`, and that is a measured decision, not an
  oversight.** It is exactly as wide as the layer alpha it commits into, so it
  adds no loss of its own and widening it cannot carry the pen's 1024 pressure
  levels to the canvas. `R16Unorm` is not even a legal render target on the
  feature set Umber requests. The measurements are under "Pressure" in Platform
  support — read them before proposing this again.
- **`Brush::build_up` is the one exception to the `max`, and it is a *blend
  state*, not a shader branch.** A sparse texture stamp's mark is the overlap of
  many faint stamps — GIMP and Krita composite every dab — so a `max` caps a
  stroke at the mask's own brightest texel and paints half the author's mark.
  Build-up swaps the coverage target for `a = cov + a(1 − cov)`. The dab shader
  is byte for byte identical either way, and nothing downstream sees it: the
  scratch still holds coverage in 0..1, so composite and commit are untouched
  and opacity is still applied exactly once. The `max` path is the default and
  stays exactly as it was. Build-up is only meaningful where a dab is not solid
  — a bitmap tip, grain, or a pressure-opacity ramp — which is why no MyPaint
  brush uses it.
- **There are four dab pipelines**, from two independent binary choices
  (per-dab colour, build-up), built by one loop over one descriptor rather than
  four copies of it. `DabStyle` carries both and must be the same for every
  frame of a stroke.
- **Grain multiplies coverage and is anchored to the *document*.**
  `mix(1.0, tile, strength)`, so a strength of zero is the exact identity — a
  brush with no paper pays one multiply by one and no branch. Document-anchored
  is the whole effect: a second stroke lands in the same pits as the first. It
  needs its **own repeating sampler** — a paper tile covers the document and has
  to wrap, where a tip stretched over its dab must not.
- **A smudging stroke writes a *second* scratch target, holding a colour per
  dab.** Coverage keeps its blend — the anti-compounding guarantee above
  is untouched — while colour blends `over`, so the smear trails along the
  stroke instead of averaging everything the brush picked up. The coloured
  pipelines write two attachments; the ordinary ones write one and are the fast
  path. `StrokeStyle::per_dab_color` must match what `draw_dabs` was told, for
  the whole stroke: turning it on midway leaves the earlier dabs with no colour
  recorded, and they commit as the flat palette colour while the rest smudge.
- **`finish_stroke` must flush `StrokeBuilder::pending` before committing.**
  Pointer events outpace frames, so a stroke always ends with dabs that have not
  reached the GPU. Leaving them behind strands coverage in the scratch: it
  redraws as a live preview (the stroke "hangs") and is then baked in by the
  *next* stroke's commit, wearing that stroke's colour. This was a real bug;
  `ending_a_stroke_keeps_its_tail_pending` guards the core half of it.

### Selections

`umber-core::selection`. A selection is **an outline plus a coverage mask over
the outline's own bounding rectangle**, and it is both halves deliberately —
the module docs have the argument. Short version: a byte per document pixel is
100 MB on a canvas Umber supports and nearly all of it zero; a path alone is
the wrong thing to hand a fragment shader and has no answer for a partly
covered pixel. So the mask is what gets used and the rings are what get drawn.

- **The clip is applied in the dab pass and nowhere else.** Coverage is
  multiplied by the mask on its way into the scratch, so `composite.wgsl` and
  `commit.wgsl` never learn there is a selection — which is the only way to
  guarantee the two of them cannot clip differently and make the stroke jump at
  pointer-up. It also means stroke opacity is still applied exactly once, the
  `max` still saturates, and an eraser is clipped by construction rather than by
  a second piece of code.
- **A placeholder texture cannot stand in for "no selection"**, unlike the tip's
  and the paper's. A 1x1 mask read outside its own rectangle is *zero*, which
  would mean nothing may be painted anywhere. Hence `use_selection`, read
  through a `select` rather than a branch, and with it clear the shader
  multiplies by exactly 1.0 — `no_selection_is_the_exact_identity` pins it.
- **Outside the mask's rectangle is decided arithmetically, not by clamping.**
  Clamp-to-edge would smear the boundary texels across the canvas and leave the
  whole row and column beyond a rectangle selection paintable.
  `nothing_outside_a_selections_own_rectangle_is_paintable` guards it.
- **Bound from `start_stroke`, like the tip and the paper**, and compared by
  `Arc` identity. That is also what re-binds it after a tab switch or an Android
  resume, where the renderer is a different object.
- **Fill is nonzero winding and the edge is antialiased.** Even-odd would punch
  a hole in the middle of a loop somebody drew freehand. Coverage is four
  sub-scanlines per row with exact horizontal span coverage, which makes an
  axis-aligned rectangle exact on both axes.
- **A resize drops it**, for the reason a resize clears the undo history: the
  bounds are a rectangle of a canvas that no longer exists. Both halves —
  `Editor::apply_canvas` and `CanvasRenderer::resize` — do it, so neither can be
  the one that forgot.
- **The selection is per-document and the draft is not.** `Selection` lives in
  `DocumentState`; `SelectionDraft` is the gesture, and belongs to the pointer,
  so a tab switch abandons it.
- **The outline is a dashed line, not marching ants.** Animating it means
  requesting a frame for ever, which is the cost `render`'s `repaint_at` exists
  to avoid.

### Transforms and the clipboard

`umber-core::transform` is the model, `transform.wgsl` and `CanvasRenderer`'s
float methods are the pixels, and `umber-core::clipboard` is what copy and paste
put on and take off.

- **The preview and the commit are the same two commands, not two renderings.**
  `Float::base` is the layer as the float will sit on it — the original pixels
  with the lifted region taken out. `render_float` restores the damaged
  rectangle out of it and draws the transformed copy over it, into a **spare
  slice of the layer array**; the commit is that function again with the layer's
  own slice as the target. So unlike the stroke there is no second copy of the
  blend maths to keep in step, and there is nothing to get wrong: what the
  screen showed during the drag is byte for byte what gets written.
- **`composite.wgsl` was not touched, and must not be.** `Editor::layer_draws`
  swaps the active layer's slot for the preview slice, which already holds what
  the layer will hold — so the float composites at the right stack position,
  under the right blend mode, at the right opacity, with none of that restated.
  A uniform and a branch in the composite would be the divergence the selection
  clip and this both exist to avoid.
- **The layer is untouched until the commit.** That is what makes Escape free,
  and it is what makes the undo patch — captured at commit, like a stroke's —
  the pre-transform pixels.
- **The patch spans source ∪ destination.** `Transform::damage`. One covering
  only where the pixels went would undo to a document that still had the hole
  in it. A paste has no source, so its damage is the destination alone.
- **The destination is the bounding box of the *quad*, plus a pixel.** Exactly
  `StrokeBuilder::bounds`'s rule and the same failure if it is too tight: a
  turned rectangle reaches `half × sqrt(2)`, and an edge left uncommitted
  redraws as a preview and is baked in by the next edit.
- **Filtering is the hardware sampler's, and there is deliberately no CPU
  resampler beside it.** The layer array is already `Linear`, so bilinear is
  free; a second implementation called by nothing is the drift refused
  everywhere else. What `umber-core` owns is the *map*, and
  `a_transform_and_its_inverse_are_exact_opposites` pins it.
- **A float exists only with the transform tool in hand, on the layer it came
  from.** Checked once per frame in `render` rather than at the rail, the
  shortcuts, the layer list and the preset — an invariant enforced at five call
  sites is one that will be forgotten at the sixth. Every path that leaves the
  document (tab switch, save, export, resize, close) commits it first, which is
  what lets it live *above* the `--- documents ---` line like the stroke.
- **A float counts as busy for the autosave**, pointer up or not. Its pixels are
  in no layer, so a document written mid-transform would disagree with the
  screen.
- **The mask passes need their own bind group.** A colour target is an exclusive
  usage, so the floating copy cannot be bound for sampling in the pass that
  renders into it. `fs_mask` never reads that slot, so the mask stands in.
- **`begin_float` submits twice.** `Queue::write_texture` is flushed *before*
  the command buffers of the submission it precedes, so a paste cleared in the
  same encoder is wiped. Once per gesture, where `start_stroke` already submits.
- **The clipboard holds straight-alpha sRGB**, not layer bytes, and both
  directions go through `docimport::srgb`'s exact-inverse pair — so a copy and a
  paste straight back restore the bytes they started with, and the day a system
  clipboard arrives the form it wants is already what is held. Masking scales
  **alpha on the straight side**; scaling the stored bytes is wrong by a gamma
  curve and invisible on anything opaque.
- **`Clip::place` decides what a paste does**, in `umber-core`, because "where
  does the picture go" is a rule and rules are testable without a window:
  centred on the selection or on the view, nudged back on where it fits,
  **centred and cropped** where the clip is larger than the canvas — and the
  crop is said out loud, because a floating region lives in canvas-sized storage
  and there is nowhere to hold the overhang.
- **The marquee travels with the picture**, at commit, by transforming the rings
  and rasterising again. Nothing in `selection.rs` was changed for it. Only for
  a lift: a paste did not come out of the selection.

### Layers

Layers occupy slices ("slots") of one texture array, and the whole stack
composites in a **single pass** — `composite.wgsl` loops bottom to top. Do not
"simplify" this into a pass per layer.

- **A layer's slot never changes.** Stack order is the `Vec` order, so
  reordering is a pointer shuffle, not a texture copy. Anything indexing layers
  by position must not assume position equals slot.
- **`LayerStack::MAX`, `MAX_LAYERS` in `canvas.rs`, and `MAX_LAYERS` in
  `composite.wgsl` must agree.** The last one sizes a uniform array.
- **Deleting a layer clears undo history.** Slots are recycled, so a patch
  recorded against a freed slot would be replayed into whichever layer inherits
  it. Structural undo is the real fix and is not built yet.
- **A recycled slot still holds the old layer's pixels** — clear it on the GPU
  when a new layer takes it.
- **The in-progress stroke blends inside the stack**, at the active layer's
  position, not over the finished composite. Otherwise painting beneath a
  Multiply layer previews wrongly and jumps on release.

### Documents

Several documents are open at once. `Session` (`umber-app/src/session.rs`) holds
the per-document state — document, layers, history, camera — and switching tabs
moves that block in and out of `Editor` wholesale.

`Document` is a canvas size, a `Background` and a resolution.

- **The background composites *under* the stack, inside the same pass**, as one
  `acc + bg * (1 - acc.a)` after the layer loop and **before** the export
  branch. Both sides are premultiplied, so it is an add; transparent is all
  zeroes and therefore the exact identity, and a document without one pays that
  add and no branch. Being before the export branch is what makes both halves
  fall out of one line: a white-backed document exports opaque, a transparent
  one keeps its alpha. `export_rgba`, `pick_colour` and `probe_canvas` reuse
  that pass, so the flat PNG, the eyedropper and a smudging brush's canvas
  probe are all right automatically — **do not add a second path**.
- **It lives on `CanvasRenderer`, not in `CompositeParams`.** It belongs to the
  document and a renderer already is one document's; as a per-frame parameter
  it would have to be threaded into the three internal callers that build their
  own params, which is three more places for an export to stop matching the
  screen. `Graphics::add_canvas` must therefore call `set_background` — a
  renderer cloned from another document's does not inherit it.
- **A background is *only* ever transparent or opaque.** A partly transparent
  one would have to answer what an export means and buys nothing a bottom layer
  at reduced opacity does not.
- **Resizing a document clears its undo history**, for the same reason deleting
  a layer does: a `PixelPatch` is a rectangle of the *old* canvas, so replaying
  one would paste the right bytes into the wrong pixels or name a rectangle off
  the edge. `Editor::apply_canvas` does it, so neither call site can forget.
  `CanvasRenderer::resize` also needs **no stroke in flight** — it throws the
  scratch away rather than resampling it.
- **Where the pixels land is `CanvasCopy::plan`'s, in `umber-core`.** That is
  what keeps the dialog's preview and what the GPU does from drifting, and it
  is testable without a device. The layer array is copied whole, every slice in
  one transfer: the anchor moves the *picture*, not one layer relative to
  another.
- **`resize` must rewrite `dab_state.doc_size`.** The dab pass turns document
  pixels into clip space with that number, so leaving it behind puts every later
  dab at the wrong place and the wrong scale, on a layer that still looks
  plausible.

- **Nothing above the `--- documents ---` line in `editor.rs` is per-document.**
  That is what keeps a tab switch to four moves instead of an audit of every
  field. Adding per-document state means adding it to `DocumentState` too, or it
  will leak between tabs.
- **Each document owns a `CanvasRenderer`**, because each owns a layer texture
  array. `Graphics::add_canvas` clones the pipeline handles out of the first
  renderer — do not let it recompile the shaders per document. Closing a tab
  must drop the renderer, or the textures are never given back.
- **`resumed` rebuilds storage for every open document, not just the active
  one.** That path is Android's: the surface dies on suspend and the session
  survives it. Pixels do not survive, and never have; a document with no
  renderer would be a blank window with no way out.

### The document format

**Umber's format is OpenRaster.** `umber-core::docformat` writes `.ora` and
`docimport::openraster` reads it — the same reader that opens Krita's and
MyPaint's files. `docs/document-format.md` has the whole argument.

- **There must never be a second ORA reader.** The point of choosing a format
  Umber already read is that saving and opening share one decoder. A "native"
  path that parsed `stack.xml` again would be the exact drift this avoids.
- **`docimport::srgb`'s two directions must stay exact inverses.** Layer
  textures hold premultiplied linear colour; ORA holds straight alpha. If
  `encode_pixel` and `decode_pixel` stop agreeing, a document moves a level
  every time it is saved and reopened.
  `saving_and_reopening_does_not_move_a_pixel` drives every reachable
  (colour, alpha) pair through both.
- **The `umber-` attributes are the extension mechanism**, and every other ORA
  reader ignores them, which is what keeps the file a plain `.ora`.
  `umber-blend` exists because Add's nearest SVG name (`svg:plus`) is only
  approximate — without it, reopening Umber's own file reports a loss that did
  not happen. `umber-version` is bumped only when a revision stores something an
  older build would drop silently, and an older build then **refuses** the file
  rather than opening it with pieces missing.
- **The undo history is written too**, under `umber/`, pointed at by
  `umber-history`. `docformat::history` has the argument; the rules it lives by:
  - **A slot is never written down.** `PixelPatch::slot` is a texture slice and
    slots are recycled, which is why deleting a layer clears the history — a
    slot in a file read into another session's allocation is that bug made
    permanent. Entries name a **stack position**; `SaveHistory::new` maps slot
    to position at save time and refuses the whole history if any patch cannot
    be placed, and `ImportedDocument::open` maps it back — which is why that
    returns an `Opened` rather than a tuple, because the stack and the history
    have to be built together.
  - **Anything that does not line up exactly is dropped, whole.** The manifest
    fingerprints the canvas and the layer names, compared against the layers
    that actually *loaded* — a skipped layer shifts every position after it. The
    entries are a sequence in which each restores the pixels the next expects,
    so one missing from the middle is not a shorter history but a wrong one.
    A history replayed into the wrong layer is far worse than no saved history.
  - **The file has its own budget**, `BUDGET_BYTES` of *encoded* patches against
    512 MB raw in memory, oldest dropped first, encoded newest-first and stopped
    at the limit so a session far over pays for nothing it will not keep.
    Patches are PNG at `Compression::Fast`: measured, that beats the ZIP's own
    Deflate on size everywhere but a sketch and on time by 10×. Re-measure with
    `examples/measure-history.rs` before changing any of it.
  - **This did not bump `umber-version`**, and the argument is in
    `docformat`'s module docs. An older build ignores an entry it has never
    heard of and opens with an empty history — exactly what every build before
    this did. `history::VERSION` governs the manifest, and an unreadable one is
    *discarded* rather than refused.
  - **It is a preference, `ui.save_history`.** A full-canvas session saturates
    the budget and takes a 9.7 MB document to 41.5 MB; that is a trade the user
    has to be able to refuse. Nothing here touches the GPU — the patches have
    been in memory since commit time.
- **`mergedimage.png` is the caller's, not `docformat`'s.** Flattening means
  blend modes, and the blend maths lives in the composite shader. A software
  copy here would be a second implementation to keep in step, and a file whose
  preview disagreed with the screen is the bug that produces.
- **A save writes to a temporary neighbour and renames.** A write that dies
  halfway must not replace the artist's last good file with a truncated archive.
- **`read_layer_rect` blocks and a save calls it once per layer.** That is
  acceptable on an explicit Save and nowhere else; it must not migrate towards
  the drawing loop. The autosave does *not* use it — see "Autosave".
- **Save must close a tab only when a file was actually written.** A cancelled
  file dialog is not permission to discard a document.
- **`docformat::write_encoded` is the one atomic write.** `save` is `encode`
  plus that; the autosave needs the halves apart because it puts one archive in
  two places. A second temp-and-rename would be a second thing to get right.

### Autosave

`umber-app/src/autosave.rs`. The same file, on a timer, by a route that never
stalls a frame — because the blocking readback a Save uses is exactly wrong
when nobody asked for it.

- **Nothing starts unless the pointer is up and no stroke is live.** That is
  the whole of how an autosave cannot drop a stroke, and it makes "every five
  minutes" mean "at the first quiet moment after five minutes" — which is what
  a painter would choose anyway. `Autosave::next_due` is where it is decided
  and it takes the `Session` rather than a list built by the caller, because it
  runs **every frame** and the drawing path allocates nothing.
- **The pixels come through `CanvasRenderer::begin_capture`, never
  `read_layer_rect`.** One layer in flight at a time, one reused staging
  buffer, four megabytes read out per frame. All three bounds were measured and
  each is load-bearing: recording every copy at once cost 27 ms in one frame,
  and reading a whole 16 MB layer in one go cost 5 ms. As it stands the worst
  frame is about a millisecond. `a_capture_of_a_large_document_never_costs_a_
  frame` pins it.
- **Every readback goes in bands, because a document can be larger than the
  largest buffer the device will make.** `downlevel_defaults` caps
  `max_buffer_size` at 256 MB and `using_resolution` raises only the texture
  dimensions, so a 10000² canvas is paintable and was then not readable:
  `create_buffer` refuses 400 MB, and a validation error aborts the process.
  Raising the limit is the wrong fix twice — it breaks the rule that a desktop
  build may not depend on what a mobile GPU refuses, and 256 MB is a limit real
  hardware has. `band_rows` decides the band and returns the whole document
  whenever it fits, so nothing changes for an ordinary canvas. The blocking
  readbacks share `read_texture_rows`; the capture bands *across frames*, which
  is why `Capture` carries a row cursor and why the flattened preview is
  composited once per step rather than once per band. Driven in the tests by
  `set_readback_limit`, because reaching the real limit needs a canvas too large
  to ask a CI runner for.
- **A cancelled capture is marked, not dropped**, for the reason `reset_probes`
  gives. Both halves have to be told — the renderer gives its buffer back, the
  scheduler stops waiting — which is what `app.rs`'s `stop_autosave_of` is for.
  Miss the scheduler half and *no* document is ever autosaved again, since only
  one capture runs at a time.
- **The metadata is snapshotted when the capture begins.** The readback spans
  frames and the encode spans a thread; a layer renamed in between would
  otherwise produce a file whose names and pixels came from different instants.
- **Autosaving to the document's own path overwrites it without asking.** That
  is what an autosave is. The tab's dot only comes off if `Tab::revision` still
  matches the number taken when the capture began — claiming work was safe when
  it was not is worse than claiming nothing.
- **The undo history is not written.** Up to 32 MB of PNG-encoded patches, every
  five minutes, unattended. An autosave exists so the painting is not lost, not
  so the afternoon can be replayed.
- **Expiry can only reach inside one directory, structurally.** `Reaper` is the
  only thing in Umber that deletes a document, and "the callers only pass
  internal paths" is not good enough — a later change makes that false in
  silence. So: one canonicalised root; every candidate canonicalised
  independently; the candidate's *parent* required to equal the root, so it
  cannot descend; `symlink_metadata` first, so a link out of the directory is
  refused before it is even resolved; no recursion; and only names an autosave
  writes. The sweep is run against the directory an internal copy was just
  written to, so there is no second statement of where that is.
  `a_reaper_refuses_a_path_outside_its_root` and
  `expiry_after_a_write_takes_the_old_copies_and_leaves_the_painters_file` are
  the two that matter. **Do not add a call to `remove_file` that goes round
  it.**
- **A failure says so once and carries on.** A broken autosave must never
  become a dialog that reappears over somebody's canvas every five minutes, and
  it must never stop somebody painting.
- **The window's close is refused until every unsaved document is accounted
  for.** `WindowEvent::CloseRequested` used to exit on the spot. The prompt
  names each document rather than counting them, and recomputes the list every
  frame so one saved while it is up stops being named. `Editor::quit_requested`
  is a flag because `ui::draw` has no `ActiveEventLoop`.

### Importing other applications' files

`umber-core::docimport` reads `.ora`, `.kra`, `.psd` and `.png`.

- **An import that loses something must say so.** Every loss appends an
  `ImportWarning` and the UI shows them; the rule is that subtly wrong pixels
  are worse than a refusal, because a refusal sends the artist to export an ORA
  while a wrong import wastes an afternoon.
- **The shipped brush library and a user's own import hold to different
  standards, deliberately.** `examples/build-brush-library.rs` *refuses* a
  MyPaint brush that needs anything Umber cannot render — nothing shipped under
  an author's name should paint unlike their brush. An interactive import
  approximates instead and names what it dropped, via
  `brushimport::dropped_features`. Do not make either behave like the other.
- **`ImportedLayer::pixels` is canvas-sized RGBA8, sRGB-encoded with alpha
  premultiplied in linear space** — exactly what a layer texture holds, so it
  goes straight to `write_texture`. Premultiplying in sRGB is the classic way to
  get haloed edges here.
- **`psd` 0.3.5's `Layer::visible()` returns its own opposite.** Adobe's flag is
  *hidden*; the crate reads it as *visible*. `photoshop.rs` inverts it, and a
  test pins that. Do not "fix" the inversion.

### The brush library

- **`Editor::presets` is the merged list** — everything shipped, then everything
  the user saved — and `apply_preset` selects by *index* into it. `resync`
  rebuilds it and re-finds the selection **by id**, because an index does not
  survive a delete.
- **The user library is a directory, not a file.** `brushes/brushes.ron` plus
  `brushes/tips/*.png`, because a bitmap tip does not go in a text file. A zip
  was the alternative and loses on all three counts: every edit rewrites the
  whole archive, the atomic write would have to risk the masks as well as the
  index, and a stamp is a picture people want to be able to open.
- **`BrushPreset::tip` is a *name*, not a mask.** It resolves through
  `UserLibrary::tip`, which is what lets two brushes share one stamp and one GPU
  upload. A name that resolves to nothing **paints round** — a library copied
  without its tips must still load. `UserLibrary::save` takes the mask
  separately, and `None` there means "keep the tip it already names"; taking a
  tip off is clearing the field.
- **A pre-tips `brushes.ron` is migrated and the original is left in place.** A
  migration that deletes the only copy of somebody's collection has to be right
  first time. Guarded by `a_flat_library_is_migrated_into_the_directory`.
- **Grouping is derived from `BrushPreset::category`, and the user's own filing
  overrides it through `BrushPreset::collection`.** Two fields, because they
  answer different questions: `category` is what the brush arrived with — the
  pack's grouping or `style::classify`'s reading of the name — and it is what
  the brush falls back to the moment the user's choice is taken off. Overwriting
  it would throw that away. An import sets `collection` to `preset::IMPORTED`
  ("Imported"), because twenty brushes filed correctly across six collections
  are twenty brushes somebody has to go and find.
- **A user's filing of a *shipped* brush cannot live on the preset**, and this is
  the one thing here that is easy to get wrong. `preset::builtin` is
  `include_str!`'d into the binary and replaced wholesale by every update, so a
  choice written there would survive until the next release and then vanish —
  silently, months later. It goes in `Library::collections`, a table beside the
  presets in the user's own `brushes.ron`, keyed by the brush's **stable id**:
  the file is in the user's data directory, which an update never touches, and
  an id survives a release adding, removing or reordering shipped brushes where
  a position would not. `UserLibrary::assign` routes by ownership — a brush the
  library holds carries its collection on the preset, so it travels into an
  export — and `resync` stamps the table back on every time the merged list is
  rebuilt. `a_shipped_brushs_collection_survives_the_shipped_library_being_
  replaced` is the guard. A dangling entry is **kept**: this module cannot
  enumerate the shipped library, and a brush that has gone may come back.
- **`Index::rank`'s "yours first" reads the collection's *name*, not its
  members.** A name `style::classify` could never produce — "My brushes",
  "Imported", one somebody typed — is one somebody chose. Reading it off the
  members ("every brush in here is the user's") looks equivalent and is not:
  dragging one shipped brush into "My brushes" would send the collection you use
  most to the bottom of the rail.
- **The drag that moves a brush between collections is a model, `brushdrag.rs`,
  with no drawing in it** — the same division `dock.rs` keeps against
  `panels.rs`. It decides which rail row the pointer is over and refuses the one
  target that would do nothing, the collection the brush is already in; a "lands
  here" mark over something that will not happen is worse than no mark.
  `brushlib.rs` supplies this frame's pointer and the rectangles the rows landed
  in, and the highlight is drawn from the *previous* frame's aim so it can be
  part of the row rather than painted over its label.
- **A notice must wrap at a width it is given.** A label in an egui horizontal
  layout defaults to `TextWrapMode::Extend`, so `brushlib::notice_bar` and
  `controls::banner` used to size the strip — and with it the modal and the
  window — instead of being sized by it. `set_max_width` does not fix it; an
  extending label overruns the ui. An import that dropped several features is
  the longest text in the interface and put the browser wider than the screen.
  The browser's own size is in `theme::metrics` for the same reason.
- **`set_tip` is called from `start_stroke` and nowhere else**, and skips the
  upload when the mask is the same `Arc`. Identity, not equality: comparing a
  megabyte of coverage would put back the cost the check exists to avoid.
  Changing a tip mid-stroke would restamp what is already in the scratch under a
  new shape, and the failure direction of the guard is a *stale* tip —
  `a_second_brush_with_a_different_tip_replaces_the_first`.
- **A non-square tip keeps its own proportions, via a per-pass uniform.**
  `tip_scale` is the mask's dimensions with the longer normalised to 1, and the
  vertex shader scales the quad's axes by it — which is exactly the geometry
  padding the mask into a square used to produce, minus the empty margin and the
  padded texture. It does *not* go through `dab_ratio`: the ratio's long axis is
  the dab's *x* axis, so a portrait mask would have to be rotated a quarter turn
  and rotated back, and the ratio is the user's to squash a stamp with.
  `tip_scale` is `(1, 1)` with no tip, which is the exact identity.
- **A tipped dab has an angle whatever its roundness**, because a bitmap is not
  rotationally symmetric. `Brush::dab_has_angle` answers only the elliptical
  half — the tip is a *name* the editor resolves — so the UI combines it with
  `Editor::tip`.
- **The shipped library can carry masks**, through `tip::builtin` and the
  generated `tip_table.rs`. `BrushPreset::tip` resolves against the user's
  library first and the shipped table second. The table is generated from the
  directory listing by `examples/build-bitmaps.rs`; do not hand-edit it.
- **Key dispatch happens at the winit level, before egui sees a keystroke**, so
  every text field outside the canvas has to suspend it via
  `shortcuts::set_capturing`. Without that, typing "brush" into a search box
  selects the brush, then the eraser, on the way past.
- **The library's modals are drawn from `panels::sidebars`, not from the
  Brushes panel body.** The layout can hide that panel, and a modal that goes
  with its panel cannot be shut and cannot be reopened.
- **The way into the brush editor is a mark in the Brushes *header*, not a link
  under the list.** The design draws `✎ Edit "<name>"…` at the foot of the panel
  body; a panel dragged short scrolls that out of sight, taking the only way to
  change a brush with it. Import stays a link, because it opens a file dialog
  and a mark cannot say which four applications' brushes Umber reads.
- Nothing on the drawing path allocates per frame: the grouping and credit lines
  are built once per change, search folds case in place, and rows scrolled out
  of view skip painting. At 201 presets the naive version of each shows up in a
  frame time.

### Uniform layout

The Rust `#[repr(C)]` struct and the WGSL `struct` must agree byte for byte, and
WGSL's alignment rules are not C's. In particular **`vec3<T>` is 16-byte
aligned**, so a `vec3<u32>` pad does not match a Rust `[u32; 3]` — it shunts
everything after it to the next boundary and the buffer comes out 16 bytes
short. Use scalars for padding. A mismatch shows up as a wgpu validation error
naming both sizes, which is at least easy to read.

### Colour space

Linear RGBA everywhere inside the engine — blending in sRGB darkens midtones.

The surface format is deliberately **non-sRGB**. egui emits colours that are
already gamma-encoded, and an sRGB surface would encode them twice, washing out
the UI. `composite.wgsl` therefore does the linear→sRGB encode explicitly at the
end. If you switch the surface to an sRGB format you must remove that encode,
and the UI will be wrong again.

### Coordinate spaces

- **Document space** — pixels in the canvas, y-down, origin top-left. Brush
  sizes and dab positions live here, so brush appearance is zoom-independent.
- **Screen space** — physical window pixels. winit reports these; egui uses
  points (`physical / scale_factor`).

`Camera` converts between them. `zoom_at` keeps the document point under the
cursor pinned — without that correction the canvas slides away as you zoom.

### Dab shape

A dab is an ellipse with an angle, and may be scattered off the stroke and have
its radius jittered. `Brush::size` always describes the **long** axis, so
raising `dab_ratio` narrows the dab rather than growing it.

- **The angle has three states, not two.** Fixed (`dab_angle` alone — a broad
  nib), following the stroke (`dab_angle_follows_stroke` — a rake), and rolled
  per dab (`dab_angle_jitter` — grain, a fringe, charcoal). Jitter is an
  *offset* on whichever of the first two applies, not a replacement for it, and
  it is uniform rather than gaussian: a rotation that clusters around one
  heading is still a comb.

- **The quad is built rotated and squashed in the vertex shader**, so `local`
  stays the fragment's position in the dab's own frame and `length(local) <= 1`
  still means "inside". That is what keeps the fragment falloff identical to
  when every dab was a circle, and it means a 20:1 chisel rasterises a thin
  quad rather than the square containing it.
- **Antialiasing is sized from the *short* axis.** It is the demanding one: a
  chisel two pixels across needs the same softening a two-pixel round brush
  does.
- **The damaged rect must cover the dab's *quad*, not its circle.**
  `StrokeBuilder::bounds` unions the axis-aligned box of the rotated quad of the
  *scattered* dab. Too tight and the edge of a mark is never committed — it
  redraws as a live preview and is then baked in by the next stroke, in that
  stroke's colour, and that has now happened three times. A round dab fits its
  bounding square at any angle, which is why the circle held until bitmap tips
  arrived: **a tip paints into the corners**, and a quad turned 45° reaches
  `radius * sqrt(2)`.
- **The scatter RNG is seeded per stroke, never from the clock.** A stroke has
  to redraw identically, or every pixel test involving a scattering brush
  becomes flaky and undo/redo would not reproduce the same marks.
- **Every random draw is guarded by its `> 0.0` setting.** The RNG is a single
  stream, so an unconditional draw for a feature a brush does not use would
  reshuffle the numbers every *other* feature gets. `a_brush_with_no_new_
  dynamics_emits_exactly_what_it_used_to` pins that.

### Pressure dynamics

Four parameters follow pressure — size, opacity, hardness and scatter — and
three of them share one shape: `peak × (min_ratio + (1 - min_ratio) ×
curve(p))`, behind a `pressure_*` flag. Opacity has no floor because coverage
genuinely reaches zero.

- **Read them through `radius_at` / `hardness_at` / `scatter_at`, never off the
  field.** The field is the value at full pressure, not the value now.
- **`min_scatter_ratio` may legitimately be zero**, unlike the size and hardness
  ratios: "clean line until you press" is a real pencil, and 16 shipped brushes
  are exactly that.
- The importer's `normalised_curve` fills all three from MyPaint's mappings, and
  the same rule governs each: MyPaint states most of what a brush *does* as a
  mapping on top of a base of zero, so reading `base_value` alone silently drops
  it. See `docs/brushes.md`.

### Inputs other than pressure

`umber_core::dynamics` is the general form of the above: a fixed-capacity table
of `(target, input, curve, range)` on every `Brush`, evaluated once per dab.
Speed, stroke position, direction and a per-dab random draw reach size, opacity,
hardness, scatter, ellipticity, angle, colour pickup and the dab's own colour.

- **The table is fixed-capacity because `Brush` is `Copy`,** and it serialises
  as a plain sequence of only its live entries — so an empty one costs `[]` and
  a library written before it existed still loads. A curve per input per target
  would be 60 curves on every brush to carry a median of two.
- **An empty table is the fast path and must stay one.** `StrokeBuilder` skips
  the speed filters, the stroke ramp and the whole evaluation when nothing reads
  them, and every branch is guarded by the setting that reads it — not by a
  blanket "is anything modulated".
- **The random draw is guarded, like every other one.** The RNG is a single
  stream; an unconditional draw for a feature a brush does not use reshuffles
  the numbers every other feature gets. One draw per dab, shared by every entry
  that reads `Random` — which is also exactly what libmypaint's `random_input`
  is. `a_modulation_that_reads_no_randomness_leaves_the_rng_alone` pins it.
- **A modulation's units are MyPaint's, not Umber's**, and they compose
  differently per target: size is a *log* offset so it multiplies the radius,
  opacity is a *factor* because MyPaint multiplies two settings to reach it, and
  the rest add. `Modulated` documents each.
- **`Brush::colours_dabs` — not `smudges` — decides
  `StrokeStyle::per_dab_color`.** A colour modulation puts a stroke on the
  per-dab colour path just as pickup does, and the flag has to agree with what
  `draw_dabs` was told, for the whole stroke.
- **`mypaint.rs`'s `piecewise` is libmypaint's `mypaint_mapping_calculate`, line
  for line, extrapolating end segments included.** It does not hold end values.
  Extrapolation is bounded because `DabInput::normalise` clamps the input to its
  domain; remove that clamp and one bad timestamp is a dab the size of the
  canvas.
- **Everything in a `.myb` goes through `MybFile::eval`.** Reading a
  `base_value` directly is what shipped three brushes invisible and thirteen at
  the wrong size. An input Umber cannot produce is *evaluated at its neutral*,
  not skipped — that is what MyPaint renders on the same machine.

### Colour pickup

A smudging brush needs to know what the canvas holds under it, every frame of a
stroke. `read_layer_rect` and `pick_colour` both block on the GPU and must never
be called from the drawing loop, so `probe_canvas` is the non-blocking version:
it records a small composite into a rotation of staging buffers, and
`take_probe` collects whichever came home a frame or two later.

- **The probe reuses the composite pass**, with the in-progress stroke included.
  That is what lets a blender pick up its own wet paint when scrubbed back and
  forth, and it means there is no second copy of the blend maths to keep in step
  — the same reason `export_rgba` and `pick_colour` reuse it.
- **The lag is not a defect.** A smudge is a trailing average by construction,
  and `smudge_length` delays it far more than the readback does.
- **The composite's export path returns sRGB with straight alpha.** Decode to
  linear before averaging: the mean of two gamma-encoded values is not the
  encoding of their mean, so averaging the bytes makes a brush crossing an edge
  pick up a colour lighter than either side.
- **Reset the probes when a stroke ends.** A sample belonging to the previous
  stroke arriving during the next one smears a colour picked up somewhere else.
- **Classify brushes with `umber_core::style`, and read the name before the
  settings.** Sorting on `smudge >= 0.5` looks obvious and is wrong: every
  MyPaint oil paint mixes with what is under it, so that rule put 68 of 196
  brushes in "Blenders" and emptied the paints. Erasing is the only setting that
  overrides a name.

### Undo

Stores the RGBA bytes of the rectangle a stroke damaged, not whole layers (a
full 2048² snapshot per stroke would exhaust a gigabyte in ~60 strokes).

The capture happens at **commit** time, not stroke start: the layer is untouched
until commit, so reading it there yields exactly the pre-stroke pixels, and by
then the damaged rect is known. `read_layer_rect` blocks on the GPU, which is
acceptable once per stroke but must never move into the drawing loop.

- **Every entry is an `Edit` — a patch, an `EditKind` and a `Timestamp`** — and
  both the kind and the time travel with it across an undo, via `Edit::made_at`.
  Recomputing either on the far side would renumber and re-time the list as it
  is stepped through, and the kind is read off the *snapshotted* stroke style,
  so switching tool mid-stroke cannot change what the stroke that is ending
  turns out to have been.
- **`EditKind` has a variant only for something the engine can restore.** It is
  Paint, Erase and Transform because an entry exists only where a patch was
  captured. Adding "Clear layer" or "Delete layer" means making those undoable
  *first*; a row naming an action that clicking it will not undo is worse than
  one the list stays quiet about, and the History module's footnote exists to
  say so. The same bound governs the icons: `panels::edit_icon` is exhaustive
  over `EditKind` deliberately, so a new variant cannot be added without
  deciding what it looks like — and an icon set richer than the enum would be a
  promise about what the engine records. **A paste is not a variant of its
  own**: it is a Transform patch with nothing where the pixels came from, and
  two rows that undo identically should not have two names.
- **The time is wall-clock, and may be absent.** `Instant` means nothing outside
  the run that produced it and these go into a file, so `umber_core::time`
  carries a `Timestamp` in Unix milliseconds. `Edit::at` is an `Option`, and
  `None` — an entry out of a document written before histories carried times —
  draws an *empty* column. A time invented at import would be
  indistinguishable from a recorded one.
- **`History::gap_at` is a gap, not an age.** What the list shows is how long
  passed between one mark and the next, which is a property of the pair and does
  not change as the afternoon wears on; an age would need the panel repainted
  every second to stay true. It returns `None` where the clock ran backwards:
  `Timestamp::since` refuses to report a negative interval as a duration,
  because an NTP correction is not something an artist spent.
- **There is no date crate, and the tooltip says UTC.** Hinnant's
  `civil_from_days` is twenty testable lines and is pinned across 1970, 2000 and
  2100 and against an independent calendar for every day of forty years. A crate
  would not buy the thing that would justify one — *local* time — because
  `time`'s local-offset refuses to answer in a multi-threaded process, and Umber
  is one. Labelling the zone is what makes UTC honest rather than two hours
  wrong.
- **The two stacks read as one timeline** — everything applied, oldest first,
  then everything undone in the order redoing would put it back. `kind_at`
  indexes it without allocating, and `position` is the count of applied edits,
  which is what a row of the list stands for.
- **A jump is a count of single steps, not a seek.** `steps_to` clamps to what
  is held and the app carries it out as that many `undo`/`redo` calls. There are
  no snapshots to jump to — that is the whole design — so an eight-row jump
  costs eight blocking reads and writes, which is fine on an explicit click and
  is why nothing on the drawing path may reach it.
- **Evictions are counted, not forgotten.** `dropped` is what lets the list
  admit it no longer reaches the start of the document instead of drawing the
  oldest surviving entry as though it were the beginning. It survives a save,
  because a history that did not reach the beginning when it was written does
  not reach it now either.
- **The budget is a fixed 512 MB, not a fraction of the canvas — and the panel
  names the figure once anything has been dropped.** A patch is the *rectangle*
  a stroke covered, so its cost follows the canvas rather than the mark: on a
  10000² document a stroke drawn across the picture is 400 MB, the budget holds
  exactly one, and the second ages the first out. That is correct and it is
  indistinguishable from a bug unless the module says what the limit is —
  "Earlier edits discarded" alone was read as a regression from the banded
  readback, which touched none of this. Scaling the budget to the canvas is the
  tempting fix and is wrong twice: it is *per document*, so a few large tabs
  would be gigabytes, and a short history is a far better failure than being
  killed for memory. Compressing the patches in memory is the other tempting
  fix — `measure-history.rs` puts PNG at the fast level at about 1.6 ms/MB, so
  a 400 MB patch is two thirds of a second of encoding added to every
  pointer-up, and as much again to decode on undo. If depth on a large canvas
  is ever genuinely wanted, the fix is a patch that stores *tiles* rather than
  the stroke's bounding box; nothing short of that is worth the change.
- **`restore` rebuilds the whole timeline from one read out of a file** —
  entries in timeline order, the position within them, and `dropped` — and still
  answers to the in-memory budget, so a file written by a build with a larger
  one cannot hand this process more than it allows. See "The document format".

## Interface

Layout and tokens come from the **"Umber app"** screen of the Umber design
project (Claude Design project `3bfca321-22c2-4bf2-bbc9-80fab57f1e65`, read via
the `DesignSync` tool). That page supersedes the earlier "Umber Explorations"
page — go by it.

Most of the design is built: layout edit mode, the brush editor and library, the
settings dialog, document tabs and the splash. What is not — the navigator, the
brush editor's Wet edges section, Palette and Harmony picker modes,
ten of the sixteen tools, drag-to-reorder in the rail, saved workspaces — is
listed in the README's "What is not there yet", with the reasoning in
`docs/architecture.md`'s roadmap and, for the brush settings, `docs/brushes.md`. **Do not add UI for features that do not work** — a
disabled control with an explanatory tooltip is better than a live one that
lies, and a control that is simply not drawn is better than either where the
design shows a whole row of them.

- **Never hard-code a colour.** Everything comes from `theme::Palette`, which is
  what makes the second theme a table of values rather than an edit sweep.
- **`theme::metrics` holds the design's fixed sizes.** Use them instead of
  re-typing 264.0 or 36.0 at the call site.
- **Never put a Unicode symbol in the UI.** Archivo carries none of them, so
  they render as blank boxes. Add a variant to `icons::Icon` and draw it.
- **Shortcuts live in `shortcuts.rs`, not in a `match`.** The settings dialog
  enumerates them, which a match arm cannot do. `resolve` compares ctrl/shift/alt
  exactly — that is what stops plain `Z` (zoom tool) also firing on `Ctrl+Z`.
- **A shortcut *label* follows the user's keyboard; the stored form never may.**
  `keylayout` asks the platform what the layout prints and `key_name` falls back
  to `us_key_name` where it cannot say, so Zoom in reads "Ctrl++" on a Nordic
  board — the key that actually zooms in. `Chord::id` and `key_id` are untouched,
  for the reason `Chord::id` gives: the file has to parse on the next machine.
  Precedent: `display` already prints "Cmd" where the id says "Ctrl".
  `keylayout::name_for` is a pure function of an injected reading, which is the
  only way the Norwegian and German answers are tested at all, and the reading is
  cached — the platform is asked from the *input* path, never while painting.
- **A widget revealed on hover must not be what decides the hover.** egui stops
  its hover search at the topmost *interactive* widget, so a `Sense::hover()`
  row reads as not-hovered the moment the pointer is over a button inside it —
  and if that button only exists while the row is hovered, the two oscillate
  once a frame. Allocate the row unconditionally and test `contains_pointer`,
  which is geometry alone. This was a real bug on the Shortcuts page's `+`.
- The design's sliders, toggles and segmented pickers are **painted** in
  `widgets.rs`. Restyling egui's stock widgets into them was tried and fights
  the framework; add to `widgets.rs` instead.
- **The canvas scrollbars are `ScrollSpan`'s geometry and `widgets.rs`'s
  paint**, the same division `dock.rs` and `panels.rs` keep. `ScrollSpan` lives
  in `umber-core` because it decides where the picture goes and is testable
  without a window. Two rules it exists to hold: a bar is drawn when part of the
  document is outside the view — which covers "larger than the window" *and*
  "fits, but pushed under a panel" — and the scrollable travel is the document
  plus one viewport, **not** the union of the document and the view. A travel
  that grew as the view left the document would change under the pointer
  mid-drag and the thumb would accelerate away from the hand holding it.
  `Editor::scroll_bars` records where they landed, because they sit *inside* the
  canvas region in egui's background layer, so neither `pointer_over_canvas` nor
  `app.rs`'s `layer_id_at` check would otherwise keep a press on one from
  starting a stroke.
- **The canvas does not fill the window.** `Camera` takes a `pivot` — the centre
  of the panel-free region — and `CompositeParams::pivot` must be the same
  value, or strokes land away from the cursor. Both come from
  `Editor::canvas_pivot`, set from the central panel's rect each frame.
- egui works in **points**, the canvas in **physical pixels**. Convert with
  `pixels_per_point` at the boundary.
- egui keeps separate light and dark styles; `theme::apply` writes both and sets
  the preference, otherwise switching themes leaves egui's internals in the old
  mode.
- **HSV is the colour picker's state, not a derivative of the colour.** Hue is
  undefined for greys, so deriving it from RGB each frame means dragging value
  to black silently resets the hue to red. `Editor::hsv` is the source of truth;
  `set_color` preserves hue when the incoming colour has none.
- `Color::to_hsv` runs over **sRGB** components, not linear ones. A picker is a
  perceptual instrument; HSV over linear values bunches badly in the shadows.
- Watch for `powf` on a value that can go slightly negative — `sin(PI)` in f32
  is just below zero, and a negative base with a fractional exponent is NaN,
  which ecolor's `gamma_multiply` asserts on. This has already bitten once.
- **The brush editor's sections are Tip, Dynamics, Inputs, Scatter, Texture and
  Blending.** The design names six and this is six, but not the same six: **Wet
  edges** alone still has no engine behind it and is not drawn at all, and
  Stabiliser is one slider that rides on Tip. Blending and Inputs are names of
  our own — colour pickup needed a home and none of the design's is one, and the
  modulation table is a *list* where Dynamics is three pressure curves, so
  Dynamics is pressure and Inputs is everything else. Texture holds the paper
  *and* build-up, because both are about a mark made of many faint stamps rather
  than one solid one. Between them they expose every field of `Brush` — adding
  one means adding a control, or the library can use a brush nobody can make.
- **The transform tool's handles are `Transform::grab`'s positions, painted.**
  `ui::transform_box` reads the same eight `handle_at` answers the hit test
  does, so a handle cannot be drawn where the pointer disagrees with it — which
  is the worst kind of control that lies. Solid line, not the selection's
  dashes: the two are often on screen together and mean different things.
- **The selection tool's mode is a dropdown, and it is the Colour panel's
  dropdown pattern.** A painted trigger and a popup of `selectable_label`s, the
  same shape `picker_mode_switch` uses. One dropdown pattern in the interface
  rather than two, and `widgets.rs` gains nothing a second caller would need.
- **The canvas dialogs are one form and two call sites** (`canvasdlg.rs`). New
  document and Canvas settings ask the same four questions, so they share
  `CanvasForm` and one body; two dialogs drifting apart is how "New" ends up
  offering a preset "Canvas settings" cannot express. They are drawn from
  `ui::draw`, not from a panel body, for the reason the brush library's modals
  are. The anchor control appears **only** when the size is actually changing —
  on a New document there is nothing to anchor, and on an unchanged size it
  would be a live knob that does nothing.
- **`egui::DragValue` is the one stock widget used on purpose.** The design has
  no numeric field, and a canvas size is one of the few values here that people
  type exactly rather than feel for on a rail. Everything else in those dialogs
  is `widgets.rs`'s.
- **The brush list's samples are stamped from the brush**, not drawn from two of
  its numbers. `widgets::brush_sample` is a miniature dab loop under a pressure
  ramp; it seeds its RNG identically on every row, so two rows differ because
  their settings differ and the list does not shimmer as it scrolls. A stamp
  brush draws its *mask*, from one texture per mask cached by `Arc` identity and
  one mesh per row — not one texture per row, and never one per stamp.
- `ResponseCurve` is a fixed array of evenly spaced samples, not free control
  points. That keeps `Brush` `Copy`, makes sampling a lerp with no search, and
  means the editor's handles move only vertically — so the curve can never be
  dragged into mapping one pressure to two values. Do not "improve" it into a
  `Vec` of points without solving all three.

### The dockable modules

`dock.rs` is the model — where every module is, what a drag would do — and has
**no drawing in it at all**, which is what lets insertion indices, minimum
sizes, the config round trip and the drop rules be tested without a window.
`panels.rs` paints. Keep the two apart.

- **`PanelKind::ALL` is every module; `DEFAULT_DOCK` is the shipped
  arrangement, and they are deliberately different.** History is in the first
  and not the second. A layout file written before a module existed does not
  name it, and an absent panel is a closed one — so a default that included a
  new module would make a fresh install and an upgraded one disagree about what
  the workspace holds, and the alternative (bumping `umber-layout`) discards
  every arrangement anybody has made. Adding a kind therefore needs no version
  bump; adding one to `DEFAULT_DOCK` would.
- **`PanelKind::Tools` is the one exception to that, and only because a pre-
  columns file names the rail.** The tool rail used to be chrome — always
  present, with a side of its own — so an old config records `rail <side>`.
  `from_config` reads that as a Tools column at that edge, the outermost, which
  is where the rail was drawn; so a fresh install and an upgraded one agree
  without the version moving. The writer no longer emits `rail`, and its
  absence is what tells a later load that a *removed* Tools module was meant.
  `a_config_written_before_the_rail_was_a_module_still_has_a_rail` and
  `a_removed_tool_rail_stays_removed_across_a_save` are the pair.
- **A column's minimum width is the widest `PanelKind::min_width` in it**, not
  one number for every column. The rail's whole point is being narrow and every
  other module is unusable there, so `metrics::TOOL_RAIL` is the Tools floor and
  `limits::SIDEBAR_MIN_WIDTH` everybody else's — and dropping Colour into the
  rail's column widens it rather than leaving a picker three buttons across.
- **A side is a list of `Column`s, ordered from the window edge inwards**, and a
  column is exactly what a whole sidebar used to be: a stack with a width. Index
  0 is against the window on *either* edge, so nothing downstream has to ask
  which way round a side counts. That shape is also what kept the config's
  version header still: a file written before columns names none, and every
  `dock` line for a side falls into the one column that side implicitly has. The
  `width` line is read and no longer written, and it is the whole of the
  migration — `a_config_written_before_columns_loads_as_one_column_a_side` pins
  it. **Do not bump `umber-layout` for a change an old file can be read into.**
- **A column emptied by a drag is kept until the drop resolves.** Removing it on
  the spot slides every column outside it sideways under the pointer, and
  renumbers the very column indices the drop target is being computed against.
  `Layout::prune` runs *after* the insert, from everything that ends a drag and
  from `close`. `a_column_emptied_by_a_drag_survives_until_the_drop` guards it.
- **Adding a module hands it to the pointer.** `Layout::add_dragging` lifts it
  straight into a drag with `Origin::Closed`, so the same drop that moves an
  existing panel places the new one and `Esc` abandons the add. Do not "simplify"
  it into `open`, which appends to a sidebar — that is what the Window menu's
  checkboxes are for, and a checkbox that threw the pointer into a drag would be
  a surprise.
- **A drag begun by a *click* cannot end on a release.** The button is already
  up, so the ordinary test drops the module on the button that added it, on the
  next frame. `Layout::drag_should_drop` gives a sticky drag its own rule: the
  next press, and not until a frame has been seen with the pointer idle, because
  a fast click reports press and release together. The rule is in the model so
  it is tested without a window; `panels.rs` only supplies this frame's pointer.
- **The module library is drawn from `panels::sidebars`**, like the brush
  library's modals and for the same reason, plus one of its own: it is how a
  removed module comes back, so tying it to a panel would tie the way back to
  the thing that has gone.
- **A module's picture is painted, never a bitmap.** `module_preview` is a
  schematic in palette tokens. A screenshot would be stale the first time the
  panel gained a control and wrong in the other theme immediately.
- **A module body must not imply the engine does more than it does.** The
  History list has a row only where a patch exists, and says under itself that
  it covers strokes and not layers. Same rule as everywhere else here: a control
  that lies is worse than one that is not drawn.

## Testing

`umber-render/tests/gpu_pipeline.rs` runs real GPU work headlessly: it creates a
device with no surface, stamps dabs, commits, and reads pixels back. These tests
catch shader and blend-state bugs that no CPU test can. They **skip** rather
than fail when no adapter exists, so they stay meaningful on CI runners.

`export_rgba`, `pick_colour` and the autosave's capture all reuse the *screen*
composite pass with an export flag rather than having their own shader. A second
copy of the blend maths would be a second thing to keep in step, and an export
that differs from the screen is a classic bug. Keep it that way —
`render_export` is the shared half, and `a_capture_reads_back_exactly_what_the_
blocking_path_does` is what stops the two drifting.

When changing rendering, add a test there. `overlapping_dabs_do_not_compound` is
the model: it asserts a specific pixel value that only holds if the wet-layer
design is intact.

**All GPU tests share one device and run serialised**, via the `OnceLock` and
`Mutex` in `Harness::new`. Do not "optimise" that away: a device per test meant
seventeen concurrent Vulkan devices each blocking on `poll` for its own
submission, which starved them into a hang — the run never finished rather than
failing. Sharing one device is also ~10× faster.

`composite_pixel` runs the real composite pass into an offscreen target, which
is the only way to test layer opacity and blend modes. Two things to copy when
adding to it:

- Use a **non-sRGB** target format, matching the real surface. An sRGB target
  double-encodes and every expected colour becomes wrong.
- Prefer blend identities over hand-computed values — "Multiply by white is the
  identity", "Screen with black is the identity" are exact and survive rounding.
  Where a value is unavoidable, remember blending is **linear**: 50% white over
  black is sRGB ~188, not 128.

## Platform support

Desktop (Windows/macOS/Linux) works. Android and iOS are architecturally
prepared but have **no build scaffolding yet**:

- `umber-app` builds as a `cdylib` and has an `android_main` entry point.
- Device limits are `downlevel_defaults`, so a desktop build cannot start
  depending on capabilities mobile GPUs will refuse.
- `suspended()` drops the surface but keeps editor state, because Android
  destroys the window when backgrounded.

Do not claim mobile support works — it has never been built or run.

### Pressure

Touch screens report real pressure via winit's `Force`, and so — on Windows —
does a pen: winit's `WM_POINTER` handler calls `GetPointerPenInfo` and hands the
pressure over as `Force`. **A pen therefore arrives as `WindowEvent::Touch`, not
as mouse events**, and produces no `CursorMoved` at all, because winit consumes
the pointer messages and Windows never promotes them to legacy mouse ones.

Two things follow, and both were bugs:

- **Anything deciding whether a press belongs to the document must take the
  position from the event**, not from `Editor::cursor`. `cursor` is written by
  `CursorMoved`, so under a pen it holds wherever the mouse was last left —
  `(0, 0)` on a fresh launch, which is the menu bar. That is what
  `ui_owns_pointer` takes a position for.
- **A `Moved` for a touch id that never `Started` is a hover**, a pen in range
  and off the glass. It must not go in `Editor::touches`: a pen carried out of
  range sends no "up", so the entry would never be removed, and the next press
  — Windows issues a fresh pointer id per contact session — would count as a
  second finger and be read as a pinch.

The pressure resolution is 1024 levels, which is what the `WM_POINTER` API
carries however many the tablet itself distinguishes. `PressureSource::Device`
is the default and passes it straight through; the enum's other arms are the
mouse-only fallbacks. macOS and Linux have no equivalent path yet — do not
describe pen pressure as working there.

**A reported `None` and a reported `0.0` are the same event to winit**, and the
two readings of it are opposites: a mouse has no sensor and must paint at full
pressure, a pen just off the glass is reporting zero and must paint nothing.
winit's Windows path runs the raw value through a normaliser accepting `1..=1024`
and answers `None` for everything else, so the last samples of every pen stroke
arrive looking exactly like a mouse. `PressureModel::resolve` settles it with a
latch: once a stroke has carried one real reading, a later gap is a zero. **Per
stroke, never per session** — a session-wide latch would let one pen stroke make
every later mouse stroke paint nothing, which is far worse than the blob it
fixes.

**1024 levels is not a limit worth lifting, and this has been measured.** A pen
display resolving 8192 or 16384 is the standing reason to reach for WinTab —
`wintab32.dll` does report the device's native axis through
`WTInfo(WTI_DEVICES, DVC_NPRESSURE, …)`, and that is why Photoshop, Krita and
Clip Studio all carry a WinTab/Windows Ink switch. The route exists. What it
would reach does not, and `examples/measure-pressure.rs` is what says so —
re-run it before anyone rebuilds this argument from memory:

- **Coverage gains exactly nothing.** The scratch is `R8Unorm`, so a dab has 256
  expressible coverages, and sweeping the shipped library at 1024 levels already
  produces all 256 — median *and* maximum. At 16384 the count is the same 256.
  These are not levels that are hard to see; they are levels with nowhere in the
  pipeline to be put. `max` and build-up both blend inside that same target, and
  commit writes `Rgba8UnormSrgb`, so neither overlap nor accumulation widens the
  set.
- **Size gains less than a pixel.** Across the 142 shipped brushes whose size
  follows pressure, one level of 1024 moves the diameter by a median of 0.0245
  document pixels — 41 levels per pixel — and 0.28 px at the ninetieth
  percentile. Only the largest brush in the library exceeds a pixel, at 1.56 px
  on a dab 1045 px wide, which is 0.15% of its own width and narrower than its
  antialiased edge. The count of distinct whole-pixel diameters over the whole
  sweep is identical at both resolutions: `Brush::size` and `min_size_ratio`
  bound it, not the input.
- **The light-touch end is compressed by design, so it is the weakest case
  rather than the strongest.** Resolution near zero is the usual argument for a
  finer axis, but `min_size_ratio` (median 0.08) puts a floor under the radius,
  `radius_at` clamps at 0.5 px, and `EASE_IN` — the "light hand stays thin"
  curve — has a slope of 0.24 there. Over the lightest twentieth of the axis the
  median brush reaches three distinct quarter-pixel diameters at 1024 levels and
  the same three at 16384.
- **Two things downstream would blunt a real gain anyway.**
  `StrokeBuilder::emit_segment` lerps pressure between consecutive samples, so
  the dabs between two reports already get continuous floats and a stroke's ramp
  is never a staircase of the input quantisation. And the event rate is the
  coarser axis: a half-second ramp at ~200 Hz is about a hundred samples for a
  thousand available levels, so the granularity a painter can actually meet is
  in *time*, not in the value.

So do not add a WinTab path *for pressure resolution*. If one is ever built it
has to be justified by something else it carries — barrel buttons, tilt and
rotation, or a driver whose Windows Ink path is broken — and `wintab32.dll` must
be `dlopen`ed rather than linked, for the reason the Vulkan loader is: a machine
with no tablet driver does not have the library and must still start.

#### Why widening the scratch does not deliver those 1024 levels

This has been asked twice. The answer is no, and the reason is not the scratch.

**Pressure is only quantised where it drives opacity.** Size, hardness, scatter,
angle and every `dynamics` target take it as an `f32` all the way to the dab, so
1024 levels already reach them intact. `coverage_at` is the one that ends up in
a texture.

**The scratch is exactly as wide as its destination, so it adds no loss.**
`LAYER_FORMAT` is `Rgba8UnormSrgb` and an sRGB format encodes *RGB only* — its
alpha channel is linear 8-bit. Commit therefore re-quantises coverage to 256
levels whatever the scratch held. Measured on the GPU: 1024 pressure levels
produce **256 distinct committed alphas**, and that number is unchanged by an
exact-float or an `R16Float` scratch. `a_pressure_step_finer_than_the_layer_
makes_no_mark` pins both halves — a 1/1024 step is invisible, a 1/255 step is
exactly one level. **Only a wider layer could change this**, which is the last
point below.

**`R16Unorm` is not available anyway**, and this is the point to check first.
It requires `Features::TEXTURE_FORMAT_16BIT_NORM`, which Umber does not request
because device limits are `downlevel_defaults`; and even *with* the feature,
wgpu's `guaranteed_format_features` gives it `storage` usage, **not**
`RENDER_ATTACHMENT`. It cannot be a render target on the guaranteed set at all.
`R16Float` is the only single-channel 16-bit candidate left — `R16Uint` and
`R16Sint` are attachments but integer formats do not blend at all. It is
`(msaa_resolve, attachment)` on `Features::empty()` and BLENDABLE because its
sample type is filterable float, so `max` and the build-up blend would both
work. `Rgba16Float` is already used for the colour scratch, which is the
existence proof.

**The one thing a 16-bit scratch would buy is build-up accumulation**, where
`a = cov + a(1 − cov)` compounds rounding inside the scratch. `max` gains
nothing — it is monotone and idempotent under round-to-nearest. Build-up's
failure mode is a *stall*: once `cov * (1 − a)` falls below half a level the
accumulator stops moving, so a constant coverage of `1/255` asymptotes at 0.5
rather than 1.0, and one below `1/510` never builds at all.

**Measured, that is worth at most 3 levels of 255.** Stamping the one shipped
preset that sets `build_up` (`pack01-drybrush`) along a stroke at its own
spacing, 50 dabs deep, against exact arithmetic: `R8Unorm` is at most 3 levels
out, mean 0.5, with 2.8% of the stroke's pixels more than one level out;
`R16Float` is at most 1. The stall needs a *constant* faint coverage on one
pixel for a hundred-odd dabs, and a bitmap tip cannot produce that: the mask
slides under the stroke, so a pixel sees a different texel every dab, and the
mask is itself an 8-bit `R8Unorm` texture with no *stored* value below `1/255`
for a wider scratch to recover. A user could still build the adversarial
brush (build-up plus a low pressure-opacity ramp, painting very lightly), where
the error reaches tens of levels; no shipped preset combines them, and the
remedy is a wider *layer* for that too.

**The cost is real:** the scratch is canvas-sized, so `R16Float` doubles it —
200 MB instead of 100 MB on a 10000² canvas — on the texture the dab pass
read-modify-writes per fragment and the composite samples every frame. Nothing
reads the scratch back to the CPU, so no readback path is involved.
`STROKE_FORMAT` is shared with the tip and grain masks, which are genuinely
8-bit source data and would have to be split off first.

**Raising the layer to 16-bit is the only answer that would work, and is not
worth it now.** It doubles every layer slice (400 MB → 800 MB per layer at
10000², against `MAX_LAYERS` of 64), halves the reach of undo's 512 MB budget
since a `PixelPatch` would carry 8 bytes per pixel, and lands on the file
format: ORA is 8-bit PNG, so a 16-bit layer either truncates on save — making
`saving_and_reopening_does_not_move_a_pixel` false — or writes 16-bit PNG that
other ORA readers may refuse, against the whole point of the format choice. It
also changes `ImportedLayer::pixels`' contract and every readback
(`read_layer_rect`, the autosave capture's 4 MB/frame budget, `export_rgba`,
`pick_colour`, `probe_canvas`). Note it would *fix* rather than break the
`LAYER_FORMAT` sRGB argument, since `Rgba16Float` has the mantissa 8-bit linear
lacked. It is a coherent future change and a large one; it is not a pressure fix
worth 256→1024 levels of alpha nobody has yet shown they can see.

### Settings, Input & pen

`umber-app/src/inputlog.rs` records the pointer stream and `settings.rs`'s
`input_pane` draws it. It exists because **nobody working on Umber has a pen**,
so everything above shipped unverified; the pane is how somebody with the
hardware settles it. The rules it lives by:

- **It is observation, and nothing downstream may read it.** A stroke must
  behave identically whether the pane is open or not. `Editor::input` is
  therefore above the `--- documents ---` line — it describes the tablet plugged
  into this machine, not a document — and `Editor::note_input` is called once,
  from `window_event`, before the match.
- **The resolved figure is recorded, never recomputed.** `resolve` mutates the
  model, so calling it again to have a number to draw would corrupt the one
  driving the real stroke. `note` pushes the event's sample and `Editor::sample`
  amends it with what the one real call answered — which is why the note has to
  run before the event is dispatched.
- **The test strip runs on a private copy of the model**, reset per press,
  because it is dragged while no stroke exists and there is no real call to
  record. `settings::show` ends the probe whenever the pane is not in front, or
  a drag interrupted by closing the dialog would go on resolving every event in
  the application.
- **An absent reading is never drawn as a zero.** `value_meter` takes an
  `Option` and `pressure_graph` breaks its line at a gap. Printing `0.00` for
  "the device said nothing" would put the exact ambiguity the latch exists to
  resolve back on the page.
- **The ring is fixed-capacity**, because it is written once per pointer event,
  which is the drawing path.
- **Tilt is a sentence, not a meter.** winit carries one only as the stylus
  altitude inside a `Force::Calibrated`, which is iOS's form; a readout sitting
  at zero would look like a device answering.

## Releasing

```sh
pwsh tools/release.ps1 0.0.2 -DryRun   # or: sh tools/release.sh 0.0.2 --dry-run
pwsh tools/release.ps1 0.0.2           # for real
```

**Cutting a release is pushing a tag.** Everything else follows from that, which
is what stops a release being a sequence somebody has to remember. The script
checks the tree, the version and the notes, runs the same gates CI runs, then
pushes `main` and an annotated `v<version>` tag. It uploads nothing, so it
cannot half-publish; `.github/workflows/release.yml` does the rest.

- **`CHANGELOG.md` is the release notes**, published verbatim. There is no
  second place to write them and therefore no way for GitHub and the repository
  to disagree. A section starts at `## <version>` — alone or followed by a
  date — and runs to the next `## `. That rule is stated three times, in
  `tools/release-notes.sh`, in `tools/release.ps1` and in
  `crates/umber-desktop/tests/release.rs`; the test is the one that matters,
  because it runs on every CI push rather than only at tag time.
- **The version lives in one place**, `[workspace.package]` in the root
  `Cargo.toml`; every crate takes `version.workspace = true`. Bump it and write
  the changelog section in the same commit — `the_changelog_describes_this_
  version` and `this_version_is_the_newest_entry` fail the build otherwise, so a
  release cannot be cut without notes and cannot publish the *previous*
  release's notes.
- **The workflow tests before it builds.** A tag can point at a commit CI never
  saw, so the gates are repeated there rather than assumed.
- **`workflow_dispatch` is the rehearsal.** It builds and packages everything on
  every platform and publishes nothing — the `publish` job requires
  `refs/tags/v`. Change the workflow, dispatch it, and only tag once it is
  green; a tag spent on a broken workflow is one somebody may already have
  fetched.
- **Linux binaries build on the oldest runner, not the newest.** A binary built
  against an old glibc runs on newer distributions and not the other way round.
- **The libraries that matter are `dlopen`ed, not linked** — the Vulkan loader,
  libxkbcommon, the Wayland and X11 clients. No automatic dependency scanner
  will find them, so `packaging/linux/build-packages.sh` and the `PKGBUILD`
  state them by hand. A package that omits one installs cleanly and then fails
  to open a window, which is the worst shape a packaging bug takes.
- **Packaging is a script, not workflow steps.** `build-packages.sh` runs on any
  Debian-ish machine, so the packages can be rehearsed locally; a release
  process only a robot can run cannot be debugged.
- The MSI's `UpgradeCode` in `packaging/windows/umber.wxs` must never change. It
  is what tells Windows the next version replaces this one rather than
  installing beside it.
- **Three icon mechanisms were not enough; the fourth is identity.** With the
  executable's `RT_GROUP_ICON`, `ICON_SMALL` and `ICON_BIG` all in place, the
  taskbar still drew the generic paper icon. Windows groups taskbar buttons by
  **AppUserModelID**, and a process that never sets one is given a derived
  identity belonging to whatever launched it — so a terminal, Cargo and an
  installer's shortcut produce three different ones. `taskbar::claim_identity`
  takes `io.github.spillebulle.umber` explicitly, and it must run **before the
  first window exists**: the shell reads the identity when it creates the
  button, and setting it later does not move a button already on screen.
- **On Linux the window icon is not used at all.** Wayland matches the window's
  **app id** against an installed `.desktop` file and takes the icon named
  there; X11 matches the entry's `StartupWMClass` against the window's class.
  Umber set neither, so there was nothing to match and no icon, in every Linux
  package. The two platforms genuinely want different strings — the reverse-DNS
  app id for Wayland, `umber` for X11 — and `taskbar`'s tests pin both against
  `packaging/`, so renaming one and not the other fails in `cargo test` rather
  than in a package nobody opens until it is released.
- **`taskbar::APP_ID` is one name across every platform** — the Windows
  identity, the Wayland app id, the Flatpak app id, the desktop entry's
  filename and the installed icon names are all that string. A second spelling
  of it is a mismatch waiting to happen.
- **Windows caches taskbar icons per executable path**, which can pin a stale
  icon for a dev build long after the cause is fixed. A change here is not
  disproved by one look at the taskbar.
- **Windows keeps two icons per window and Umber has to set both.**
  `with_window_icon` is `ICON_SMALL`, the title bar's; the taskbar and Alt-Tab
  draw `ICON_BIG`, which is winit's separate `with_taskbar_icon`. winit
  registers its window class with `hIcon: 0`, so setting only the first leaves
  the taskbar with nothing and Windows substitutes its generic application
  icon — which is what shipped through 0.0.3. The executable's own icon
  resource does not cover this: that one is for Explorer, the Start Menu
  shortcut and the moment before the process exists.
- **The installer's "Start Umber" checkbox must stay `Impersonate="yes"`.**
  Umber installs per-machine, so the installer is elevated; launching without it
  would run Umber as the elevated account and write every preference, brush and
  autosave into that profile instead of the user's. Its condition is
  `NOT Installed` so a repair does not launch the application unasked. The
  action comes from `WixToolset.Util.wixext`, which `release.yml` must add
  alongside the UI extension.
- **RPM requirements are sonames, not package names.** Fedora calls a library
  `libX11` and openSUSE calls it `libX11-6`, so an rpm naming one refuses to
  install on the other; every rpm distribution records the sonames it provides,
  so `libvulkan.so.1()(64bit)` resolves on all of them. Debian's names are
  stable across its derivatives, so the `.deb` may name packages.

### Updating an installed copy

`umber-app::update` asks the releases API what the newest version is, and — for
the installations Umber owns — fetches an asset and swaps the binary. The whole
of it is desktop-only and lives in `umber-app`: `umber-core` and `umber-render`
must not learn about HTTP, the same boundary that keeps them testable.

- **Umber does not sign its releases.** That is the missing piece, and it is the
  reason every guarantee here is stated in terms of the transport: HTTPS only
  (`Agent::https_only`, so redirects cannot drop to plain http), an address
  taken from the API rather than constructed, and a download that must be
  exactly the length the API reported. A length is not a signature and nothing —
  the About dialog least of all — may imply that it is. Signing means a key with
  somewhere to live, a step in `release.yml`, and a public key compiled in;
  until that exists, say what is actually true.
- **An installation a package manager owns is never written to.** `.deb`,
  `.rpm`, Arch and Flatpak copies are detected and told which manager owns them
  and what to run. Overwriting them is usually not permitted, makes the
  manager's records false, and is undone by the next system upgrade — silently,
  months later, which is the worst shape this bug takes. The MSI is the one
  managed case Umber still updates, because Windows supplies the mechanism:
  hand `msiexec` a package. Never edit Program Files directly.
- **The Flatpak does not check at all**, and that is not an oversight. Its
  sandbox is granted no network — `packaging/linux/io.github.spillebulle.umber.yml`
  carries no `--share=network` on purpose — so a request could only time out and
  report a decision as a failure, and Flatpak's own updater already does the
  job. `Updates::check_unavailable` is the switch, and the manifest comment is
  the other half of it.
- **`install::detect` is a pure function of a `Probe`.** The path, the
  environment and a "does this exist" predicate are injected, which is what lets
  the Linux and macOS answers be tested on a Windows machine — the only way they
  are tested at all. Do not reach for `std::env` inside it.
- **Version comparison is numeric, and a tag that is not `v<major>.<minor>.<patch>`
  is ignored.** `"0.0.10" < "0.0.9"` lexically, so string ordering would decide
  the tenth patch release was older than the ninth. Pre-releases are in the
  ignored group deliberately: the release script never makes one, and a stable
  installation should not be walked onto a candidate build.
- **The asset suffixes in `release::wanted_asset` are the other half of
  `release.yml`.** Renaming an artefact there without changing them means an
  update that reports "no build for this machine" for ever.
- **On Windows the swap is rename-then-replace.** A running executable cannot be
  deleted but *can* be renamed, so the old binary becomes `umber.exe.old` and
  the next start sweeps it (`sweep_previous_binary`, called from `run`). If the
  second rename fails the first is undone: a failed update must leave a working
  Umber, not none.
- **The check runs on a thread and wakes the loop with an `EventLoopProxy`.**
  `ControlFlow::Wait` means a value appearing in a channel is not an event, so
  without the wake the answer sits there until the user moves the mouse. That is
  what `app::Wake` and `user_event` are for, and why `update` takes a plain
  closure rather than a winit type — the same layering rule as everywhere else.
- **The startup check is on by default and the first run says so.** Off by
  default is a check nobody ever switches on. On is only defensible because
  `notice_seen` holds the first request back until a notice has been shown and
  answered, and the switch is in Settings, General. Do not make the default on
  without the notice, or quietly widen what the request carries.
- **`ureq` is built with `rustls` and no default features.** native-tls is
  OpenSSL on Linux, which would have to be satisfied on the aarch64 cross-builds
  and inside the Flatpak sandbox; roots are compiled in because an AppImage
  cannot rely on the host's certificate store.
- **No test may touch the network.** The release parsing is driven from a
  fixture, the install detection from injected readings, and the archive
  handling from archives the test builds itself.

### What is built, and what is deliberately not

| | x86-64 | ARM64 |
|---|---|---|
| Windows MSI | ✔ | ✔ |
| macOS archive | one universal binary, both slices | |
| `.deb`, `.rpm`, AppImage, tarball | ✔ | ✔ |
| Flatpak bundle | ✔ | — |
| Arch `.pkg.tar.zst` | ✔ | — |

- **macOS ships one universal binary**, built on Apple Silicon with the Intel
  slice cross-compiled and `lipo`'d in. There is no Intel runner job: GitHub is
  retiring `macos-13`, and a queue that never gets picked up stalls every job
  downstream of the matrix — which is how the Flatpak and Arch jobs went three
  rehearsals without ever starting. Only the native slice is tested, which is
  the honest limit of a machine with one architecture.
- **Arch is x86-64 only because Arch Linux is.** ARM is Arch Linux ARM, a
  separate distribution with its own repositories and no official container
  image; packaging for it would mean trusting a third party's image to build
  something nobody here can test.
- **No RISC-V.** There is no hosted riscv64 runner, so it could only be
  cross-compiled and never executed, and neither wgpu's Vulkan path nor winit
  has been verified on the architecture. Shipping a binary nobody has run is
  the thing `docs/brush-sources.md` and the importer rules both refuse to do
  elsewhere.
- **No Snap.** It would sandbox and auto-update, which is exactly what the
  Flatpak already does, on more distributions; Ubuntu users have the `.deb`.
  Two sandboxed formats is two to keep working for one benefit.
- **No musl or static build.** The application `dlopen`s the Vulkan loader and
  the display client, and `dlopen` under statically linked musl does not work.
- **The Flatpak is a bundle, not a Flathub listing.** Flathub builds from source
  on its own infrastructure, which is a separate submission and a good idea; the
  bundle attached to a release is not a substitute and must not be described as
  one.

## Conventions

- **Commit after every change, and say in the message what changed and why.**
  One commit per coherent change, not one per session's work: a commit that
  bundles a merge resolution with a behaviour fix hides the fix, and the diff
  nobody can isolate is the one nobody can revert. The subject line says what
  the change *does*; the body says what was wrong before, or what a reader
  would otherwise have to reconstruct — the same standard the comments here are
  held to. Most of this file's invariants were learned from a bug, and the
  commit is the first place that reasoning lands.
- British spelling in user-facing strings and docs ("colour", "stabilisation").
- Comments explain *why*, especially where a simpler-looking alternative is
  wrong — most of this file's invariants are also recorded at their call sites.
- This is a GPL-3.0-or-later project. New files carry no licence header, but the
  licence is deliberate: derivative work must stay free.
