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
- **Three `umber-` attributes are the extension mechanism**, and every other ORA
  reader ignores them, which is what keeps the file a plain `.ora`.
  `umber-blend` exists because Add's nearest SVG name (`svg:plus`) is only
  approximate — without it, reopening Umber's own file reports a loss that did
  not happen. `umber-version` is bumped only when a revision stores something an
  older build would drop silently, and an older build then **refuses** the file
  rather than opening it with pieces missing.
- **`mergedimage.png` is the caller's, not `docformat`'s.** Flattening means
  blend modes, and the blend maths lives in the composite shader. A software
  copy here would be a second implementation to keep in step, and a file whose
  preview disagreed with the screen is the bug that produces.
- **A save writes to a temporary neighbour and renames.** A write that dies
  halfway must not replace the artist's last good file with a truncated archive.
- **`read_layer_rect` blocks and a save calls it once per layer.** That is
  acceptable on an explicit Save and nowhere else; it must not migrate towards
  the drawing loop, and an autosave will have to solve it first.
- **Save must close a tab only when a file was actually written.** A cancelled
  file dialog is not permission to discard a document.

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

- **Every entry is an `Edit` — a patch and an `EditKind`** — and the label
  travels with it across an undo. Recomputing it on the far side would renumber
  the list as it is stepped through, and it is read off the *snapshotted* stroke
  style, so switching tool mid-stroke cannot change what the stroke that is
  ending turns out to have been.
- **`EditKind` has a variant only for something the engine can restore.** It is
  Paint and Erase because an entry exists only where a patch was captured.
  Adding "Clear layer" or "Delete layer" means making those undoable *first*; a
  row naming an action that clicking it will not undo is worse than one the list
  stays quiet about, and the History module's footnote exists to say so.
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
  oldest surviving entry as though it were the beginning.

## Interface

Layout and tokens come from the **"Umber app"** screen of the Umber design
project (Claude Design project `3bfca321-22c2-4bf2-bbc9-80fab57f1e65`, read via
the `DesignSync` tool). That page supersedes the earlier "Umber Explorations"
page — go by it.

Most of the design is built: layout edit mode, the brush editor and library, the
settings dialog, document tabs and the splash. What is not — the navigator, the
brush editor's Wet edges section, Palette and Harmony picker modes,
twelve of the sixteen tools, drag-to-reorder in the rail, saved workspaces — is
listed with its reason in the README. **Do not add UI for features that do not work** — a
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
- The design's sliders, toggles and segmented pickers are **painted** in
  `widgets.rs`. Restyling egui's stock widgets into them was tried and fights
  the framework; add to `widgets.rs` instead.
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

`export_rgba` and `pick_colour` both reuse the *screen* composite pass with an
export flag rather than having their own shader. A second copy of the blend
maths would be a second thing to keep in step, and an export that differs from
the screen is a classic bug. Keep it that way.

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

Touch screens report real pressure via winit's `Force`. **Desktop pen tablets do
not report pressure through winit's mouse events**, so desktop falls back to a
flat 1.0 or a speed-derived approximation. The `PressureSource` enum exists so a
native tablet path (Windows Ink / `WM_POINTER`) can be added without touching the
brush engine. Do not describe desktop pen pressure as working.

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
- **RPM requirements are sonames, not package names.** Fedora calls a library
  `libX11` and openSUSE calls it `libX11-6`, so an rpm naming one refuses to
  install on the other; every rpm distribution records the sonames it provides,
  so `libvulkan.so.1()(64bit)` resolves on all of them. Debian's names are
  stable across its derivatives, so the `.deb` may name packages.

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
