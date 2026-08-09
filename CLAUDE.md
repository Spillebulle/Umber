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
   once, over the stroke's damaged rect only, then the scratch is cleared. That
   is the Normal path, which is every stroke there has ever been; a brush
   carrying a blend mode takes a different route, below.

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
- **An importer measures the *paper* for build-up as well as the tip, and the
  two take different statistics.** Both can cap a stroke and the tip's reading
  cannot see the other one. `tip::stroke_coverage` takes the **peak**, which is
  the mark for a stamp: a tip is stretched over its dab, so a `max` stroke is
  capped at the mask's brightest texel and the whole mark is capped with it. A
  grain is anchored to the *document*, so it is sampled at the pixel rather than
  at the dab — its brightest texel survives whatever the strength, the peak
  agrees with itself, and what collapses is the **mean**. That is
  `tip::grain_coverage`, and there is no stamping loop in it because every dab
  reaching a pixel is scaled by the same texel: `max` is exactly the tile, and
  compositing is `1 − (1 − t)^n`. Reading the tip alone shipped six textured
  presets on the `max` path and made a Clip Studio sketch pencil arrive at 27%
  of the opacity its author set — a 500×500 grunge scatter of mean 0.272 at
  `TextureDensity` 100, where Clip Studio's own stroke reaches 77%. Two of that
  file's four textured sub-tools carry no tip at all, so the measurement never
  even ran. **A stencil is the boundary and answers no**: where a tile is only
  ever 0 or 1 there is nothing to build, so the two rules make the identical
  mark and the cheaper one is right. Build-up is for a grain that is *faint*,
  not for one that is merely dark.
- **There are four dab pipelines**, from two independent binary choices
  (per-dab colour, build-up), built by one loop over one descriptor rather than
  four copies of it. `DabStyle` carries both and must be the same for every
  frame of a stroke.
- **A brush carries its own blend mode, and the blend maths lives in one file
  both passes compile.** `shaders/blend.wgsl` is `concat!`ed in front of
  `composite.wgsl` and `commit.wgsl`, so the rule that those two must implement
  identical blending maths is *structural* rather than a discipline — two
  hand-written copies of Multiply is exactly the drift that makes a stroke jump
  at pointer-up. One consequence to know before reading a shader error: naga
  counts lines from the start of the concatenated text, so a line number it
  reports against either file is shifted by the length of `blend.wgsl`.
  `docs/brushes.md` has the whole argument; the rules:
  - **Normal is untouched and must stay the fast path.** One pass, the
    fixed-function blender, no copy and no allocation. The preview writes
    `s + lay * (1 - s.a)` directly rather than routing through the general
    form, because the two agree exactly where the general form would differ in
    the last bit of floating point. That line is not a duplicate of Multiply.
  - **A blended commit needs a *copy* of the layer, and Multiply is why.** No
    combination of fixed-function blend factors produces `B(Cb, Cs)`, so
    `fs_blend` computes the whole thing and is drawn with `blend: None`,
    reading the destination out of a copy — a colour attachment may not also be
    sampled. Same constraint `flip.wgsl` works around.
  - **The copy is per damaged piece, never per stroke rectangle.** A backdrop
    spanning the bounding box is canvas-sized for a *thin diagonal*, which is
    the 381 MB the tiled undo patch exists to avoid put back on the GPU. A
    piece is a contiguous run of cells within one row of the damage grid — so a
    row may hold several — which bounds the backdrop at `canvas width × 64`.
    The cost is a render pass per piece, because a copy cannot be recorded
    inside a pass. That is fine on the desktop and is the thing to revisit on a
    tile-based renderer; `commit_blended`'s docs name the atlas that would fix
    it and why it is not worth building yet.
  - **An eraser and a stroke on a mask both ignore the mode**, and both are
    refused the blended path at the *same* gate. An eraser deposits no colour;
    a mask slice holds coverage read on one channel where `fs_blend` writes
    four. Guarding one and not the other is the asymmetry that gets forgotten —
    `a_stroke_on_a_mask_ignores_the_blend_mode_it_is_carrying` and
    `an_erasing_brush_ignores_the_blend_mode_it_is_carrying` are the pair, and
    the first paints grey on grey deliberately, because a mask is read on `.r`
    and black on white is the one pair Multiply cannot be told from Normal by.
  - **The brush row's preview does not show the mode**, and that is decided
    rather than overlooked — a row has no picture underneath it to blend with,
    and faking one would be a fourth CPU copy of the maths. It is the one
    setting `preview_dabs` does not distinguish; see `widgets.rs`.
  - **It did not move `umber-version`.** A `Brush` never enters an ORA file —
    `docformat` writes `composite-op` and `umber-blend` by hand from the
    *layer* — so this changes no document byte. `brushes.ron` carries it behind
    the `#[serde(default)]` already on `Brush`.
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
- **A live selection carries three real buttons over the canvas** — Deselect,
  Copy, Cut — so their rectangles go in `Editor::selection_buttons` and through
  `canvas_overlay_owns_pointer`, exactly as the transform tool's flip pair and
  `Editor::scroll_bars` do. Miss that and a press on one is also a press on the
  canvas: a dab under the button that was clicked, inside the selection somebody
  was about to copy. The rectangles are recorded on **every** frame the strip is
  drawn, not only the frame it is clicked on, and cleared the moment it is not.
  The pen reaches the same test through `pointer_over_canvas`, which is what
  stops this being a control that works with a mouse and paints with a pen.
- **The strip is gone whenever a float is up.** The transform tool's own buttons
  occupy that place, and a Copy beside them would name a selection that is no
  longer what an edit acts on.
- **Where it goes is `overlay::place_strip`'s, in `umber-core`**, and that
  module is the *selection strip's* rule rather than a general placer — the
  flip pair keeps its own, in `ui.rs` where it is drawn, and the module docs say
  so rather than claiming both. Above the marquee, below it where there is no
  room, over it where there is room on neither side, and clamped into the view
  in every case — measured against the **visible** part of the selection, so one
  three quarters off the left edge gets its strip over the quarter that can be
  seen. Why it clamps where the flip pair declines to draw: a floating transform
  can be dragged back into reach and a selection cannot, so declining would
  leave its commands with no control at all. A view too small to hold the whole
  strip is the one case it *does* refuse, because the caller clips its painting
  to that view and anything hanging off would be an invisible live target —
  which is the thing the whole module exists to prevent. Same division
  `ScrollSpan` and `Clip::place` keep: the rule is testable without a window,
  and `ui.rs` only paints it.
- **A selection has four boolean modes, and the boolean happens on the
  coverage.** `max` for Add, `min(a, 255 − b)` for Subtract, `min(a, b)` for
  Intersect — `max`, `min` and the complement, so intersect is difference's twin
  rather than a fourth idea. Polygon boolean geometry is the alternative and is
  a large, bug-prone algorithm for a result the mask already has exactly.
  **The bounding rectangle moves differently for each and getting it wrong is
  silent**: Add unions, Intersect takes the *overlap* — not either operand's own
  rectangle, because `blit_into` only visits the overlap and a mask sized to the
  larger one leaves this selection's coverage standing everywhere the other does
  not reach, which is a union wearing intersect's name — Subtract keeps this
  one's, and every result is trimmed to what it actually covers. **Intersecting
  with no selection is the shape**, where subtracting from it is nothing: no
  selection means the whole document.
- **A feather is a blur of the mask with the rings left exactly where they
  were**, which is the opposite of the decision a boolean forces and right for
  the same reason. The kernel is symmetric, so the 50% contour of the softened
  mask *is* the sharp edge it was blurred from — the marquee and `contains` keep
  reading the exact geometry rather than a staircase traced back out of pixels.
  **The radius is a field on `Selection`**, because rings cannot describe a soft
  edge and Umber rasterises them again twice — `Selection::flipped` and
  `Editor::carry_selection` — so both re-apply it or a canvas flip quietly
  hardens every soft edge in the picture. A boolean records the **larger** of
  the two radii, the only answer that cannot harden an edge that was soft. **And
  both re-raster sites keep the hard mirror where the radius dissolves the
  rebuilt shape**: a boolean's traced rings can be small while its recorded
  radius is wide, and deleting somebody's selection because they flipped the
  canvas is unrecoverable, since undoing a flip is another flip.
- **The feather kernel is a tent, two box passes per axis, and "per axis" is the
  whole of it** — a tent is the box convolved with itself and convolution is per
  axis. Running sums make it linear in the area whatever the radius; every
  partial sum is an exact integer, so the only rounding is one store per pass
  and a rectangle comes out exactly mirrored about its own centre; and being
  separable over a mask that is itself separable, an axis-aligned rectangle
  keeps the exactness the fill rule gives it. **A radius of zero is the exact
  identity** — same bounds, same bytes, no allocation. Outside the canvas counts
  as unselected, so a selection against the document edge fades at it, as
  Photoshop's and GIMP's do.
- **A feather makes the lift's third case the ordinary one.**
  `transform.wgsl`'s `min(a, m) / a` takes content softer than the mask *whole*;
  against an antialiased edge that is one pixel, against a feather it is a band
  `2 × radius` wide, so a soft wash lifted through a wide feather comes out
  unfeathered wherever it is fainter than the ramp. Nothing changed it and
  `a_lift_still_splits_paint_the_selection_did_not_make` refuses the tempting
  over-correction — but it is now the common case rather than a curiosity.
  `a_lift_through_a_feathered_selection_splits_the_alpha_it_finds` fills the
  layer flat rather than painting through the selection it lifts through: paint
  made through the mask has `a == m`, so the share is identically one and the
  test would pass under any rule.
- **The four operations are on the tool options strip as well as on the
  modifiers**, and `App::combined_selection_op` is the one place the two meet —
  Shift adds, Ctrl (Cmd) subtracts, both intersect, nothing held takes the
  strip's setting, so a modifier overrides one gesture rather than changing what
  the strip says. A free function for the reason `gesture::press` is one, and in
  `app.rs` rather than `umber-core` because *which* modifier means which has to
  be reconciled with what Alt already does on this canvas. **Their fills must
  not overlap** — the tint is a fraction of the ink so two `rect_filled`s
  composite where they cross, and Add drew Intersect's darker lens until the
  union became three disjoint rectangles.
- **The feather rail is `widgets::inline_slider` and applies to the next
  gesture.** Its figure can be typed now — `inline_slider` is `typed_rail`'s
  one-line shape rather than a second control — so the sentence that used to
  stand here, that the strip's figures could not be typed, no longer holds. What
  has not changed is that it sets what the next shape will be rather than
  softening the one standing, which is every application's spelling of a
  tool-options feather — and
  re-applying it live is not merely expensive but lossy, since the sharp mask is
  gone after a boolean. **A budget on that strip must be measured where it is
  spent**: the combine line reads `available_width()` afresh, *after* the mode
  hint, because that hint is drawn unconditionally and
  `SelectionMode::Polygon`'s is eighty-four characters.

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
- **A lift is a `min` against the selection, never a multiply.** Painting is
  clipped by the mask in the dab pass, so a pixel the selection half covers
  *already* holds half a stroke's alpha; scaling that by the coverage applies
  the mask a second time and carries a quarter into the float while leaving a
  quarter on the layer. That is a one-pixel ghost of the outline behind every
  lift, and it is exactly what it looked like to the artist. `transform.wgsl`'s
  `fs_mask` takes the share as `min(a, m) / a` instead: of the alpha that is
  there, the part inside the selection is at most the coverage, and this takes
  it to be exactly that. Opaque pixels lassoed out of a picture are untouched by
  the change — `min(1, m)` is `m`, the old behaviour and the old antialiasing —
  and content softer than the mask keeps its *own* falloff rather than having
  the mask's multiplied into it. One number drives both passes, the float scaled
  by the share and the hole by its complement, so the two cannot disagree about
  where the paint went, for the reason `render_float` is one function called
  twice. It needs the **layer's own slice** bound for sampling, because the
  share is a share of what is there and neither of the passes' own targets may
  be read while it is a colour attachment; the layer is untouched until the
  commit, so it is the one pristine copy both can share.
  `a_lift_leaves_no_ghost_of_the_selection_it_was_painted_through` is the guard,
  and `a_lift_still_splits_paint_the_selection_did_not_make` refuses the
  tempting over-correction of simply taking everything the mask touches.
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
- **A negative `scale` is a flip, and `MIN_SCALE` clamps magnitude only.** It
  used to clamp positive, on the reasoning that a flip was a feature with its
  own controls rather than something a hand should stumble into; there were no
  such controls, and dragging an edge through the opposite one is how every
  other application spells this. Nothing downstream needed a branch — the matrix
  stays invertible with a negative determinant, an affine map keeps adjacent
  corners adjacent so `quad` cannot become a bow tie, the anchor formula is
  sign-agnostic, and `is_identity` correctly reads a flip as a change. The one
  thing that did: the uniform-corner branch must compare *magnitudes* and
  `copysign` per axis, or a Shift-drag past the corner hands the un-flipped
  axis's sign to both.
- **`Transform::grab` reads everywhere outside the box as `Handle::Rotate`, and
  therefore never answers "nothing".** Rotation used to be a ring around the
  corners alone, which is a target most people never find. "Click outside to put
  it down" could not stay in core once that changed — `grab` sees one position,
  and the difference between a click and a drag is not in it — so it is the
  pointer layer's: an outside press turns nothing until it has travelled past
  `PUT_DOWN_SLOP`, and the release commits only if it never did. `drag` being
  absolute against the press rather than accumulated is what lets the rotation
  wait without losing the degrees it spent waiting.
- **The rotation mark is instructive, the flip buttons are real.** The mark is
  placed off `handle_at`'s own answers rather than the screen bounding rect, so
  it turns with the box and swaps sides on a flip and cannot be drawn where the
  hit test disagrees with it. The buttons are the opposite case — controls over
  the canvas — so their rectangles go in `Editor::transform_buttons` and through
  `canvas_overlay_owns_pointer`, exactly as `Editor::scroll_bars` does, or a
  press on one also starts a stroke.
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
  clipboard arrives the form it wants is already what is held. Masking **bounds
  alpha, on the straight side** — two rules and each was a bug. `min(alpha,
  coverage)`, for the reason the lift is a `min`: a multiply applies the
  selection to paint the dab pass had already clipped by it, so a copy of an
  antialiased edge came back at a quarter of the mark it was taken from, and the
  exact-inverse promise above was false for anything painted inside a selection.
  And on the *straight* side: scaling the stored bytes is wrong by a gamma curve
  and invisible on anything opaque.
- **`Clip::place` decides what a paste does**, in `umber-core`, because "where
  does the picture go" is a rule and rules are testable without a window:
  centred on the selection or on the view, nudged back on where it fits,
  **centred and cropped** where the clip is larger than the canvas — and the
  crop is said out loud, because a floating region lives in canvas-sized storage
  and there is nowhere to hold the overhang.
- **The marquee travels with the picture**, at commit, by transforming the rings
  and rasterising again. Nothing in `selection.rs` was changed for it. Only for
  a lift: a paste did not come out of the selection.
- **A cut is a copy and its exact complement, from one pass over one buffer.**
  `Clip::cut_from_layer` returns both halves and `from_layer` is that same
  function with the second not built, so the two cannot disagree about the edge
  of a soft selection. The share that leaves is **subtracted** from the alpha
  that was there rather than computed as `alpha × (1 − coverage)`: both sides
  round to nearest, so an independent removal leaves a rim of the edge in the
  copy *and* on the layer — the ghost outline a masked lift used to leave.
  `taken + left == before` therefore holds byte for byte, and
  `a_cut_takes_exactly_what_it_leaves_behind` drives every (alpha, coverage)
  pair through it.
- **One readback serves the clipboard, the write-back and the undo patch**,
  because the bytes read *are* the pre-cut state of the rectangle. There is no
  second blocking read, and no new pipeline: the removal is a `write_layer_rect`
  of what `umber-core` worked out.
- **A cut records `EditKind::Erase`, and that is not a placeholder.** It removes
  coverage and undoes by putting a rectangle of pixels back, which is what an
  eraser stroke is and undoes as — and two rows that undo identically must not
  have two names, the same rule that keeps a paste filed under Transform. So no
  new variant, no new `panels::edit_icon` arm and no `history::VERSION` bump.
- **A copy, a cut and a paste all put a float down first**, which is one rule
  rather than three and is the rule every path that leaves the document already
  follows. Once committed those pixels *are* the document, so `take_region` —
  the one place either command decides what it reads — needs no special case
  after it, and deliberately has none. A lift carried the marquee with it at
  commit, so the selection is already over the right pixels.
- **A float that arrived by *paste* is the case to leave alone.** It did not
  come out of the selection, so afterwards the marquee names somewhere else and
  a copy answers "nothing to copy". Reading `Transform::dest_rect` instead is
  the obvious repair and is **wrong**, in a way that only shows up on the cut:
  that is the bounding box of the *quad* plus a skirt, and a rectangle is not
  the shape of the picture — cutting it with no mask clears the corners a
  rotation left over, whatever showed through the clip's own transparency, and
  the skirt. Silent damage to the layer, in one entry, and worse than a Ctrl+C
  that does nothing; the clipboard already holds what was pasted. Putting the
  case back needs the clip's **alpha** as a mask, not its bounding box.
- **A cut is gated on the lock once**, in `cut_selection`, and the button is
  *disabled* to match — the rule "Clear layer" already follows, so the gate
  catches a keystroke rather than being the only thing between a live control
  and a dialog. A copy is not gated at all, because it writes nothing.
- **The cut's patch is the rectangle, not cells.** There is no `TileMask` to
  have accumulated one from, so with nothing selected a bare Ctrl+X on a 10000²
  canvas is 400 MB and the budget holds exactly one. Same rule as a stroke
  across such a canvas, and said out loud for the reason the Undo section gives.

#### The desktop's clipboard

`umber-app::sysclip`. In `umber-app` for the reason `update` is: `umber-core`
and `umber-render` may not learn about the platform, which is the boundary that
keeps them testable without one. `umber-core::clipboard` was **not changed at
all** — the straight-alpha sRGB RGBA8 it already held is exactly
`arboard::ImageData`, which is what that choice was made against.

- **`sysclip::decide` is the whole rule and is a pure function of three
  readings** — what the desktop holds, what `Editor::clipboard` holds, and what
  the desktop should hand back for Umber's own clip — for the reason
  `install::detect` is a pure function of a `Probe`. **No test may touch the
  real clipboard**: a CI runner may have no display server at all, and a test
  that grabs the desktop's clipboard on somebody's machine is hostile.
- **A picture on the desktop wins, unless it is the one Umber put there or
  Umber's copy never got there; with no picture on the desktop, Umber's own clip
  lands.** Every clause was needed. The first makes a paste respect the machine
  it runs on. The second keeps the copy-and-paste-back exactness true once a
  copy leaves the process. The third was a **silent bug**: a write can fail — a
  Windows global allocation refusing a large picture — leaving the desktop
  holding what it held *before* the copy, and believing that put down a picture
  the artist never copied. There is no ordering to appeal to; Umber can know
  whether its own copy arrived, never when the desktop's did.
- **The third reading is a picture, not a flag, and `TRANSPORT_IS_EXACT` decides
  whether one is taken.** A platform whose clipboard does not hand back the
  bytes it was given holds something that is not Umber's clip and is nonetheless
  Umber's copy; comparing against the clip would believe the desktop and paste
  mangled bytes. So a write on such a platform keeps the **echo** — what came
  back — and compares against that. Windows and Linux pay nothing: no read, no
  second copy. An echo that comes back *equal* is dropped rather than held
  beside an identical picture, which is memory and **not** a promotion: the next
  copy still echoes, because a premultiply is the identity on anything opaque
  and one agreeing picture says nothing about the next. It is a `const` and not
  a `cfg` so the echo compiles everywhere, which is the only check on it anybody
  here can perform. **Nobody working on Umber has a Mac and no part of that path
  has ever been run**; `examples/measure-clipboard.rs` settles it in one run,
  and its exactness sweep covers every alpha deliberately, because a premultiply
  would hide in a sweep of solid colours.
- **A mismatch is `warn` only where no echo was taken.** Where one was, a
  mismatch just means somebody copied something else — it happens on every
  foreign paste. Where one was not, a same-sized picture that is not ours is the
  one reading that says the constant is wrong for this machine, and it is the
  *only* detector for that, so it must not be quiet. Losing this was a real bug
  in the first draft of the echo.
- **A picture off the desktop is an ordinary `Clip` and there is no second
  placer.** `Clip::place` decides where it goes exactly as for Umber's own,
  which matters more here: a screenshot is usually larger than the canvas, and
  that is the case the crop notice already covers. It is adopted into
  `Editor::clipboard` **only once `begin_float` has accepted it** — adopting up
  front threw away the region the artist had copied whenever the paste was
  refused for a lock or a folder.
- **Nothing is threaded, and the paste is what decides it.** The copy's argument
  is the export's; the paste's is stronger — a write still in flight when the
  next Ctrl+V reads the desktop would leave `decide` looking at the previous
  picture.
- **Numbers here are `examples/measure-clipboard.rs`'s, and re-running it is not
  optional.** The first figures written into these docs were three times too
  slow because the machine was building six other things at the time. About
  2.5 ms/MB to write and 2.1 to read; Ctrl+C with nothing selected on a 10000²
  canvas is roughly a second, doubled where an echo is taken, on top of the
  readback it already paid for. **Nothing on screen says so**, and that was
  decided rather than skipped — a progress bar over a blocking call that reports
  none is the lying control `Stage::progress`'s `Option` already refuses.
- **Text is egui-winit's**, which is why its `clipboard` feature is on. It
  already maps Ctrl+C/X/V and Shift+Insert — including the non-Latin-layout
  fallback — to egui's own events; restating any of that would be a second copy
  of egui's rule in a codebase that refuses to reimplement a caret. `links`
  stays off, so `about::link_row` still paints its own hyperlink.
- **What Umber copies belongs to Umber while it runs.** On X11 and Wayland the
  contents are served by the process that put them there, so closing Umber
  empties the clipboard unless a manager took the handover arboard offers on the
  way out — which is why every exit must stay an `event_loop.exit()` and never a
  `process::exit`. `SetExtLinux::wait` is **refused**: it serves requests until
  somebody else copies, which for an application that stays open means blocking
  the thread that draws.
- **The Linux packages gained no shared library, and that was checked rather
  than assumed.** `x11rb` speaks the protocol over the socket and links no
  libxcb; `wl-clipboard-rs` and `smithay-clipboard` reach only
  `libwayland-client.so.0`, which winit already opens. `tree_magic_mini` would
  want `shared-mime-info` at runtime and is deliberately not declared, because
  it is reachable only from `MimeType::Autodetect` and arboard names an explicit
  type on every call — **an arboard bump has to re-check that.**

### Text

`umber-core::text` sets it, `umber-core::fonts` finds the faces, `textpanel.rs`
draws the module and `cputext.rs` is the splash's own use of the same `Pen`.

- **Placing text is literally a paste, and that is the whole design.** The set
  lines go through `Clip::place` and `begin_float` — the same two Ctrl+V uses —
  so the transform tool's handles move, scale, turn and flip it, Escape abandons
  it, and one undo takes it back off as an ordinary `EditKind::Transform`. No
  new float kind, no new undo variant, no second placer.
- **Text *is* kept now, and `umber-core::textobj` is the record.** This bullet
  used to say the opposite — "the string, the face and the size are recorded
  nowhere" — and that was true and deliberate for as long as placing text was
  only a paste. A `TextObject` is the string, the face, the colour and the
  placement, written into the `.ora` under `umber/text/` and named by
  `umber-text`: the shape `umber-mask` already uses, and for the same reason,
  that `stack.xml` must not carry a paragraph of somebody's prose with XML
  metacharacters in it. Placing text is *still* a paste; the record is what sits
  beside the pixels afterwards.
- **`umber-version` did not move for it, and the fingerprint is what makes that
  honest rather than convenient.** An older build ignores the attribute, decodes
  the ordinary PNG and shows the identical picture — plainer, not wrong. It can
  also *paint* on that layer and save, leaving a record beside pixels it did not
  make, and re-rendering would destroy the brushwork. So the record carries the
  rectangle its layer image occupies and a hash of its bytes, and **on load a
  mismatch discards the record and keeps the picture.** The same sentence a saved
  history lives by. `a_text_layer_painted_on_by_an_older_build_opens_as_paint`
  drives the *hash* half rather than the cheaper rectangle half, deliberately.
- **The fingerprint belongs to the file and never to the session.** The writer
  takes it from the bytes it is writing and the reader checks it against the
  bytes it read, so a `TextObject` in memory carries none, nothing can go stale
  and no readback is needed after an edit. What is hashed is the **trimmed
  straight-alpha image**, not the canvas-sized layer buffer: `trim` drops fully
  transparent pixels, so a premultiplied `(5,5,5,0)` would come back as zeroes
  and a fingerprint over the buffer would refuse a document nobody had touched.
- **A text layer is a layer that carries a record, not a third kind beside a
  folder.** `docs/text-tool.md` §3 called it a layer *kind* and "a model change
  of the same size as folders were"; it is not, and that shape is why nothing
  else changed. It holds a slot, composites through the same pass, takes a mask,
  clips, links, reorders, and travels into a structural undo entry with its
  record inside it — so a folder deleted with text in it parks both and one undo
  brings both back, with nothing written to make that happen. `LayerStack::MAX`
  means what it always meant. Layer effects reached the same conclusion
  independently, which is what makes it more than a convenience.
- **Painting on one is refused at `LayerStack::refusal_at`, one gate with one
  reason.** It subsumes the two tests `begin_stroke` already made — a lock and a
  folder — and adds text, and it fails **closed** on an index off the end.
  A stroke on a text layer's **mask** is allowed, because a mask bounds the alpha
  the composite reads and changes no layer pixel, so it cannot put the record out
  of step. **"Clear layer" is deliberately not refused** — it means to replace
  the pixels, so it must take the record off instead, which is also the whole of
  "convert to paint" and records no undo entry because no pixel changes.
- **A canvas flip mirrors the placement; a resize drops the record.** The mirror
  is exact, because `diag(-1,1)·R(θ)·diag(s)` *is* `R(-θ)·diag(-sx,sy)` — so a
  flip costs nothing, which matters because undoing a flip is another flip and a
  dropped record could never come back. A resize drops it for the reason a resize
  clears the history: the placement is a rectangle of a canvas that has gone, and
  a shrink has cropped the pixels. Translating by the anchor offset is exact for a
  *grow* only, and two behaviours behind one command is how the cropping case ends
  up untested.
- **A missing font freezes and never substitutes.** `TextFace::resolve` asks for
  the exact family and style and refuses; `FontLibrary::resolve` is deliberately
  *not* what does it, because that one is total by construction and would
  re-render somebody's caption in a face its author did not choose. The
  PostScript name is recorded for the notice and is **not** a lookup key, because
  `Face` carries none. **Embedding the font in the `.ora` is refused**: it is
  redistribution performed by the artist without their knowledge, in a file they
  may email, and for a machine-licensed system font it is a licence breach they
  did not commit.
- **The record has a size bound of its own**, and it may **not** share the
  effects one. `MAX_EFFECTS_BYTES` is *derived* — one effect per kind,
  `MAX_ENABLED` per document — so an over-long record is unwritable and the bound
  is needed on the reading side alone. A text record cannot be derived: what
  bounds a block is the area it renders to, not how much somebody typed, so a
  legal block can outrun any figure. What the two share is the rule for **where
  the figure lives: with whichever side can violate it.**
- **No new `EditKind` and no `history::VERSION` bump.** A text edit puts a
  rectangle of pixels back in one place, which is what a paste already does, so
  it is `EditKind::Transform`. What it *also* restores is the record, which
  belongs in an `EditBody` arm rather than a kind — `EditBody` is already where
  the flip's difference lives. Not written to the file: a reopened undo restores
  the pixels alone, which the next save's fresh fingerprint makes safe.
- **The writer is wired, and this bullet used to say it was the live gap.** Both
  ends are there now: `app.rs` fills `SaveLayer::text` off the layer and
  `autosave.rs` off its snapshot, for the reason each takes `effects` from the
  same place — a save reads the stack it is looking at, an autosave's pixels
  arrive over several frames and its metadata is snapshotted when the capture
  begins, and the two have to write the same file. **`text` is the field whose
  absence is silent**: `..SaveLayer::new` writes `None`, so a writer that forgot
  it would turn a document that opened as text back into plain paint with
  nothing said. That is why the guard counts the literal at every construction
  site rather than trusting the two to agree.
- **Bold and italic are a family's own faces or they are nothing.**
  `FontLibrary::restyle` answers with a real face of the family, and its `None`
  *is* the feature: Umber never smears an outline to make a bold and never
  shears one to make an oblique, so where a family has neither the mark is
  **disabled with a sentence**. `Lacking` has five and each says what is
  actually missing, because `can_restyle` answers about *this slant on one side
  of the weight* — and sentences written as though it answered about the whole
  family told somebody to install a bold that was two rows above it in the list
  beside them.
- **Which rasteriser honours variation axes was misread for months, and
  `Face::variations`' comment is what misread it.** `umber-core::text` draws
  through `skrifa` with `DrawSettings::unhinted(…, &location)` and shapes with
  `harfrust`'s `ShaperInstance::from_variations`, so it **does** instance a
  variable font; `cputext.rs` takes the same route. `ab_glyph` is what ignores
  axes, and it is **egui's** — so "the interface has no bold" was always a
  statement about the interface and never about the text tool. Read as the
  latter it says bold is impossible, which is why nobody built it.
- **What lights the Bold control is `is_bold_anchor`, never `Face::is_bold`.**
  `is_bold` is `BOLD_THRESHOLD`'s partition and Archivo has four upright faces
  above it, so lighting from it said SemiBold *was* bold — and a lit mark asks
  for the regular weight, so no press ever reached Bold from SemiBold, ExtraBold
  or Black. The anchor asks whether the bold you would be given is the one you
  already have. `restyle`'s target keys on whether the **slant** moved for the
  same reason; keying on the boldness half handed SemiBold straight back.
- **A guard on a model is not a guard on the panel**, and this is the
  generalisable one. `every_weight_of_a_family_reaches_its_bold_in_one_press`
  tests `is_bold_anchor` and cannot see whether the panel *calls* it — a critic
  reverted that one call site and all 1,485 tests stayed green. What catches it
  is `pressing_bold_actually_puts_a_heavier_mark_on_the_canvas`, which measures
  ink. **Any test of a two-state reading has to start from a case where the two
  readings disagree**, or it is testing the reading it happens to like.
- **A style name identifies one face of its family.** `insert` refuses a
  duplicate on the name alone, stricter than its sort key, because `exact`
  answering with whichever sorted first let `restyle` hand back a name that
  resolved to a different face — and drew two identical picker rows, both
  highlighted.
- **A face recorded with no `variations` has stated no axis position, and that
  is not agreement.** A variable font's default master is recorded that way
  deliberately; read as "agrees with every width" it let a condensed face be
  handed the wide default. `axis_agreement` has three answers and ranks unknown
  between them.
- **The preview's colouring was a second copy of `Setting::clip`'s arithmetic
  and had drifted**, using coverage as alpha where `clip` multiplies by the
  colour's own alpha. Nothing produces a translucent `Editor::color` today,
  which is exactly why it would have gone on being wrong — the same reason a
  fixture carrying one value is not coverage. It routes through `Setting::clip`
  now.
- **A loss is named in the panel, not discovered.** Lines break only where the
  artist breaks them, a line mixing left-to-right and right-to-left writing is
  shaped but not reordered, and a character the face has no glyph for is left
  blank and named by codepoint rather than borrowed silently from another font.
  Same rule the importers keep: subtly wrong output is worse than a refusal that
  sends somebody somewhere else.
- **It is a module, not a tool**, and it is deliberately not in `DEFAULT_DOCK` —
  see `PanelKind::ALL` versus the shipped arrangement, and why adding to the
  second is the change that would need a version bump.
- **There is one `Pen`, in `umber-core::text`, and `cputext` uses it.** There
  used to be two, on the reasoning that the splash paints before `umber-core`'s
  consumers exist — false, since `umber-app` names `umber-core` as an
  unconditional dependency — and the copies had **already drifted**. Its `at`
  clamps two pixels inside the buffer, and what that buys was measured *after*
  the comment claimed otherwise: **nothing visible.** The displaced delta lands
  at `(0, row+1)` and `ab_glyph_rasterizer`'s prefix sum cancels it there, so
  the worst spurious pixel is 1.047 with the inset and 1.047 without. The
  artefact is a property of clamping a contour at all, not of where it clamps
  to. The **cost** is real — 22.9 of coverage truncated on Archivo's `g` at
  24 px in a 16x20 box against 0.06 un-inset — and is invisible only because
  both callers pad by a whole em. **If anybody tightens that padding, the inset
  is the first thing to reconsider.** `4f537c0`'s commit message states the
  false mechanism and cannot be amended.
- **`theme::text` and `umber_core::text` collide by name**, and the first is
  the font-size table used in about eighteen files under `umber-app`. Import the
  item, never the module: `use umber_core::text::Pen;` and never a bare
  `use umber_core::text`.

### Layers

Layers occupy slices ("slots") of one texture array, and the whole stack
composites in a **single pass** — `composite.wgsl` loops bottom to top. Do not
"simplify" this into a pass per layer.

- **A layer's slot never changes.** Stack order is the `Vec` order, so
  reordering is a pointer shuffle, not a texture copy. Anything indexing layers
  by position must not assume position equals slot.
- **`LayerStack::MAX` bounds stack entries; `MAX_DRAWS` sizes the uniform
  arrays; `MAX_SLOTS` is the device's and everything else is derived from it.**
  They were one number and are now three. `MAX` and `MAX_LAYERS` in `canvas.rs`
  are 64 and bound **entries**, folders included — still stricter than the draw
  array needs while folders are flattened away, and still deliberate, because a
  folder compositing as a group *will* occupy a draw. `MAX_DRAWS` is 191 and
  sizes the two `array<vec4<f32>>` in `composite.wgsl`, because **a draw is not
  a stack entry**: a layer's effects each composite as a draw of their own so a
  shadow at Multiply multiplies against the backdrop.
  **`MAX_SLOTS` is 256 and is not a choice.** `downlevel_defaults` never names
  `max_texture_array_layers`, so it inherits `Limits::defaults()`' 256, and
  `using_resolution` raises only the three texture dimensions — the trap being
  that it is right there raising limits from the adapter and looks as though it
  raises this one. A 257th slice is a validation error, which is fatal. The
  design asked for 257 and it would have shipped. So the ceiling is the *input*:
  64 layers, 64 masks and the float's spare take 129, and the 127 left are the
  effect budget — which is where 191 comes from, since an effect draw reads an
  effect slice. **127 rather than 128 is also what makes the cap reachable** by
  a legal document.
  Sitting exactly on the device's figure is safe **only because it is asserted
  against it**: `Limits::downlevel_defaults` is a `const fn`, so that is a
  compile error rather than a test, and the comment has to say why the limit is
  inherited rather than named. `the_three_draw_capacities_agree` pins the rest,
  and **pins the array *declarations* and not only the constant** — leave the
  constant right and write `array<vec4<f32>, 64>` and the WGSL struct is merely
  smaller than the buffer, which validates, and the composite then reads `extra`
  as `layers` past index 63. **Raising the array lengthens no loop** (bounded by
  `layer_count`); it costs 6,224 uniform bytes against 16 KiB. And note
  **raising `MAX_LAYERS` *lowers* the effect budget**, two slices a layer —
  which is caught, by `effect::BUDGET_DERIVATION`, as a compile error naming
  the reason. `canvas.rs` says "nothing fails when it happens" and that was
  true on the branch it was written on and is false now.
- **Deleting a layer parks its slice rather than recycling it**, which is what
  lets the undo history survive a delete. A `PixelPatch` names a slot, so a
  patch recorded against a *freed* slot would be replayed into whichever layer
  inherited it — the history used to be cleared for exactly that reason. The
  deleted layer now moves into the undo entry and owns its `SlotClaim`, so the
  slice cannot be reissued and no patch stops meaning its own pixels. No copy,
  no readback, no GPU work: the pixels never move at all.
- **A parked slice is charged to the undo budget**, and the design said it cost
  nothing until a critic proved otherwise. `slot_capacity_needed` is one past
  the highest slice ever *claimed* and `ensure_slots` never shrinks, so enough
  delete-then-add cycles take the layer array to its ceiling and leave it there
  — at `MAX_SLOTS`'s 256 that is 4.29 GB at 2048² and **102.4 GB at 10000²**,
  with the budget reporting kilobytes.
  `StackShape::byte_len` puts a parked slice in the same currency as a patch,
  which is what makes eviction able to reach it.
- **A recycled slot still holds the old layer's pixels** — clear it on the GPU
  when a new layer takes it.
- **A mask is another slice of the *same* layer array, not a second
  `R8Unorm` one.** It costs 3 bytes a pixel on a texture most documents never
  allocate, and buys that a mask *is* a layer to `read_layer_pieces`,
  `PixelPatch`, `resize`, `flip_layers` and the autosave capture — where a
  dedicated array would have meant its own banded readback, resize, flip,
  capture, patch width and history revision, six paths duplicating six that
  exist. A layer without a mask allocates nothing and fetches nothing: the
  sample is behind a **uniform** branch on `has_mask`, which is legal because
  `textureSampleLevel` takes no derivatives. `MAX_SLOTS` is therefore neither
  `MAX_LAYERS` nor `MAX_DRAWS`; the second is what sizes the uniform array.
- **Removing a mask parks its slice too**, for exactly the reason deleting a
  layer does: the slice would otherwise go back on the free list, and a patch
  naming a freed slot would replay into whatever inherited it. Both go through
  the same entry, and neither clears the history any more.
- **`StrokeStyle::on_mask` is the one edit-target switch**, in the struct that
  already carries "the preview and the commit must be handed the same style",
  and `Editor::stroke_target` is the single place it becomes a slot — falling
  back to the layer where there is no mask, so nothing downstream ever sees an
  impossible state. The commit needs **no new pipeline**: a mask is an ordinary
  slice, so what `on_mask` decides is only where the preview blends, and that
  blend is written to match `commit.wgsl`'s on one channel.
  `a_stroke_on_a_mask_previews_exactly_as_it_commits` reads both.
- **A clipped run answers to the nearest unclipped layer below**, through one
  running `clip_alpha` in the composite loop, set from a layer's alpha *after*
  its own mask and its wet stroke. **A clipped layer at the bottom shows
  nothing.** Starting that accumulator at 1.0 would make the flag mean something
  different depending on where the layer sat.
- **A lock is refused at one gate per operation, never at the call sites** —
  `begin_stroke`, `begin_float` (lift and paste both), `clear_active_layer`,
  `delete_layer`, `mirror_document`. A canvas flip is refused **whole** when any
  layer is locked: a half-mirrored picture is not a state the flip's pixel-less
  undo entry can describe. A paste onto a locked layer raises a notice; a canvas
  press does not, or the pen going down would be a dialog.
- **Linking carries a set through the stack, and deliberately not through a
  transform.** `Float` holds one layer's pixels, one base, one bind group, and
  `EditBody::Pixels` holds one patch — so moving several at once needs N of each
  *and* an entry holding several patches, or an undo would step through a
  multi-layer move one layer at a time and leave the document in states it was
  never in. The README says so rather than the flag half-working. **Groups did
  not change this**: several sets that each travel through the stack is still
  reordering, which is a `Vec` shuffle.
- **A link is a *group*, and the group is bounded by the colours.** `Layer::link`
  is `Option<u8>` and `LayerStack::LINK_GROUPS` is 6 because
  `theme::Palette::link_colours` is 6 — a group is told from its neighbours by
  the colour of the chain on its rows, so a seventh would be a mark that lies
  about which layers travel together. Asking for one is refused with a tooltip
  saying so. `free_group` hands back the **lowest** free number, so unlinking
  returns a colour to the pool rather than walking off the end of it, and
  **`unlink` and `remove` both dissolve a group that has fallen to one
  member** — `link` refuses to make a group of one, so nothing may leave one
  behind either, and a lone member would draw a chain meaning "moves together
  with nothing" while holding a colour `free_group` could then never return.
  `Palette::link_colour` takes the number modulo the table as a third line of
  defence, after the model's refusal and the ORA reader's filter, because the
  alternative failure is an index panic on the drawing path. The link colours
  are checked for separation from **each other and from all four accents** in
  both themes; checking only the authored accent is what shipped a green a hair
  from `Accent::Sage`.
- **The chain lives in the ticked strip and nowhere else.** Linking is the one
  thing on a layer that is a statement about several layers at once — a group of
  one says nothing, which is why `link` refuses fewer than two — so a chain in
  the per-layer flags row would have to mean "link this to what?". One button,
  which unlinks when `shared_group` says the targets already are one set and
  links otherwise, because two buttons would be two spellings of one question.
- **`umber-link-group` did not raise `umber-version`, and `umber-link` is still
  written beside it.** A link changes no pixel — it decides what travels with
  what when a layer is dragged — so a build that reads only the old flag shows
  the same picture and merely has one set where this build has three, which is
  exactly what that build did with the file it wrote. A file with the flag and
  no group reads as group zero: the single set it was written as.
- **Nothing here clears the undo history any more, and reordering never did.**
  The difference was always whether a slot changes hands — a `PixelPatch` names
  one, and only a delete freed one for the next layer to inherit. A delete now
  parks the slice instead, so neither does. `LayerStack::reorder` is the whole of
  it, and `move_up`/`move_down` are written in terms of it so the rule that the
  selection follows the *layer* rather than the position exists once instead of
  in three places that have to agree.
- **The drag that reorders the list is a model, `layerdrag.rs`, with no drawing
  in it** — the same division `brushdrag.rs` keeps. It has **two** hit tests and
  needs both: a press must land strictly inside a row, where a drop rounds to
  the nearest, because the opacity slider sits directly above the list in the
  same column and a press that rounded would turn dragging the slider into
  dragging the bottom layer. Rows are read by their carried index, never by
  their position in the slice, because the panel draws the stack upside down.
- **The in-progress stroke blends inside the stack**, at the active layer's
  position, not over the finished composite. Otherwise painting beneath a
  Multiply layer previews wrongly and jumps on release.

#### Folders

A folder is an **entry in the same `Vec`** carrying no slot, and its contents
are the contiguous run immediately below it whose `depth` is greater.
`docs/layer-folders.md` has the whole argument; these are the rules.

- **A folder sits *above* its own contents**, and this is the one thing here
  that is easy to get backwards. It is what a layers panel draws, it is the
  order ORA's nested `<stack>` writes — the first element of a stack is the
  uppermost — and it makes the folder the natural *end* of its group as the
  composite walks bottom to top, which is where a "close the group" marker will
  have to go. `LayerStack::subtree` is the one place containment is computed;
  everything that moves, deletes, hides, locks, ticks or draws a folder's
  contents asks it.
- **Every folder is pass-through, and that is the whole of why nothing else
  changed.** A pass-through folder is exactly its contents composited in place,
  so `composite.wgsl` was not touched, the four things that reuse that pass
  needed nothing, and `umber-version` did not move — an older Umber, or GIMP,
  flattens the nesting and shows the identical picture. Only visibility and
  locking fold in, because both are booleans and `hidden ∧ anything = hidden`.
  **An opacity does not fold**: a folder at 50% over two overlapping children is
  not two children at 50% each. So a folder has no opacity and no blend mode,
  and their controls are *not drawn* — not drawn disabled.
- **`Editor::layer_draws` is where folders are flattened away**, and therefore
  **a draw's position is not a stack position**. Anything handing the composite
  a stack index must map it through `Editor::active_draw_index`, which answers
  `u32::MAX` for a selected folder — deliberately not "the layer below it",
  which is a real draw and would preview the stroke on a layer nobody chose.
- **`Layer::slot` is an `Option` because a folder holds none.** A slot a folder
  held and nothing wrote to is a lie the autosave would find — it reads every
  slot back, so the file would gain a blank layer nobody made — and it would
  cost 400 MB of texture per folder on a large canvas.
- **Folders cost the undo history nothing**, and the rule is the one reordering
  answers to: no slot changes hands. Grouping, re-nesting and folding free none
  and reassign none, so every recorded patch still names its own pixels.
  *Deleting* a folder deletes its contents, and their slices are **parked** in
  the one undo entry rather than freed, for exactly the reason deleting one
  layer parks its own — so a folder deletion is undoable whole, which is the
  only shape it could take. The lock gate is over the whole subtree, because
  half a deletion is not a state to leave a stack in.
- **The history's stack positions count folders**, and the manifest's name
  fingerprint is built the same way, so the two cannot disagree about what
  "layer 3" means. A folder holds no slot so it can never be what a patch
  resolves to; what it does is occupy a position, which is exactly why it has to
  be counted. A position that names a folder drops the whole history.
- **Every operation a control offers has a `can_` beside it, sharing its plan.**
  `plan_reorder`/`can_reorder`, `plan_group`/`can_group`, `can_remove` against
  `remove_many`. The drag, the Group button, the chevrons and both bins draw
  themselves from those, so a control cannot light up promising something the
  model will then decline. `reorder` and `group` are judged on the depth
  sequence they *would* produce — `well_formed` over the whole projected
  stack — so neither can invent a layer nested inside nothing and a refusal
  changes nothing at all.
- **A set of entries is deleted in one pass, never a loop of single deletes.**
  A folder's contents sit *below* it, so removing one shifts every index
  beneath it, including ones a backwards walk has not reached. That is not a
  hypothetical: `delete_picked_layers` was a reverse loop and it deleted a layer
  nobody ticked, then cleared the undo history so it could not be taken back.
  `LayerStack::remove_many` resolves the whole set before anything moves, and
  `App::delete_entries` is the single gate both delete commands go through.
- **`MAX_DEPTH` is enforced in `umber-core`**, not hoped for: the eventual group
  stack in the fragment shader is a fixed-size array and a document too deep for
  it has to be refused where somebody can be told. An import too deep is
  *straightened* rather than refused — `flatten_ill_formed`, once, in
  `ImportedDocument::open` — because the pixels are all there either way and
  only the grouping changes.
- **Ticking a folder is written into the ticks, not derived at read time.**
  `LayerStack::pick` cascades down the subtree; `targets` is untouched. A
  painter who ticks a folder and unticks one layer means it, where a re-derived
  set would put that layer straight back — and it makes "a folder ticked whose
  contents are not" unreachable, so there is no third checkbox state to draw.
- **Collapsing is a filter on what the list draws and nothing else.** It is not
  in the file, for the reason a tick is not, plus one of its own: a fold that
  survived a save would be a state somebody had to undo before they could see
  their own painting. A collapsed folder composites exactly as an open one does.
- **`layerdrag` decides the depth and `LayerStack` decides the legality.** The
  pointer's horizontal position picks the nesting, one level per
  `metrics::LAYER_INDENT`, capped by what the target row can hold. A refused
  drop lights nothing up rather than falling back to a depth nobody asked for.
- **Past either end of the list is the top level, whatever `x` says.** A level
  is twelve pixels, so "beside the folder rather than in it" was a twelve-pixel
  target in the middle of a gesture — and in a stack whose every row is inside a
  folder it is the *only* way back out, which makes it the only way to a second
  top-level folder. Past the ends there is nothing to be inside of, so the
  answer is unambiguous and the target is the whole panel. Only *past* an end,
  never the end row itself, or a folder that happened to be topmost could not be
  dropped into. The model already permitted every one of these moves —
  `a_stack_entirely_inside_one_folder_can_still_reach_the_top_level` — so this
  was reachability alone, which is exactly the kind of bug the mark below hides.
- **The drop mark is a dashed outline stepped in to the depth it would land
  at**, not the selected row's own fill. Borrowing that fill made "the layer
  lands here" and "this row is selected" the same mark, and it could not say the
  one thing a drop with folders in the stack has to say — inside, or beside. It
  is `panels::drop_slot`, dashed in the accent, which is how this interface
  already spells "a place something is going" on the dock.
- **A folder's row draws a folder mark, not a thumbnail.** The honest thumbnail
  is the composite of its contents and is a third mode for `thumbnail.wgsl`; one
  arbitrary child would be a picture that lies about what the group holds.
- **The autosave snapshot has its own `pixel_index`.** A folder is read back as
  nothing, so the capture is shorter than the stack and a positional zip would
  pair every layer above a folder with the pixels of the one below it.
- **A tick is a field on the layer and is never written to the file.** Every
  other flag is a property of the picture; a tick says what the painter is
  *about to do*, and reopening a document to find four layers still ticked is an
  instruction nobody gave. A field rather than a set beside the stack, because a
  set would be keyed by slot and would then have to be kept in step with
  reordering and deletion by hand — as a field both come free, which is the
  argument `linked` already makes for itself. It is also not a document
  modification: a tick puts no dot on the tab.
- **`LayerStack::targets` is the one rule for what a *bulk* operation reaches** —
  every ticked layer, or the selected one alone when nothing is ticked. The
  fallback makes the rule total, so no caller has to special-case an empty tick
  list; note that no caller reaches it today, because the strip is only drawn
  when something is ticked. It is deliberately **not** the rule for the
  single-layer controls: the row's own eye and the flags row write their flag
  directly, because those are the controls that mean "this layer" and routing
  them through `targets` would make a tick somewhere else change what the eye
  in front of you does. `App::delete_picked_layers` is written in terms of
  `delete_layer` so the lock gate, the float being put down and the history
  being cleared are each stated once.
- **The ticked-layers strip is drawn only when something is ticked, and it
  shares the "All" box's line rather than having one of its own.** The tick box
  on each row is drawn whether or not anything is ticked, and so is the "All"
  box above them; the six buttons are the only part that comes and goes. A
  strip with a row to itself would still be right about *what* it draws and was
  wrong about what that costs: the box below it is always there, so appearing
  **inserted a line**, and ticking the first layer shunted the whole stack down
  under the pointer that had just ticked it — reported from a running window.
  Only the buttons appearing changes nothing's position.
  **The line's height is `metrics::LAYER_TICK_ROW` and is not taken from its
  contents**, because the chain is a 20 px `icon_toggle` and the box is
  `PICK_HIT`'s 18: a line sized by what is on it would be two pixels shorter
  with nothing ticked, which is the same jump made small enough to be
  mystifying. `ticking_a_layer_does_not_move_the_layer_list` is the guard and
  needs no GPU — it measures the body in all three states —
  and `layers_panel_preview` is what says it looks right.
- **"All" is a box at the head of the tick column, not words beside the
  buttons.** There used to be a "3 ticked" label and an All/None pair sharing
  that line with the six: they fit in the abstract and were drawn over each
  other at `metrics::PANEL`'s real 264 px. One box in the column it acts on says
  which control it is by *where it sits*, so it needs no label to be overdrawn,
  and it is drawn always — like the row boxes and unlike the buttons — because
  it is the way in to ticking rather than something you find after ticking. It
  ticks everything, or unticks it once everything already is, which is the pair
  in one control. Its geometry is `widgets`' `PICK_AT`/`PICK_HIT`/`PICK_MARK`,
  shared with `layer_row`, or the header drifts out of the column the first time
  a row is restyled — and `pick_all_box` therefore allocates **its own width and
  not the line's**, because the buttons right-align into what is left of that
  line. Its click is collected like the buttons' `Bulk` and applied after the
  line, so the ticks have one writer per frame: the buttons are drawn from
  `picked_count` and `targets` read *before* the line, and a `pick_all` landing
  half way through would leave the two disagreeing about what "the ticked
  layers" were. **It has three states and a folder's box has two**, and that is
  not an inconsistency: ticking a folder cascades, so "ticked, contents not" is
  unreachable there, while "some of the stack" is the ordinary case here and an
  empty box would say none was ticked.
- **A module's header lays its controls out before its title, and the title takes
  what is left.** Four marks and a close mark want 114 points; the header's
  control strip is 120 at `metrics::PANEL`, 83 at `limits::SIDEBAR_MIN_WIDTH` and
  38 at `metrics::TOOL_RAIL`. So at the design's width it fits and at every
  narrower one the strip overruns leftwards into the title — which is the
  "3 ticked" label and the six bulk buttons drawn over each other, one storey up.
  Truncating the title is the fix and it costs a word: in edit mode at 190 points
  "Palette" reads "Palet…". That is better than the full word with a button
  through it, and it only happens while somebody is dragging the panel. The guard
  sweeps **each kind's own `min_width`** rather than one constant — missing
  `PanelKind::Tools`' 100-point floor is the "domain the code sees" failure again
  — and carries the galley's **row count** as well as the two rectangles.
- **The Layers module's stack commands are in its header**, for the reason the
  Brushes module's Edit mark is: a panel body is a scroll area, and with a stack
  of any size the list fills it immediately, so the four commands that act on the
  stack were the first thing to scroll away. Group is bulk (`targets`); the
  chevrons and the header's trash mean *this layer*, because `reorder` moves one
  entry and the bulk delete already lives in the ticked strip. The add mark went
  to the **flags row** instead, because it acts on neither the selection nor the
  tick set.
- **The flags row wraps and the tick line does not, and the asymmetry is the
  point.** `metrics::LAYER_TICK_ROW` is a constant because the thing that changes
  there is *ticking*, whose pointer is on the list a jump would move. What changes
  the flags row is a mask being added or taken off, which is a press on a toggle
  that stays on the first line whichever way the row breaks. Holding it at two
  lines always would move the list for everybody to spare the one width where it
  wraps.
- **A thumbnail is the layer's *content*, and it is two passes because the
  bounding box of that content is on the GPU.** `thumbnail.wgsl` reduces a
  rectangle of one slice to a 64-square: first the whole slice to the
  **greatest** alpha per cell, which `umber_core::thumbnail::content_rect` turns
  into a document rectangle and `framed` into the region to draw; then that
  region to a **mean**, which is the picture. The first must be a maximum: a
  one-pixel line averaged over a 32×32 cell is 1/1024, which is zero in eight
  bits, so a mean reports every sketched layer as empty. `textureLoad`, not a
  sampler — a bilinear tap at 30:1 is a point sample with extra steps, and the
  region deliberately runs off the canvas where clamp-to-edge would smear the
  edge row across the margin. The frame never magnifies past 1:1, so a single
  dab reads as a single dab.
- **The invalidation rule is `CanvasRenderer::slot_revision`, bumped inside
  every method that writes a slice** — commit, float commit, `write_layer_rect`,
  clear, mask fill, flip, resize. Putting it inside the method rather than
  beside each of the eight call sites in `app.rs` is the "forgotten at the
  sixth" failure written out in advance.
  **This file called that "exhaustive by construction" and it was false for
  exactly one method.** `render_float` writes the float's *preview* slice every
  frame of a drag and bumped nothing, and nothing noticed because a thumbnail is
  never taken of the float's spare — so the one consumer that existed could not
  see the gap. Layer effects found it, because their cache is keyed on the slot
  the *draw* carries and a float swaps that slot. "By construction" is a claim
  about a set of methods somebody has to have enumerated; it is true now,
  `a_dragged_float_carries_the_effect_derived_from_it` is what holds it, and the
  lesson is that a rule enforced inside N methods still needs somebody to check
  that N is all of them. `Thumbs::wanted` is the whole policy —
  the active layer first, then stack order — and it is a model with no drawing
  in it.
- **"Nothing on this layer" is a cached answer, not a missing one**, or a blank
  layer is re-read on every frame for as long as it is open. It draws the same
  checker a picture that has not arrived yet does: distinguishing the two would
  put a spinner on every row of a freshly opened document.
- **The cache is keyed by document and empties itself when that changes**, which
  is what lets it live *above* the `--- documents ---` line — a slot is a slice
  of one document's array, so slot 3 is a different layer in every tab. And a
  slot that leaves the stack loses its picture, because slots are recycled.

#### Layer effects

`umber-core::effect` is the model and `docs/layer-effects.md` is the design.
**Stage 0 only: nothing bakes, nothing draws.**

- **`Outline` in code, "Stroke" in the interface.** `Stroke` is taken four times
  over — `umber-core::stroke`, `StrokeBuilder`, `StrokeStyle`, and four
  `stroke_*` fields of the composite's uniform — so no type, variant, field or
  function under `umber-core` or `umber-render` may spell a *layer effect*
  Stroke. The one place the interface's word appears is `EffectKind::label`, a
  string rather than an identifier, which is what makes the rule enforceable.
- **`Effect` is one flat `Copy` struct over every kind, not a variant per
  kind.** That settles what effects cost a `Layer: Clone` and a structural undo
  entry: a layer holds at most one per kind and each is a few dozen bytes with
  no allocation in it. The cost is a field a kind ignores, which is what lets
  the panel keep a shadow's angle while the row is switched to an outline and
  back.
- **`Layer::effects` is private and the invariant is the stack's.** At most one
  per kind, always in composite order, maintained in `LayerStack::set_effect`
  and `remove_effect`. Moving an outline from outside to inside moves its draw
  *across* the layer, so the order cannot be settled once when an effect is
  added; it is re-derived at the one gate that writes the field.
- **`plan_set_effect` returns the vector to install rather than a verdict**,
  with `can_set_effect` beside it — which makes "a refusal changes nothing at
  all" structural rather than disciplined. It refuses an index off the end, a
  **folder** (no coverage to derive from until group compositing lands), and the
  budget. It is deliberately **not** gated on the lock: an effect's parameters
  are a value on a layer exactly as its opacity is, and a layer's opacity is
  neither lock-gated nor undoable.
- **The cap governs *adding*; overflow is the draw path's.** `restore_shape`
  puts a deleted layer back with its effects and consults no budget, because an
  undo that refuses to undo is worse than a picture missing a shadow — and an
  import, an open and a layer leaving a folder can all arrive over budget too.
  So the draw path drops effects in a stated order and says the document is over
  budget. `undoing_a_delete_may_take_a_document_over_the_effect_budget` pins it.
- **`effect::MAX_ENABLED` is 127 and it is live in an ordinary document.** 64
  layers × 2 kinds asks for 128 and the last is refused. It was 128 in the first
  draft, from a `MAX_DRAWS` of 192 the device could not supply, and at that
  figure nothing could reach it — one lower is the whole difference between a
  guard nothing meets and a refusal somebody will. The derivation is a `const`
  assert and not a comment, because the failure is **directional**: raising
  `MAX` leaves the literal over the device's guarantee while only the harmless
  direction trips an explained assertion.
- **A flat struct over several kinds may not take a container
  `#[serde(default)]`, and this is the generalisable one.** `Brush` can, because
  it has one kind and one `Default` describes it. `Effect` cannot: filling an
  absent field from a whole-struct default meant an *outline* written before a
  parameter existed loading with the drop shadow's blend, softness and distance
  — a blurred, Multiply outline the artist never set, silently, on the day the
  parameter was added. Each field defaults to its own **neutral** instead, and
  `kind` has no default at all, so a file that omits it is refused rather than
  read as an arbitrary one. There is no `impl Default for Effect`.
- **A struct's *field names* are a format too, not only its variant names.** The
  rule already recorded for `BlendMode` covers half the mechanism. `Effect`'s
  ten field names are **destined for** `umber/effects/<n>.ron` — the writer is
  not built yet, so today the round trip is tests only, and the pin was put in
  before the format rather than after. A round trip is self-consistent under any
  rename, and the per-field defaults turn an unrecognised field into a
  **silence** rather than an error — so `#[serde(rename = "colour")]` on
  `color`, the likely rename in a codebase whose convention is British spelling,
  left every test green while an old file loaded with the colour gone. Pin the
  whole serialised text as a literal.
- **`Color` derives no serde and `effect::linear_rgba` is why it still does
  not.** A derive there would make its four field names a format for every
  future struct that happens to hold a colour, granted in advance to code nobody
  has written. Linear `f32` rather than `Swatch`'s sRGB bytes, because an
  effect's colour is painted with rather than stored — the same question the
  palette asked and answered the other way.
- **No `EditKind` variant and no `history::VERSION` bump.** An effect's
  parameters are a value on a layer with no pixels behind them, exactly as a
  layer's opacity is. `docs/layer-rename.md` is the standing design for the
  `EditBody` arm that would make a layer's *values* undoable; if it is built,
  effects join it.

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

- **A flip keeps the undo history where a resize clears it.** The canvas size
  does not change, so not one recorded rectangle stops being valid — see the
  `EditBody` rule under Undo. `app.rs`'s `mirror_document` is the single route,
  shared by the command and by both undo directions; a second implementation for
  the undo would be a second thing to keep exact. Everything that has to be
  quiet first is: the float is committed, the stroke is finished, and the
  autosave capture is cancelled on both halves.
- **The flip pass must stay an exact texel permutation.** Undoing a flip *is*
  another flip, so any loss compounds every time somebody flips and undoes.
  `flip.wgsl` reads with integer `textureLoad`, through **non-sRGB views** of
  the `Rgba8UnormSrgb` layer array on both sides, with `blend: None` — a raw
  `u8/255` round trip is exact where a decode to linear and a re-encode is a
  promise about rounding. That is why `LAYER_FORMAT_LINEAR` is in the array's
  `view_formats`. A texture cannot be its own attachment and
  `copy_texture_to_texture` cannot mirror, hence the scratch and the copy back.
  `a_flip_mirrors_the_canvas_and_flipping_twice_restores_it_exactly` guards it.
- **The selection flips with the picture, through `Selection::flipped`** — rings
  mirrored and rasterised again, exactly as the marquee travelling with a
  transform commit does. Not a mirrored *mask*: that would be a second
  rasteriser to keep in step about every antialiased edge.
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
  rather than opening it with pieces missing. It is at **3**. Revision 2 was
  masks and clipping, because a build ignoring either shows a picture that is
  *wrong* — what the mask hid comes back, and the clipped layer paints
  everywhere. Revision 3 is **layer effects**, and that one is not about what an
  older build shows but about what it *writes*: the parameters are the whole
  feature, so opening and saving drops them for ever. Locks, links and a **text
  record** all ride along, because ignoring them changes no pixel — and the text
  record only rides along because of its fingerprint, without which an older
  build could paint over the layer and this one would re-render over the
  brushwork. See "Text".
- **A derived `Debug` or serde spelling that reaches a file is a format, not a
  name.** `docformat::history::kind_id` and `blend_id` are both
  `format!("{:?}")`, and `BlendMode` additionally derives `Serialize` because a
  brush carries one and a brush is what `brushes.ron` holds. The reason for the
  derive is good — a second hand-written table is a thing that can drift. The
  cost is that **renaming a variant, a refactor with no intent behind it,
  changes what is written to disk**, in three places with three blast radii.
  For `EditKind`, a saved history is dropped whole. For `BlendMode` in an
  `.ora`, `blend_from_id` answers `None` and the reader falls back to
  `composite-op`, exact for most modes and approximate for Add, so every Add
  layer in every saved document quietly downgrades. **And for `BlendMode` in a
  user's `brushes.ron` it is not a downgrade at all**: `parse` reads with `?`,
  so an unknown variant is a hard error and the painter's whole collection
  fails to load. The serialised *preset* is the one nobody thinks about and it
  is the one that costs most.
  **Both id sets are therefore pinned as literal strings**, which catches a
  rename. What that cannot see is a `#[serde(rename = "…")]`, which changes
  what `brushes.ron` carries while leaving the `Debug` spelling untouched — one
  guard covering two mechanisms, and covering the second only because the
  derive and the `Debug` spelling agree today.
  **The serde spelling is about to reach a *document* as well, and the gap was
  real until it was pinned.** An `Effect` carries a `BlendMode`, and once the
  writer lands that spelling goes into `umber/effects/<n>.ron`, where an unknown
  variant is a **hard parse error** rather than a downgrade — and Multiply is
  the drop shadow's own default. Until then the only serde text anywhere was
  `builtin-brushes.ron`'s 252 `blend:` fields, all `Normal`, so
  `#[serde(rename = "Mult")]` on `Multiply` left all 866 tests green. That was
  demonstrated by mutation, not argued, and pinning it *before* the format
  exists is the cheap direction.
  `the_serialised_names_of_a_blend_mode_are_these_exact_strings` is the pin, and
  it is the remedy this paragraph already prescribed applied to the mechanism it
  already warned about.
  **The coverage observation is what survives the fix, and it generalises.**
  `builtin-brushes.ron` is `include_str!`'d and parsed by tests, and carries
  252 `blend:` fields — every one of them `Normal`. Nothing anywhere serialises
  a non-default mode. So rename `Normal` and the build goes red; rename
  `Multiply`, `Screen`, `Overlay` or `Add` and the whole suite passes green
  while every painter's library breaks. **A fixture compiled into the binary is
  not a test of the format, it is a test of the fixture**, and a field
  appearing two hundred and fifty-two times carrying one value is one data
  point wearing the costume of coverage. Pin the *set*, as literal text, rather
  than trusting the values a fixture happens to use.
- **Folders did not move it either, and are baseline ORA rather than an
  extension.** A folder is a nested `<stack>` — the nesting GIMP, Krita and
  MyPaint all write and the one `docimport::openraster` already parsed. A reader
  that flattens it away shows the identical picture and loses only the grouping,
  because a pass-through folder *is* its contents composited in place: plainer,
  not wrong, which is the line the version is drawn on. The folder's tag
  therefore carries a name, a visibility and a lock, and deliberately **no
  `opacity` and no `composite-op`** — a group opacity is the one thing a
  flattening reader cannot reproduce, and writing one is what would earn the
  bump. `a_document_of_folders_still_declares_the_revision_it_needs` guards it.
- **`umber-version` is at 3, and layer effects are what took it there.**
  `docs/layer-effects.md` §8.2. The argument is not what an older build
  *shows* — a layer without its shadow is merely plainer, which is the folder
  case — it is what an older build *writes*: effects are non-destructive, the
  parameters are the whole feature, so opening and saving drops
  `umber/effects/` permanently. **`docs/group-compositing.md` §4.3 wanted 3
  too; effects landed first, so group compositing takes 4**, and that is
  recorded in §4.3 rather than only in a Rust comment, because the person who
  implements it reads the document.
- **An effects record goes outside the ORA stack, under `umber/effects/`, named
  by an attribute on the element** — the mask's shape, for the mask's reason. A
  document-wide table would need a key and every candidate is wrong: a stack
  position shifts, a name is not unique, and `Layer::id` is never written down.
- **Both writers were wired in one commit, deliberately.** A build that reads a
  version-3 document and drops its effects at the next Save is the exact
  failure the bump exists to prevent, arriving inside the build that raised it
  — and the version gate cannot catch it, because the gate is
  `version > VERSION` and this build *is* 3. Wiring Save alone looks like half
  a fix and is **worse**: Save would preserve while the autosave stripped every
  five minutes, so survival would depend on which path last touched the file.
  Losing something every time is a bug somebody reports; losing it sometimes is
  one they doubt themselves over.
- **A parameter record needs a size bound of its own, and it is the first entry
  that did.** Every other entry is a canvas and answers to `MAX_TOTAL_BYTES`'
  2 GiB; a record's size follows a *count* the format does not bound. Measured:
  569 KB expands to 300 MB and twenty million effects in eight seconds,
  materialised before any budget check sees it, and sixty-four layers may name
  the same entry. `MAX_EFFECTS_BYTES` is 64 KiB, derived from what
  `MAX_ENABLED` permits.
- **`PrettyConfig::new()` takes the *platform's* line ending.** Right for
  `brushes.ron`, wrong for a document: the same `.ora` saved on Windows and on
  Linux differed byte for byte. A document travels and a preference file does
  not.
- **A refusal that cannot be reported is not a refusal.** `set_effect` answers
  `false` for the budget and `true` for a duplicate it silently replaced, so
  the budget is settled by `disable_effects_over_budget` before anything is
  installed — called from the reader *and* from `open`, idempotent by
  construction, so the diagnostic and the guarantee are one function — and
  "at most one per kind" is `LayerStack::duplicate_effect_kind`, which the
  reader asks rather than keeping its own copy of.
- **A warning must name a loss that happened.** `EffectsNotPortable` is once
  per document and counts layers with an effect switched *on*: per layer it was
  thirty lines of one sentence, and a layer whose effects are all off draws
  plain in Umber too. `EffectsOverBudget` states what happened and does not
  tell the artist to switch an effect off, because no such control exists.
- **An unreadable effects record costs that layer its effects and nothing
  else** — the mask's rule, not the saved history's. A history is a sequence in
  which each entry restores what the next expects, so one missing from the
  middle is a *wrong* history; effects are independent per layer.
- **The writer emits the lowest revision the file actually needs**
  (`required_version`), so a document with no mask and no clipping still
  declares 1 and still opens in every older Umber. A version number is a
  statement about what a file *contains*, not about what wrote it.
- **A mask goes outside the ORA layer stack**, under `umber/masks/`, pointed at
  by `umber-mask`. The nested-`<stack>`-plus-`svg:dst-in` convention is the
  obvious alternative and is not baseline ORA: GIMP and MyPaint would read it as
  a layer that erases the one below — a file that opens *wrong* elsewhere, which
  is worse than one that opens plain. **It is not what Krita does either**, and
  this file said so for a while: Krita's ORA export writes each layer's
  `projection()`, with masks already baked into the pixels, and emits no mask
  element at all. The `svg:dst-in` Krita does write is an ordinary *layer* blend
  mode. So nothing Umber reads was ever written that way, and the argument for
  going outside the stack rests on GIMP and MyPaint alone — which is enough.
- **A folder's `<stack>` carries `isolation="auto"`**, because ORA's default is
  `isolate` and every folder in this build is pass-through. See `folder_xml`:
  saying nothing made the file claim the opposite of what the screen showed, and
  the two agree only for as long as every child composites `svg:src-over`.
- **The undo history is written too**, under `umber/`, pointed at by
  `umber-history`. `docformat::history` has the argument; the rules it lives by:
  - **A slot is never written down.** `PixelPatch::slot` is a texture slice, and
    a slot in a file read into another session's allocation is a patch replayed
    into whatever inherited that number — the bug that parking a deleted layer's
    slice exists to prevent in memory, made permanent on disk. Entries name a **stack position**; `SaveHistory::new` maps slot
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
  - **An entry carries its *pieces*, and that is what raised
    `history::VERSION` to 2** — the first revision to earn a bump. A build
    reading only revision 1 would take an entry's first PNG for the whole
    rectangle and write it back over pixels that were never part of the edit,
    which is a document quietly damaged by an undo. It now discards the history
    and opens the picture whole instead. This build still reads revision 1: an
    entry with no `pieces` is one piece covering the rect.
  - **A canvas flip is what raised `history::VERSION` to 3.** A build reading
    only revision 2 would not merely be one entry short: every entry older than
    the flip was recorded in the opposite orientation, so dropping it writes
    each of those patches back *mirrored*. It discards the whole history and
    opens the picture whole. A flip entry writes no PNG, so it can never be the
    entry that reaches `BUDGET_BYTES`.
  - **Saving a history still did not bump `umber-version`**, and the argument
    is in `docformat`'s module docs. An older build ignores an entry it has
    never heard of and opens with an empty history — exactly what every build
    before this did. `history::VERSION` governs the manifest, and an unreadable
    one is *discarded* rather than refused.
  - **It is a preference, `ui.save_history`.** A full-canvas session takes a
    9.7 MB document to 22.1 MB, and a heavily grained one 21.3 MB to 52.1 MB;
    that is a trade the user has to be able to refuse. Nothing here touches the
    GPU — the patches have been in memory since commit time.
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

### Exporting

`umber-core::export` is what a format is; `exportdlg.rs` is the dialog.

- **Export is encoding and nothing else.** `export_rgba` hands over straight
  alpha sRGB out of the *screen* composite pass and `export` turns it into a
  file. There is no second flattening path, which is why the background rule
  holds for free: a white-backed document exports opaque, a transparent one
  keeps its alpha, and `app.rs`'s export never mentions `Background`.
- **PNG stays on the `png` crate** — the direct encoder is what lets an exported
  PNG carry the `sRGB` chunk stating what the composite already did. The other
  four go through `image` with `default-features = false` and only their own
  features; its PNG *decoder* is a dev-dependency alone, so the round-trip tests
  can read back what they wrote without one reaching the shipped binary. **WebP
  is refused**: `image`'s encoder is lossless-only, which delivers neither a
  small lossy file nor wider support than PNG.
- **A format that cannot carry alpha mattes onto a colour the artist chose,
  white by default, mixed in linear light.** Silently onto black is the classic
  version of this bug. Every loss is named *before* the write — the matte, GIF's
  256 colours, JPEG discarding more every time — and `losses` takes whether
  **this** document has transparency, so an opaque one is told it loses nothing.
  A warning shown every time is one nobody reads.
- **What a format is stays in `umber-core`**: extensions, what each can carry,
  the suggested name, and what to do when a typed name disagrees with the picker
  — report it, never let a filename overrule the choice. Same division
  `CanvasCopy::plan` and `Clip::place` keep, and it is what makes the whole of
  it testable without a device.
- **`write_encoded` is still the one atomic write**, and its temporary now takes
  the *target's* extension so an exported `a.png` cannot collide with a
  concurrent autosave of `a.ora` beside it.
- **Not threaded, deliberately.** The file dialog above it already blocks, no
  stroke can be live, and a threaded encode would hold the whole picture and
  report failure into what may by then be a different document. The autosave
  threads its writer because nobody asked for it; an export was asked for. GIF's
  quantiser on a very large canvas is the slow case, and if it ever bites the
  fix is a thread for the encode alone, never a second readback.

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

#### Offering a copy back

`autosave.rs` decides what may be offered and `recoverdlg.rs` paints it — the
division `dock.rs` keeps against `panels.rs`.

- **A session marker is what says the last run stopped rather than ended.** One
  small file per run under `autosave/sessions`, held open and **exclusively
  locked** for the whole run. The lock is the operating system's, so it is
  released identically by a panic, a hard kill, an out-of-memory and a power
  cut — which is exactly why a crash report cannot be the signal: a panic hook
  writes one and nothing writes anything when the process is killed outright. A
  **second Umber** sees the first's marker locked and leaves it alone. A lock
  that can be neither taken nor refused is read as *still running*: not offering
  costs an offer the autosave folder still holds, where over-offering would put
  two processes on one painting.
- **It is removed from one place, `lib.rs::run` after `run_app` returns**, and
  deliberately not from a `Drop` or from beside each `event_loop.exit()`. A
  `Drop` runs while a panic unwinds, so it would remove the evidence in the one
  case the marker exists for; four call sites is the invariant that will be
  forgotten at the fifth. After the `?`, because a loop that ended by *failing*
  is not a shutdown.
- **`SessionMark::open` must not truncate**, and the record is read back
  **through the handle the lock is held on**. The file has to be opened before
  it can be locked, so truncating on open would empty a marker somebody else is
  holding before discovering that they hold it; and Windows' `LockFileEx` locks
  the file's *bytes*, so a second handle reading a marker this process has just
  claimed is refused — which made every offer come back empty.
- **A dead marker is held until the offer is answered.** Reading it and letting
  the lock go leaves it unlocked for the minutes somebody spends reading the
  dialog, which is a window for a second Umber to offer the same documents.
- **The marker follows the session, on `crash::note_documents`'s terms.**
  `Autosave::note_documents` runs every frame, reduces the tab strip and the
  copies chosen for it to one number, and rewrites only when that number moves.
  `revision` is deliberately *not* in the reduction, so a stroke costs nothing.
  Without it a document closed or saved after its copy was written would be
  offered back — work somebody deliberately let go of.
- **The offer may not claim a copy is complete, and says so.** A crash box
  compares the copy's revision against the document's because a panic hook reads
  a session still in memory; nothing at the next start can, so every row says
  when the copy was written and that anything painted afterwards is not in it.
  `no_row_claims_a_copy_is_complete` fails the build on "everything",
  "complete", "all of" or "safe" appearing in one. What *is* checkable is
  compared: a copy the painter's own file is already at least as new as is not
  offered, within two seconds' slack for FAT's mtime granularity.
- **`at_risk` alone raises nothing.** Naming a document with no copy exists so a
  dialog offering two back does not read as a promise about the third; with
  nothing offered there is no such promise, and what is left is a box saying
  work was lost with nothing to click. It would also be the *common* case: an
  operating system restart force-kills applications and Umber refuses to close
  while a document holds unsaved work, so an ordinary reboot leaves a marker
  every time.
- **A recovered document's own file is withheld from the timer.** Its tab points
  at the painter's path so Save writes where they expect — and `next_due` only
  picks *modified* documents, so a recovered one would be the one kind the
  autosave writes back: five minutes after clicking Open to see what was in a
  copy, unasked, with no history in the copy to step back through.
  `Tab::recovered` and `Candidate::write_own_file` hold it until an explicit
  Save. **The tab never points at the copy**, or a Save would write into Umber's
  own autosave folder.
- **A row says "Recovered" only once its copy opened.** Marking it as the
  request is taken puts that word beside a truncated archive that produced no
  document, with the button that would let somebody try again gone.
- **Nothing on this path deletes a document.** `Reaper` is untouched; markers
  get their own much smaller deleter, `Marks`, with the same structural
  containment, rather than `Reaper` being widened by one name — which is exactly
  the loosening its own rule refuses. **The copy a marker names is checked, not
  trusted**: `ours` requires a name an autosave writes, directly inside the
  copies directory, which stops a marker naming the painter's own document from
  having it opened and offered back as a copy of itself.
- **A marker that names nothing is forgotten; one that cannot be *read* is
  kept.** The two look alike and are not: a rewrite a power cut caught half done
  may still name copies sitting in the folder. `MARK_FORMAT` is compared, unlike
  `crash::Report`'s, because a marker genuinely is read across builds.
- **Dismissing forgets the marker and keeps every copy**, and the dialog says
  both — including "This offer is only made once".
- **There are no previews.** `mergedimage.png` is in every copy and could be
  lifted out, but decoding one per copy on the start-up path is not a directory
  listing.

### Crash reporting

`umber-app/src/crash/`. A panic hook that writes a report and a **second
process** that draws the box. `mod.rs` is the hook and the command line,
`report.rs` is the record and every sentence it can produce, `window.rs` is the
reporter's own window.

- **The box is drawn by a fresh process, and that is the whole design.** The
  hook writes a `Report` to a file and spawns *this same executable* with
  `--crash-report <path>`; that process gets its own adapter, its own surface
  and its own egui context, and draws the dialog out of `theme`, `widgets`,
  `icons` and `tabs::dialog_frame`. Restart is then the same spawn with no
  argument. The two alternatives lose, and the reasons are the module docs':
  **in-process** is unreliable exactly when it is needed — the crash this was
  built for was a wgpu validation error in `Queue::submit` followed, *while
  unwinding*, by `wgpu-hal` refusing to destroy a swapchain semaphore a surface
  texture still held, so the device is poisoned and egui's own textures may be
  among the destroyed objects; drawing with it asks the failing subsystem to
  report its own failure, and a panic inside the panic handler replaces a
  legible stderr message with a double fault. **A plain OS message box** always
  works but has no expandable section, cannot scroll a backtrace, cannot offer
  a restart, and is three implementations with three capabilities. It is the
  right *fallback*, so the fallback here is **stderr**: an unwritable report, an
  unspawnable child and a window that will not open all end with the process
  dying exactly as it does today.
- **Nothing is made quieter.** The previous hook — the standard library's — is
  called **first and unconditionally**, so the message, the backtrace and
  `RUST_LOG` are untouched. This is an addition to what a crash does.
- **The hook must not panic, and must not report a worker.** No `unwrap`, no
  indexing, no slicing between `set_hook` and `spawn`; every failure is a log
  line and a return. A panicking thread that is not `main` ends that thread and
  leaves the application running, so a box saying Umber stopped would be false —
  it is logged and passed over. `REPORTING` latches so the second panic during
  unwinding cannot spawn a second reporter.
- **The hook reads a snapshot, never live state**, because it cannot borrow
  `Editor` and the frame that panicked may be halfway through changing one.
  `crash::note_documents` is called once per frame from `render` and returns
  without allocating unless a reduction of the tab strip to one `u64` has
  changed — which is what lets it sit on the drawing path at all. The hook takes
  the snapshot with **`try_lock`**: the panicking thread may be the thread
  holding that lock, and `lock()` would deadlock the hook against itself.
- **A rescue sentence may never overstate.** `Report::rescued` lists a document
  only where `Tab::modified` was true — the flag that means "closing this would
  lose something", already cleared by `Session::mark_autosaved` where the
  artist's own file was written — and compares the copy's revision against the
  document's to say whether it holds everything or stops short. A modified
  document with no copy is named by `at_risk` rather than passed over, because a
  box that lists two rescued documents and is silent about the third reads as a
  promise about the third. Same rule as the autosave's: claiming work is safe
  when it is not is worse than claiming nothing.
- **wgpu's `on_uncaptured_error` goes down the same path.** `crash::device_error`
  logs the error in full and then panics with Umber's own wording, so the report
  says what happened instead of `wgpu_core`'s line number. It stays fatal: a
  device that has reported an uncaptured error produces undefined results from
  then on, and a quietly wrong canvas is what this codebase refuses everywhere.
- **`panic = "abort"` changes nothing and there is no `catch_unwind`.** The hook
  runs before the abort exactly as it runs before unwinding, and nothing here
  needs the stack unwound. Catching around `run_app` would happen *after* every
  destructor that produced the second panic, `run_app` is not `UnwindSafe`, and
  on Windows the loop unwinds through a Win32 message callback where catching is
  not dependable.
- **The "Copy details" button copies `Report::details`, the same string the
  block beside it draws, and it sits *above* that block** — a backtrace is
  unbounded, so a control after it is one somebody scrolls a page of frame
  addresses to reach, which is the failure the brush editor's Edit mark is in
  the header to avoid. There used to be no such button and the reason was sound:
  `egui-winit` was built `default-features = false`, its `clipboard` feature was
  not compiled in, and `Context::copy_text` fell through to a `String` held in
  the process — a control that would have looked like it copied a backtrace into
  an issue and copied it nowhere. **The premise changed, not the standard**: the
  canvas clipboard needed the same crate, so `arboard` and the packaging
  declarations that were not worth paying for one button had to be paid for
  anyway. What did not change is the rest of the route out — the report is still
  a *file*, the box still names its path and opens its folder, and the details
  are still a selectable read-only `TextEdit`, bounded by a `ScrollArea` rather
  than sizing the window — because on X11 what is copied dies with the process
  unless a clipboard manager takes it, so the one control for getting a report
  out must not be the only one. The confirmation is a **latched line, not a
  timer** ("Copied — paste it before closing this window"): the rule `repaint_at`
  exists for is *perpetual* repainting, and one deferred frame is not that, but a
  line that simply stays costs the frame the click already caused.
  `about::link_row` still paints its own hyperlink: that is the `links` feature,
  which is a different one and is still off.
- **Reports are never deleted.** Each is a few kilobytes and is the only record
  of what happened. `autosave::Reaper` is deliberately the only thing in Umber
  that deletes a file on the user's behalf, and its containment is careful for a
  reason; a second deleter for kilobytes is the wrong trade.
- **The heading is "Oh no, oopsy", and it is the author's own voice.** It is not
  behind a preference and there is no second heading under it. What carries the
  information is the paragraph and the "Your work" section beneath — a crash box
  whose only content is a joke leaves the artist with nothing to act on, which
  is why those two are held to the rules above.
- **`parse_args` is a pure function of the arguments**, like `install::detect`,
  and an argument it does not recognise is logged and ignored. Umber is launched
  by file managers, desktop entries and `cargo run --`; refusing to start a
  painting application over a stray word is a far worse failure than starting.

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
- **A `.clip` is a SQLite database in a chunk wrapper, and both halves were
  already here.** `umber-core::sqlite` exists because a `.sut` brush is one too,
  and `csblocks` is the 256-square zlib block stream a brush *material* stores
  pixels in — a document layer's are in the same stream. That is why Clip Studio
  support added **no dependency**. Two copies of the block framing is the drift
  `docformat`'s "there must never be a second ORA reader" refuses, so it is one
  module and `csmaterial` is a caller.
- **A `.clip`'s stack runs bottom to top, and that was established from files
  rather than assumed.** `Canvas.CanvasRootFolder` → `LayerFirstChildIndex` →
  `LayerNextIndex`. A reader that gets this backwards still produces a picture,
  which is why it was worth five real files to settle.
- **What an absent block holds is read out of the file, never assumed.** A raster
  layer's `InitColor` states nothing and a **mask's states all-ones**, because a
  Clip Studio mask begins revealing everything — taking an absent block for zero
  blanks the layer everywhere nobody painted. And a test of that has to start
  from a canvas whose blocks can *actually* be absent: at 300 square every block
  but the first overhangs, so none can be left out and the guard tests nothing.
- **A bitmap's size must be bounded by the canvas, not by `MAX_DIMENSION`.**
  Nothing else ties a layer's own bitmap to the document, so a 1×1 canvas with 64
  layers passes at 256 bytes while each layer may still declare 16384² — 1.3 GB
  of inflate each, all discarded by the blit. **A blit must clip against the
  bitmap as well as the canvas**: a bitmap is padded to whole blocks, so clipping
  only against the canvas copies that padding over the picture.
- **A Krita group is a folder now.** Krita lists layers uppermost first, so the
  reversal the reader already did puts a group after its own contents — where a
  `LayerStack` keeps one. A group's **opacity** still folds into its children (a
  folder at 50% over two overlapping children is not two children at 50% each);
  its **eye** does not, because it lives on the folder. `GroupFlattened` now
  means only what it means for ORA: nested past `MAX_DEPTH`.
- **A folder carries less than a layer, and the difference is a loss list.**
  Umber's folders are pass-through, so a source folder's opacity, blend mode and
  mask all have to be reported. In `.clip` the opacity cannot even be folded —
  the contents are built before the folder is reached. And a folder's blend mode
  is lost **whether or not Umber has that mode**, so the test is "is it
  pass-through", not `blend::nearest`.
- **MediBang `.mdp` is not read, and the blocker is one bit of information.** The
  format is documented well enough to write — the note calling it "proprietary
  and barely documented" was wrong in every clause — but whether the XML layer
  list runs top first or bottom first cannot be settled from the samples
  available: one has a single layer, and in the others the visible layers do not
  overlap, so either order matches the file's own thumbnail. A reader that
  guesses inverts every multi-layer document silently. `docs/document-import.md`
  has the format; what it needs is a file that settles the direction.
- **`.psd` masks: the verdict was re-opened and a narrow fork is now the right
  answer.** "A second parser walking the same bytes" is correct about a *whole*
  PSD reader and overstated for a mask — the layer-mask block is length-prefixed
  and holds a rectangle, a default colour and two flag bytes. But it does not
  stop there: the mask's samples are a fifth channel, and the crate's channel
  walk assumes every channel is the layer's height, so the fork has to read
  channel lengths too. Roughly 200–300 lines, and the same work fixes the RLE
  refusal that currently rejects the whole file. Keep the crate for pixels,
  compare record boundaries against it per layer, and refuse **the mask, not the
  document** on disagreement.
- **`psd` 0.3.5's `Layer::visible()` returns its own opposite.** Adobe's flag is
  *hidden*; the crate reads it as *visible*. `photoshop.rs` inverts it, and a
  test pins that. Do not "fix" the inversion.

- **A mask arrives from a `.kra`'s transparency masks and from nowhere else but
  Umber's own ORA.** A mask slice holds **sRGB-encoded coverage**, because
  `composite.wgsl` reads its red channel through the layer array's
  `Rgba8UnormSrgb` view — while every source format states a mask as a *linear*
  multiplier on alpha. So 128 there is 188 here, and
  `docimport::srgb::encode_coverage` is the one place the two meet; copying the
  byte across is wrong by a full gamma curve and hides four fifths of a layer
  somebody hid by half. Krita's other four mask kinds — filter, transform,
  selection, colorize — are named, not approximated, and a transparency mask
  built from a vector selection arrives unmasked with a warning rather than a
  vector renderer being put inside an importer. The pixels are **not** beside
  the layer's tiles: Krita writes the selection's own paint device to
  `layers/<filename>.pixelselection` with the byte outside the stored tiles in a
  `.defaultpixel` neighbour, one byte per pixel.
- **`.psd` masks are not read at all, and that is `psd` 0.3.5's limit rather
  than a decision.** It reads the length of the layer-mask block and *skips* it —
  and that block holds the mask's own rectangle, which is where a mask's pixels
  live rather than the layer's — keeps the bytes behind a private accessor, and
  **panics on an RLE mask channel**, which is why such a file is refused
  outright. 0.3.5 is the newest published version. Reading one means a second
  parser walking the same bytes beside the crate's, which is the fork this
  module declines.
- **A Clip Studio dynamic is a curve *and* a floor, and the floor is half of
  what it says.** Clip Studio states each mapping's minimum as a percentage of
  the setting's own value, which is exactly what `Brush::min_size_ratio` means
  for size. Opacity has no such field and wants none — coverage genuinely
  reaches zero — so the floor is folded into the response curve, where
  `f + (1 − f) × curve(p)` is exact in a fixed row of samples. Reading the curve
  alone is a brush whose author had it painting from six tenths arriving
  painting from nothing: every stroke a fraction of the strength it was set to,
  reaching the colour asked for only once it has been laid down several times,
  which is indistinguishable from an opacity control that does not work. Per-dab
  coverage is Opacity **times** Brush density, under pressure as well as under
  speed.
- **Clip Studio leaves a setting's value in the file when the setting is
  switched off, and every read has to name the field that says so** —
  `BrushUseIn` rather than `BrushInLength`, the rotation effector rather than
  `BrushRotationRandomScale`, `BrushAutoIntervalType` rather than
  `BrushInterval`, and the texture reference **naming a material** rather than
  the column merely holding a blob. The paper is the worst of the four, because
  grain multiplies coverage: a brush that was never textured paints through
  paper it does not have — mottled, weaker than its opacity says, and darker
  each time the stroke is laid down again, since the pits are anchored to the
  document and a second pass composites over the first.
- **Pressure and randomness driving a setting Umber has no field for are lost
  and deliberately not named**, unlike tilt, the unnamed fifth source and stray
  velocity. Naming them was tried: the sweep runs over a 187-to-214-column
  schema and cannot tell a live effector from one whose bits were left behind,
  and the random bit is the *only* bit ever set on the hue, saturation and
  brightness effectors in both sample files. The result was a sentence on nearly
  every import, frequently about a mapping the brush does not have. **A list
  that cries wolf costs the losses that matter.** Silence beats a false apology
  until the enable flag beside each effector can be read.

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
- **A tip document is a 256×256 transparent square, and coverage is its
  alpha.** What you paint is what stamps: the eraser takes coverage off, opacity
  is the strength, and colour is discarded because a tip has none. Square
  because a stamp is stretched over the dab's bounding square and
  `TipMask::aspect` narrows it back, so a square is the one shape that says
  nothing the artist did not; 256 because below ~128 a stamp cannot hold detail
  a large brush magnifies, and above ~512 it is megabytes per brush. The
  white-page-and-read-darkness alternative is wrong twice here: a white stroke
  would have to mean "erase" while Umber's eraser means something else, and a
  fully covered canvas would be indistinguishable from a blank one.
- **An imported *picture* cannot use that rule, so it gets its own, stated
  one** — alpha where anything is less than opaque, darkness otherwise, decided
  **once for the whole image** and never per pixel, and the notice always names
  which reading it took. This is exactly why a tip *document* does not go
  through `coverage_of`: a fully painted canvas would flip the rule and invert
  the stamp.
- **A tip is never written outside `UserLibrary::save`.** A mask drawn or
  imported sits in the editor's hand until a Save or an Update; writing it
  earlier would leave a picture no preset names, which `prune_tips` deletes on
  the next write.
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
- **A collection the user *made* has to be written down, because every other one
  is derived.** A collection exists because some preset's `collection` or
  `category` names it, so a brand new one has no members to be derived from and
  would vanish at the next rebuild of the merged list.
  `LibraryFile::made_collections` holds them, behind a serde default so an older
  `brushes.ron` still loads and a library nobody has added one to is written
  byte for byte as before. `same_collection` — trimmed, case-folded — is one
  rule for "are these the same collection", used by both the model's refusal and
  the rail's row merge, so a brush dragged into a new collection cannot produce
  a second row beside it. `create_collection` takes the existing names from the
  caller for the reason `Library::collections` exists: the shipped half of the
  merged list is not that module's to enumerate.
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
- **Spacing is a fraction of how far the dab *reaches* along the stroke, not of
  its long axis.** `Brush::step_at` takes the angle between the two and uses the
  ellipse's **radius** in that direction, `1 / sqrt((cos Δ / a)² + (sin Δ / b)²)`;
  `Brush::off_heading`
  is where that angle comes from, and it is constant for a brush that follows
  the stroke and a function of the heading for one that does not. **A round dab
  is the exact identity** — `a == b` makes the support constant — so this
  changed nothing for 246 of the 258 shipped presets. It changed the other
  twelve completely: a marker nib held across the line travels on its *short*
  axis, so measuring the step against the long one steps past the dab and lays
  the mark down as a row of separate ellipses with gaps between them. Every
  marker, the calligraphy pen and the palette knife did it. **This is not an
  import bug and matching MyPaint is not the defence** — MyPaint states its
  spacing against the same long radius, so Ramón Miranda's "Marker" asks for a
  14.1 px step on a dab 10.4 px wide there too, and the faithful reading was
  the wrong one. What is left gapping is the two presets whose spacing is
  *above* 1.0, which is an author asking for separated dabs. **It is the radius
  and not the *shadow*, `sqrt((a cos Δ)² + (b sin Δ)²)`**, which is what this
  used first and is a different number wherever the stroke runs at an angle to
  the nib: for the shipped calligraphy pen at 46° off its own axis they are
  8.3 px and 2.9 px, and the pen still combed at the larger one. The shadow is
  how much ground a dab covers measured *across* the direction of travel; what
  decides whether two dabs merge is how far the ellipse actually reaches
  *along* it. The two agree at 0° and 90°, which is why the markers were fixed
  by either reading and the calligraphy pen, the scrapers and every other fixed
  nib by only one. The nominal
  `dab_ratio` and angle decide it, never `dab_angle_jitter` or a `dynamics`
  modulation: both move a single dab, and letting either into the step would
  make a stroke's spacing wander with the RNG. The tip mask's own proportions
  are **not** in it either — `tip_scale` narrows the quad and `Brush` cannot
  see the mask — which is a real gap only for an extreme mask at a wide
  spacing, and is where to look if a stamp brush ever shows this.
- **The damaged rect must cover the dab's *quad*, not its circle.**
  `StrokeBuilder::bounds` unions the axis-aligned box of the rotated quad of the
  *scattered* dab. Too tight and the edge of a mark is never committed — it
  redraws as a live preview and is then baked in by the next stroke, in that
  stroke's colour, and that has now happened four times. A round dab fits its
  bounding square at any angle, which is why the circle held until bitmap tips
  arrived: **a tip paints into the corners**, and a quad turned 45° reaches
  `radius * sqrt(2)`.
- **Its short semi-axis is the *dab's* `aspect`, never `Brush::dab_ratio`.**
  `dab.wgsl` builds the quad as `radius / max(aspect, 1.0)`, and `aspect`
  carries whatever a `DabTarget::Ratio` modulation added — 30 of the 258
  shipped presets can drive it below the nominal ratio, and
  `tanda/charcoal-04` all the way to a round dab against a `dab_ratio` of 10,
  so the box recorded a tenth of the dab's height. **This is the exact
  opposite of the spacing rule above, and the adjacency is how it happened**:
  damage is per dab and must follow the dab, spacing is per stroke and must
  not, or a stroke's step would wander with the RNG. Bind `aspect` to a name
  once and derive the short axis from it, so `bounds` and `damage` are fed
  from the same numbers structurally rather than by discipline.
  `widgets::preview_mark` had read `dab.aspect` all along — **the one
  duplicate this file licenses was right and the canvas was the outlier**,
  which is worth remembering before distrusting a second implementation on
  principle.
- **The same box goes into `StrokeBuilder::damage`, from the same numbers.**
  The cell mask is what the undo patch and the commit are both cut to, so a
  mask that did not cover what the bounding box covers is the under-tight
  damaged rect above, back again and much harder to see. Feed both or neither.
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
- **An imported per-dab opacity is converted to a stroke opacity, and is not
  the same number.** MyPaint, Krita and Clip Studio state an alpha *per dab*
  and composite every dab; `Brush::opacity` is applied once at commit over
  coverage a `max` has already saturated. `tip::dab_stack_alpha` is the
  conversion — the third of `stroke_coverage`'s family — and it simulates the
  dab pass's own falloff at the stroke's centre line. Reading one as the other
  shipped `4H_pencil` at 0.026 where MyPaint draws about 0.14, and put
  twenty-nine presets under 35% opacity with the faintest at 0.015. **The curve
  is converted with it**, because the relation is not linear: half the per-dab
  alpha is not half the built-up one, so a curve normalised on the raw values
  bends the wrong way once the peak has moved. A brush already painting solid
  comes back unchanged, which is most of the library — the median opacity is
  and stays exactly 1.0. **Only the MyPaint reader does this so far**; `kpp`
  and `clipstudio` state their opacity the same way and have not been
  converted, and Krita's wash/build-up painting modes have to be read before
  its can be.
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

### The eyedropper

`umber-app::syspick` reads a pixel of the *desktop*; the canvas half is
`CanvasRenderer::pick_colour`, unchanged. `Tool::Eyedropper` is the tool and Alt
with any other tool in hand is the same gesture.

- **One gesture, two ways in, one route to a colour.** Both resolve to
  `gesture::Press::Eyedropper`, so `app.rs` routes one answer and there is still
  exactly one call to `pick_colour`. A second `Press` variant would have been a
  second thing to route.
- **It is a drag, and that is the whole of how a colour outside the window is
  reachable.** winit takes the mouse capture on button-down — `capture_mouse`
  from `WM_LBUTTONDOWN` on Windows, the protocol's implicit passive grab on X11 —
  so `CursorMoved` goes on arriving with coordinates past the client size and the
  button-up arrives wherever it happens. No grab of our own, no global hook, no
  overlay; an overlay would be *under* the pointer and therefore the thing a
  screen read reads. **The pen path has not been run**: winit's `WM_POINTER`
  handler does not capture, so a pen rests on Windows' own implicit pointer
  capture; and a pen's Alt-tap is a *click*, because Alt-with-contact is the
  brush resize until the release settles it. A pen reaches the desktop half
  through the tool.
- **A read of the desktop costs a display refresh, so the sample is once per
  frame.** `examples/measure-screenpick.rs`: `GetPixel` against the screen DC is
  about 7 ms and `GetDC`/`ReleaseDC` is 9 µs, so there is no handle to cache and
  no cheaper call — 7 ms is 1/144 s and the read waits for the compositor.
  Re-run the example before changing any of it; a 60 Hz panel should read 16 ms.
- **That puts a blocking `pick_colour` on the frame loop, and it is the one
  exception to the rule under "Colour pickup".** The rule protects a *stroke*; a
  pick cannot coexist with one. `probe_canvas` is the obvious reuse and is the
  wrong instrument: its two-frame lag is free for a trailing average and wrong
  for a gesture the hand aims by a readout.
- **Three answers, not two, and the middle one was a bug.** `aim` answers Canvas,
  Interface, Desktop or Unreachable. `ui_owns_pointer` gates the *press* and
  nothing asked again, so a drag onto a panel went on sampling the document —
  `screen_to_doc` is a plain camera transform and maps a point over the Layers
  panel to a real document pixel at any zoom that fills the window.
  `Aim::Interface` reads nothing, gated on the same `Editor::pointer_over_canvas`
  that places the crosshair, so the mark and the behaviour cannot disagree.
  Reading a panel off the *screen* is the tempting alternative and hands back the
  theme's ink already composited with whatever egui drew over it.
- **`GetPixel` rather than `BitBlt`, and off every monitor is why.** Measured:
  outside the virtual screen `GetPixel` answers `CLR_INVALID` where a `BitBlt`
  succeeds against nothing and returns black. Two screens of different heights
  leave a real gap a drag crosses. `CAPTUREBLT` is refused: it repaints the whole
  desktop.
- **Windows only, and the strip says so where it is not.** The *tool* is not
  disabled elsewhere — picking inside the window works everywhere — so what is
  drawn is a sentence, in `syspick::outside_line`, which is the one place both
  readings live. X11 would be `xcb_get_image`; Wayland's answer is the
  compositor's own `org.freedesktop.portal.Screenshot.PickColor`, which is a
  different interaction rather than a backend; macOS needs Screen Recording
  permission and nobody here has a Mac.
- **A wide-gamut or HDR display hands back a number in its own space and it is
  read as sRGB.** Umber has no colour management anywhere, so there is nothing to
  convert against. Hardware overlays are not in it either — video planes read as
  black.
- **No loupe**, and Umber's own title bar is outside the client area and does
  read off the desktop. Both stated rather than denied.
- **`Tool::paints` was a `matches!` and is a `match`** — the standing rule paying
  for itself, since a new tool would silently have been one that paints nothing.
- **The eyedropper is the one tool with a cursor of its own.** Every other tool
  has a mark whose size is what the hand aims with; a pick has no mark and a
  one-pixel target.

### Undo

Stores the RGBA bytes a stroke replaced, not whole layers (a full 2048²
snapshot per stroke would exhaust a gigabyte in ~60 strokes).

The capture happens at **commit** time, not stroke start: the layer is untouched
until commit, so reading it there yields exactly the pre-stroke pixels, and by
then the damage is known. The readback blocks on the GPU, which is acceptable
once per stroke but must never move into the drawing loop.

- **A patch is the *cells* a stroke reached, not the box it spans.**
  `umber_core::damage::TileMask` accumulates a 64-pixel grid beside
  `StrokeBuilder::bounds`, and `pieces` merges neighbours along each row and
  clips them to the box. A thin diagonal across a 10000² canvas cost 381 MB as
  a rectangle and costs 6.8 MB as cells — depth 1 against 75. **Clipping to the
  box is what makes it free of regressions**: the pixels kept are always a
  subset of what the box held, so a small mark can never cost more than it used
  to. Do not "simplify" the clip away for whole cells.
- **This is a large improvement and not an unlimited history, and the
  difference must not be blurred.** A wash that genuinely covers a 10000²
  canvas is 381 MB of pixels however they are described, so its depth is still
  one. The measured depths against the 512 MB budget are in
  `examples/measure-undo.rs`, which is the file to re-run before quoting any of
  them.
- **The commit pass is scissored to the same pieces the patch was captured
  from.** Not an optimisation: committing the whole box would run untouched
  pixels through the blend, which is an identity in floating point written back
  through an sRGB encode — a promise about rounding rather than about pixels.
  Scissoring makes "an undo restores every pixel the stroke changed" structural.
  `an_undo_restores_every_pixel_a_tiled_stroke_changed` reads the whole layer
  back twice and is what guards it.
- **`read_layer_pieces` is one submission and one wait for all of them.** A
  hundred and fifty calls to `read_layer_rect` would be a hundred and fifty
  fences at pointer-up. It batches to the device's buffer limit and falls
  through to the banded reader for a piece too large for it — which cell runs
  never are, because they are never merged downwards.
- **A piece whose pixels are all identical is held as that one pixel.** Blank
  canvas and flat fills are most of what a stroke on a fresh layer captures, and
  the scan stops at the first pixel that differs, so busy paint pays four
  comparisons to be told it is not flat.
- **In-memory compression was measured and rejected.** PNG at `Fast` on the
  pieces of one full-canvas stroke on a 10000² canvas is 1.75 s, and Deflate is
  6.9 s — on pointer-up, with the artist waiting, for a factor that does not
  change the order of magnitude tiling already did. If it is ever wanted it
  belongs on a background thread compressing *older* entries, not on the commit.

- **Every entry is an `Edit` — a patch, an `EditKind` and a `Timestamp`** — and
  both the kind and the time travel with it across an undo, via `Edit::made_at`.
  Recomputing either on the far side would renumber and re-time the list as it
  is stepped through, and the kind is read off the *snapshotted* stroke style,
  so switching tool mid-stroke cannot change what the stroke that is ending
  turns out to have been.
- **`EditKind` has a variant only for something the engine can restore**, and
  the rule is the bound rather than the count. It is Paint, Erase, Transform,
  the two canvas flips, and the six structural edits — an entry exists where a
  patch was captured, where the edit is its own inverse (the flips), or where
  the *shape* of the stack can be put back (a delete, an add, a reorder,
  entries gathered into a folder, a mask added or removed). **A layer cannot be
  renamed at all**, and this line said it could until two agents went looking
  for `EditKind::Rename` on the strength of it: `Layer::name` is written only
  where a layer is created or imported, `LayerStack` has no rename method, and
  `panels.rs` holds no `TextEdit`. `docs/layer-rename.md` is the design and
  says why it is not free — it needs an `EditBody` arm of its own, because a
  names table on `StackShape` would revert a name changed *after* a reorder for
  exactly the reason `Kept` carries no mask. **"Clear layer" is the one command
  left that clears the history**, and adding a row for it means making it undoable
  first: a row naming an action that clicking it will not undo is worse than
  one the list stays quiet about. The same bound governs the icons:
  `panels::edit_icon` is exhaustive over `EditKind` deliberately, so a new
  variant cannot be added without deciding what it looks like — and an icon set
  richer than the enum would be a promise about what the engine records. **A
  paste is not a variant of its own**: it is a Transform patch with nothing
  where the pixels came from, and two rows that undo identically should not have
  two names. **A cut is not one either**, for the same reason — it is an Erase;
  see "Transforms and the clipboard".
- **A structural entry restores *shape*, not values.** `EditBody::Structure`
  holds a `StackShape` of `Kept`/`Gone` rather than a snapshot of the whole
  `Vec<Layer>`, and the difference is the point: a snapshot would make undoing
  a reorder silently revert an opacity changed after it. What a `Kept` entry
  carries is where a layer sat; what a `Gone` one carries is the layer itself,
  slot claim and all.
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
- **A flip stores no pixels, and that is sound only because undo is stepped
  rather than seeked.** `steps_to` turns a jump into that many single `undo`
  calls, so an older patch is always reached with the flip above it already
  undone — the canvas is back in the orientation that patch was recorded in and
  it applies verbatim at its own rectangle. No coordinate mapping, no mirrored
  bytes, no marking of which entries fall on which side of a flip. `EditBody`
  carries it: `Pixels` for everything that paints, `Flip` for the one edit that
  is its own inverse. The axis lives in the `EditKind` and nowhere else, so a
  row's icon and its pixels cannot disagree. A flip costs the budget nothing and
  is still evicted in timeline order — what ages out is the oldest, not the
  largest.
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
- **The budget is one figure the user sets, defaulting to 512 MB, and never a
  fraction of the canvas — and the panel names whichever figure is in force once
  anything has been dropped.** It reaches a `History` through
  `history::set_default_budget` rather than an argument, because a `History` is
  built by `DocumentState::blank`, `Editor::default` and `docimport`, none of
  which can see a `Prefs` — threading it through would put the setting into the
  signature of everything that opens a picture. Same shape as
  `shortcuts::publish`. `set_budget` re-runs the eviction on the spot, and
  `prefs::set_undo_budget` walks the parked tabs as well as the live document:
  the limit is *per document*, so one lowered on the active tab alone gives back
  a quarter of what the dialog promised. A patch is the *rectangle*
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
  The ceiling is **32 GB per document**, and the figure argues from what the
  engine can use rather than from what a machine has, which is the only honest
  source: Umber cannot read the machine's memory, and a per-document figure taken
  from it would be wrong the moment a second tab opened. A full-width stroke on a
  10000² canvas is 400 MB, so 32 GB is eighty of them; on 2048² it is two
  thousand entries, past the point where the budget rather than the canvas limits
  the history. Above that the answer is a patch storing tiles, not a larger
  number.
- **`restore` rebuilds the whole timeline from one read out of a file** —
  entries in timeline order, the position within them, and `dropped` — and still
  answers to the in-memory budget, so a file written by a build with a larger
  one cannot hand this process more than it allows. See "The document format".

### Partial exhaustiveness is worse than none

**An enum matched exhaustively at some call sites and by `matches!` at others
is more dangerous than one matched loosely everywhere**, because the compiler
appears to have your back and only half does. This is a habit here rather than
a slip — four instances were found independently in one day, in three files,
and the fourth was in the guard written to fix the first three.

`EditKind` is the worked example. Three consumers fail the build when a variant
is added — `label`, `flip_axis` and `panels::edit_icon`, all exhaustive with no
catch-all. Three did not: `is_structural` and `resurrects_pixels` were
`matches!`, which answers **false** for a variant it has never heard of, and
`ALL` was a hand-written `[EditKind; 11]` that still compiled at the wrong
length. `gesture::supersedes_stroke` was the same shape in a third file, a
*negative* `matches!` whose doc comment claimed it was stated over the whole
`Press` enum.

**The silent half is the half that damages a document, and it is worth
following one all the way down.** `SaveHistory::new` skips an entry when
`is_structural()`; one it does not skip falls through
`match edit.patches().first()` to `None => SaveBody::Flip`. So a new bodiless
variant is written into the `.ora` **as a canvas flip**. On reload,
`docimport::history` has a deliberate guard against exactly that outcome —
whose comment spells out the failure, "a corruption diagnosis for a file that
is merely newer than this reader" — and **that guard is gated on
`is_structural` too**, so it does not fire, the entry fails the `w == 0` bound
check, and the whole saved history is discarded. Somebody anticipated this
precisely, wrote the defence, and hung it on the one predicate that goes quiet
in that case.

The rules, and they are cheap:

- **Where a `match` would fail the build, do not write `matches!`.** Six arms
  cost nothing and turn a silent wrong answer into a compile error.
- **An exhaustive `match` that returns a number forces a number, not a legal
  one**, and the rule above does not cover it. `Effect::rank` is exhaustive over
  kind and position, so a third kind cannot be added without giving it a rank —
  and giving it rank 4 is the plausible slip, since `docs/layer-effects.md` §4's
  own list puts *the layer* at 4. It would then be silently neither inner nor
  outer, skipped by both `effects_below` and `effects_above`, and draw nothing.
  `no_effect_ranks_where_the_layer_does` runs over both `ALL`s. This is the same
  failure in numeric form: the compiler forced an answer and had no opinion
  about which.
- **An `ALL` array is guarded by an exhaustive match in a test, never by
  iterating itself** — a test that walks `ALL` can only ever check what is in
  it. Having the arms index `ALL` makes a short array an out-of-bounds panic,
  which is better; it is still not total, because an arm that does not index
  its own position compiles and passes. That was *measured* rather than
  assumed, and the comment names the hole instead of claiming the array cannot
  be forgotten. The only complete fix is a macro deriving the enum and `ALL`
  from one list, judged against this codebase's taste for per-variant rustdoc.
- **A guard's inputs must span the domain the *code* sees, not the one the
  constants describe.** The effect pass budget took its canvas dimension from
  `downlevel_defaults()`, which is what a canvas is guaranteed to *reach* —
  while `using_resolution` raises that same limit from the adapter, so the real
  domain was sixteen times larger. The arithmetic was right and it was asking
  about a smaller world than the one it protected, reading 37 passes against a
  real worst of 45. **`using_resolution` has now caused two bugs in one
  session** — this, and `MAX_SLOTS = 257` — both by looking like it raises a
  limit it does not, or not raising one it does. Check which of the two it is
  doing to any limit you reason about.
- **A test can agree for the wrong reason, and the cheap way to find out is to
  mutate the code it claims to cover.** An outline-position test read the
  *layer's* slice instead of the effect's, and the layer's first lit texel
  happened to be the number it wanted. Marking the shader's arm with a constant
  and finding the output unmoved is what settled it — two minutes, where no
  amount of re-reading the assertion would have.
- **Where a property cannot be tested, make it structural and say which it
  is.** The effect extract's wet-stroke flag has to equal the flag its effects
  record, and producing a layer whose effects disagree needs a canvas over
  4096 square. One binding read by all three sites is the guarantee; a comment
  saying "these three agree" would have been the discipline that eventually
  stops holding.
- **A guard's comment must not claim more reach than the mutation
  demonstrates.** Two of the four carried a doc comment promising exhaustiveness
  the code did not have, and the guard's own first draft made the same mistake
  one level up. The claim is easier to write than the guard.
  **This recurs, and the recursion is the thing to expect.** The slot-growth
  bound's sweep called `grown_capacity(0, needed, slice)` — always a cold
  start — while its comment claimed the bound held for every current capacity.
  A mutation to `current.max(1).next_power_of_two()` wastes **102 GB** at
  10000² and walked through it untouched, because at `current = 0` it is the
  identity. A guard written to answer "can this still overshoot" was blind to
  an overshoot three orders of magnitude past its own bound. **Check that a
  guard's inputs span the contract its comment states**, not merely that it
  fails on the bug you had in mind; the sweep is now over `0..needed`, which
  is the function's actual domain.
- **A `None` returned into a `.flatten()` is not a refusal, it is a silence.**
  `SaveHistory::new` answers `None` when a patch names a slot no layer holds,
  and its call site reads
  `self.editor.ui.save_history.then(|| SaveHistory::new(…)).flatten()` — so the
  save succeeds, the history is dropped, and nothing is said. `SaveWarning` has
  one variant and it is about blend modes. The refusal reads as loud at the
  site that *produces* it, which is the trap: whether a `None` is a diagnostic
  or a shrug is decided by its **caller**. Check the call site before
  describing a refusal as a failure somebody will see.

## Interface

Layout and tokens come from the **"Umber app"** screen of the Umber design
project (Claude Design project `3bfca321-22c2-4bf2-bbc9-80fab57f1e65`, read via
the `DesignSync` tool). That page supersedes the earlier "Umber Explorations"
page — go by it.

Most of the design is built: layout edit mode, the brush editor and library, the
settings dialog, document tabs and the splash. What is not — the navigator, the
brush editor's Wet edges section,
ten of the sixteen tools, drag-to-reorder in the rail, saved workspaces — is
listed in the README's "What is not there yet", with the reasoning in
`docs/architecture.md`'s roadmap and, for the brush settings, `docs/brushes.md`. **Do not add UI for features that do not work** — a
disabled control with an explanatory tooltip is better than a live one that
lies, and a control that is simply not drawn is better than either where the
design shows a whole row of them.

- **Never hard-code a colour.** Everything comes from `theme::Palette`, which is
  what makes the second theme a table of values rather than an edit sweep.
- **A token cannot answer for a surface nobody chose, and `theme::contrast` is
  what does.** Four marks sit on `Palette::backdrop` — the canvas scrollbar
  thumb, the pen dot, the splash's supporting lines and the input strip's prompt
  — and the theme editor lets anybody set that to anything. They were `text_dim`,
  chosen for being the one mid-grey ink whichever way a theme's surfaces run,
  which is exactly why it failed: on Krita's real 50% grey surround a mid-grey
  ink is **1.34:1**, worse than the 1.31:1 `rail` had already been rejected at,
  and no other token does better because the problem is the pit.
  `ink_on(surface, rank)` takes the surface towards black or white — whichever
  has the greater *headroom*, which is not the greater luminance distance — to a
  target set geometrically between 3:1 and everything the surface can give. Every
  surface admits at least 4.58:1, so the floor is always reachable.
  **`contrast::ratio` is one function shared by the derivation and every guard**,
  so a figure in a comment is a figure a test prints; two figures in this
  codebase were wrong until it was asked. Two of those four marks are as often
  over the *picture* as over the pit, and the docs say so: the worst case over
  any artwork improves, the common case over dark paint in a light-pit theme does
  not, and only a two-tone mark could fix that.
- **`backdrop` is a fill in six places and an ink in four, and only the fills
  were ever enumerated.** The selection marquee, the transform box, its handles
  and the rotation mark are dark-then-light pairs whose under-pass was
  `backdrop`, on the reasoning that it and the accent are "each dark in one theme
  and light in the other". Making Krita's palette faithful made that false and
  nothing caught it, because every guard measured inks against *surfaces a theme
  names* and these marks lie on paint somebody made — at `#808080` the pair's
  halves came within 1.60:1 and the dark line read **1.00:1** on mid-grey paint.
  `Palette::accent_underlay` derives that end of the axis from the accent;
  `the_marquees_pair_reads_over_any_artwork` sweeps artwork, and its second bound
  — never worse than the token it replaced — is the one that catches a regression
  rather than a threshold. **When a token changes, ask what is drawn *in* it as
  well as what is drawn *on* it.**
- **The accent is never ink on `control_active`.** That fill is the selected
  state; Graphite and Paper tint it towards the accent so an accent mark on it is
  4.74 and 3.80, and the four preset themes take their selection colour from the
  application they are named for, where it is 2.60 / 2.27 / 2.06 / **1.88** — and
  under 3:1 for all four accents in each. `Palette::active_ink` is the rule: the
  accent where it reads, `text_strong` where it does not, a derived ink where
  neither does. Neither fixed answer will do — always-accent is the defect,
  always-`text_strong` costs the design its ochre in the two themes that *were*
  the design. On that fill the ranks are `text_strong` and `text`; `text_dim` is
  1.43:1 in MediaBog and nothing may use it there. `controls::keycap`'s clashing
  cap is the one that was not a contrast fix at all: it stood
  `control_active`/`accent_dim`/`accent` in for a caution palette the comment
  said did not exist yet, and `warning_bg`/`warning_border`/`warning` are the
  design's own values byte for byte.
- **Six themes ship, and four of them are other applications' greys.** Every one
  is a `Palette` and nothing else, which is the claim above being cashed in.
  Every grey is sampled from a screenshot rather than eyeballed; the deviations
  are stated at the palette rather than in a commit message. Four rules came out
  of it:
  - **`ThemeKind::id` is the one statement of what a file calls a theme.**
    `prefs` writes it to the preferences file and `themelib` to a
    `.umbertheme`'s `base` line; the two used to hold a `match` each with a
    comment saying they were deliberately the same words, which is a thing that
    has to be true with nothing making it so. Pinned as literal strings, because
    a round trip is self-consistent under any rename. The shipped labels are also
    **reserved names**: nobody can call their own theme "Krita" any more.
  - **`Accent::Umber` means "this theme's own accent".** It used to spell
    Graphite's and Paper's out; for a theme accented in blue that hands back a
    colour it has never worn.
  - **A contrast floor has to be what ships, not the round number.**
    `text_reads_against_every_surface_it_is_drawn_on` runs over every theme
    rather than the ones being added, and the first thing it caught was Paper:
    `text_dim` on its own `window` is 2.92:1. A bound the shipped set does not
    meet is a bound stated wrongly — and a bound loosened to admit a palette that
    fails it is a defect wearing a guard's clothes.
  - **A guard's domain is the code's, and `style_from` is only half of it.** The
    tokens egui is handed miss every control this interface *paints* —
    `controls.rs`'s buttons, the canvas scrollbar, the pen dot, the splash. Three
    of four defects a critic found were pairs outside that domain.
  - **A preset theme's identity may live in a token the previews cannot show.**
    `every_theme_preview` draws the Settings dialog because it puts most of the
    palette on one screen, and `control_active` is not on it. A theme sampled
    from another application is likely to be recognisable by exactly that colour.
    Render a panel before judging one.
  - **A figure in a comment is what the next change gets argued against.**
    `no_two_accents_look_alike_in_one_theme` said the tightest shipped accent
    pair was 64; it is 68, and a new palette's accent had already been argued
    against the wrong number. Compute the figure with the code that will later
    check it, not beside it.
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
- **A keymap is `shortcut = <ActionId> <ChordId>`, and there is one statement of
  that line and one of the merge.** `shortcuts::shortcut_lines` writes it for
  both the `.umberkeys` keymap and the preferences file; `shortcuts::merge` is
  the tolerance both readers keep. They used to be two copies with a doc comment
  claiming they were identical, which is a promise held by discipline — and
  *nothing tested it across the boundary*, so renaming `prefs`' key left every
  test in both modules green while the interoperability died.
  `a_keymap_and_a_preferences_file_read_each_other` is that test. The one real
  difference is `every`: a keymap names every command including the unbound ones,
  a preferences file only the customised ones, because silence means opposite
  things in the two files — and a file carrying a keyboard to another machine
  must not fall back on "this build's default".
- **A dialog's button strip goes inside a `horizontal`.** A bare
  `Layout::right_to_left(Align::Center)` takes the *whole* of the remaining
  height of the `Ui` it is in, because the align is the cross axis — so on a
  short modal it stretches the dialog to the height of the window and leaves the
  buttons floating in the middle of it. `canvasdlg.rs` already wraps its footer
  for this reason and `updatedlg::actions_row` is the same wrap named; it is
  invisible on a dialog whose content is tall, which is why it went unnoticed.
- **A widget revealed on hover must not be what decides the hover.** egui stops
  its hover search at the topmost *interactive* widget, so a `Sense::hover()`
  row reads as not-hovered the moment the pointer is over a button inside it —
  and if that button only exists while the row is hovered, the two oscillate
  once a frame. Allocate the row unconditionally and test `contains_pointer`,
  which is geometry alone. This was a real bug on the Shortcuts page's `+`.
- **egui's finished textures are given back *after* `Queue::submit`, never
  before**, and `app::submit_frame` is the one place that does both so the two
  cannot be put the wrong way round. `egui_wgpu::Renderer::free_texture` calls
  `wgpu::Texture::destroy`, which takes effect immediately rather than when the
  last reference goes — so a texture named in `textures_delta.free` and also
  named by a draw already recorded this frame fails validation at submit, and
  wgpu's default handler makes that a panic that takes the application down.
  A same-frame free is legitimate and unavoidable: egui frees a texture when
  the last `TextureHandle` to it drops, and a cache replacing an entry mid-pass
  does that after an earlier widget has already queued a `Shape` carrying the
  id. After the submit no deferral is needed — wgpu keeps the resource alive
  for the submission using it — so this is the ordering, not a frame of grace.
  `egui_wgpu`'s own painter says the same thing in the same place. This was a
  real bug: opening the brush library crashed the application.
- **A texture cache keyed on an address must have the *shape* it drew in in the
  key too.** The Brushes panel and the library browser show the same presets at
  two row heights and are on screen together — the browser is a modal over the
  panel — so one preset is one `&Brush` drawn twice in one pass. Sharing one
  entry, the second row evicted the first's live texture, which is the free
  above, and it also re-rasterised and re-uploaded every preset visible in both,
  every frame. Comparing the size and rebuilding on a mismatch is not enough:
  that *is* the eviction.
  `a_preset_drawn_in_two_lists_at_once_frees_no_texture_either_still_draws`
  reads egui's own texture delta against the pass's tessellated meshes. It is a
  CPU test for a bug that otherwise only appears as a wgpu panic, and it is the
  only place it can live — `umber-render` may not depend on egui, so
  `gpu_pipeline.rs` cannot see any of this.
  The single-consumer caches — `brushlib::tip_preview`, `ui::paper_preview` —
  are safe because each is drawn once per pass with one value. That is the
  property to check before giving either a second call site, not a rule they
  state.
- The design's sliders, toggles and segmented pickers are **painted** in
  `widgets.rs`. Restyling egui's stock widgets into them was tried and fights
  the framework; add to `widgets.rs` instead.
- **A rail whose figure can be typed is `widgets::typed_rail`, and there is one
  of it — in two shapes.** `typed_row` is a panel body's two stacked lines and
  `inline_slider` is the tool options strip's single line; `RailShape` decides
  only where the label, the track and the figure are put. `number_row` is a thin
  adapter over it, kept because `colorpicker` and `settings` build a `NumberRow`
  by struct literal. The drag lands on each multiple of `snap` within an eighth
  of a step, Alt suppresses that, and a *typed* figure is snapped to nothing —
  the point of typing it is to say something the rail cannot. Every number is in
  the value's own units, never the readout's, which is what lets the wheel's
  angle (45°) and the interface scale be the same control.
  **The strip used to have a rail of its own whose figure could not be typed at
  all**, on the recorded ground that `number_row`'s two rows do not fit
  `metrics::OPTIONS_STRIP`'s 36 points — which was true and led straight to the
  wrong repair. Two implementations of "type it or drag it" is how the two end
  up disagreeing about what Escape does.
  - **`widgets::Figure` is the readout's rule** — scale, suffix, decimals, and a
    word for zero — and the one statement of format/bare/parse, so a call site
    cannot hand over a parser that disagrees with its own formatter. The zero
    word is the airbrush's "off", and the field *accepts* it as well as showing
    it: a readout its own control refuses is one the control cannot reproduce.
  - **`Rail::limit` is what a typed figure is clamped to; `Rail::span` is only
    what the rail lays out.** They differ only for brush size, whose rail stops
    at `tweaks::SIZE_RAIL_TOP` (1000) while `Brush::MAX_SIZE` is 2000, so typing
    1500 means 1500. A `const` assert keeps the two apart, because a rail that
    reached the whole range would retire the distinction silently and every test
    of it would go on passing by being vacuous.
  - **A buffer is compared against its *seed*, never against the value.** The
    value moves under a held buffer more often than it looks: egui surrenders a
    `TextEdit`'s focus on a click, so clicking the rail beneath a focused field
    is a commit and a drag in one frame. Comparing against the value then reads
    an untouched buffer as "somebody typed something" and writes the stale seed
    over the drag. `typed_value` takes the seed and has no access to the value,
    so the comparison cannot be written the wrong way round again.
  - **A `number_row` cannot carry a unit that changes with the magnitude**, and
    that is the one thing to check before reaching for it. Its readout is
    `value * per_unit` at one fixed suffix, which is what makes `parse` the exact
    inverse of `bare` *by construction rather than by agreement*. The undo budget
    says "512 MB" at one end and "32 GB" at the other, so it is the settings
    dialog's one hand-built typable figure, trading that structural guarantee for
    a tested one. Its rail being linear is **not** the reason and reads like one.
    **It opens on the readout *including* its unit**, unlike `number_field`,
    which drops the suffix: there the unit is fixed by the row, here it is the
    only thing on screen saying what a bare figure means, and opening "1 GB" as
    "1" turned a typed 512 into 32 GB.
  - **The figure is pinned to the end of its box that faces its own rail** —
    right when stacked, left when inline. A field is as wide as the widest figure
    its rail can show, so a figure floating in the middle of its reserve reads as
    belonging to whatever is across the gap.
- **A rail's span is not a bound on the value, and `drag_track` is where that
  has to be true.** `tweaks::Tweak::range` states the principle and the rail
  broke it. A value outside the span pins the knob at an end, and a stationary
  tap there — the one spot that looks as though it will do nothing — used to
  write that end: a click on the size rail took a 1045 px brush to 400. It now
  writes nothing while a **drag still does**, so nothing becomes read-only.
  Both ends matter: thirteen shipped presets carry a stroke span *below* its
  rail, where the mirrored bug raises the value instead. Widening the spans is
  the tempting fix and was measured and refused — it costs 27% of the
  granularity at the size a painter actually uses, to prevent a mis-click. The
  rule lives in `track_value`, a pure function, so "this is a no-op for every
  value in span" is a test rather than a sentence.
  **The size rail was later widened anyway, and for a different reason.**
  400 → 1000 was asked for, and it costs 13.3% of the granularity at every size
  (a log rail loses uniformly: `ln 1000 / ln 400` is 1.153). What makes it
  payable is that the figure can now be typed, so the exactness the rail gives up
  is exactness the keyboard hands back — the argument the earlier refusal did not
  have available. Preventing a mis-click is still not a reason to widen a span;
  `track_value`'s tap refusal is.
- **A menu row that stands for an `Action` takes its label and its key from
  `shortcuts`, never from a string at the call site.** The View menu drew
  `Action::FitView` as "Fit to window" while the Shortcuts page listed it as
  "Fit to view" — one command with two names, in an interface whose other view
  of it has a search field. `menu_item` is the only route and carries an
  `enabled` flag, so a row can be dead and still name its key; Undo and Redo
  were disabled *and* silent, which is why Ctrl+Z appeared nowhere outside
  Settings.
- **A field applies what was typed, not what it holds, and Escape is not a
  blur.** Two separate defects with one shape, both in a hex readout. egui's
  default `EventFilter` has `escape: false`, so `Focus::begin_pass` drops the
  caret before a `TextEdit` ever sees the key — the field then sees an ordinary
  blur and *applies* the buffer, so Escape over a half-typed `#C08` painted
  `#CC0088`. And the blur itself applied whatever the buffer held, which is
  harmless only while the buffer equals the colour: the click that picks a
  colour off the wheel **is** the click that blurs the field, and egui
  surrenders focus inside the field's own `interact`, so the stale hex was
  written back over the colour just chosen. Re-applying a colour to itself is
  not the identity either — `set_color` guards hue and copies saturation across
  unguarded, so clicking into the readout and out again wiped the picker's
  saturation on a colour dialled to zero value. Gate the write on somebody
  having actually typed. **`settings::token_row`'s Escape half is fixed, and how
  is the generalisable part.** Nothing *on the field* distinguishes an Escape
  blur from a click elsewhere, because the caret is already gone by the time the
  row draws. The key is the only evidence, and it is still in the input then —
  `egui::Modal` consumes it in `should_close`, which runs **after** its content.
  Read it, do not consume it, and the dialog still closes on the same keystroke.
  The guard has to run **inside a real `Modal`** or it proves nothing about the
  premise it argues from. What it does not claim: a complete six-digit hex has
  already been applied live and Escape does not take that back.
- **A text field in the interface needs no `shortcuts::set_capturing`.**
  `ui::draw` calls `shortcuts::set_typing(ctx.text_edit_focused())` once for the
  whole interface, so any real `TextEdit` is covered. `set_capturing` belongs to
  the chord recorder in Settings alone; a widget writing `false` to it every
  frame would hand dispatch back to the canvas while a chord was still being
  listened for.
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
- **A harmony's angles are on the RGB wheel, and `harmony.rs` says so.** `Hsv`'s
  hue is 0 red, 120 green, 240 blue, so the complement of blue here is yellow
  where a painter's RYB wheel gives orange. Both are defensible and visibly
  different, so the module states which one it is, what a painter should expect,
  and what an RYB mode would have to be — a hue *mapping* on the way in and out,
  behind a control, leaving the offsets and every caller untouched, never a
  second `hues`. Checked rather than assumed: Krita's selectors are HSV/HSL/HSI/
  HSY′ over RGB with **no** harmony rules at all, Clip Studio's Color Wheel is
  HSV or HLS with no relation generator, and Photoshop has had none in the
  application since the Adobe Color Themes panel was withdrawn. The one
  mainstream tool computing harmonies on RYB is **Adobe Color**, a web tool,
  which is why its complement of red lands near hue 137.
  `a_complement_is_the_opposite_hue_on_the_rgb_wheel` pins the three pairs
  somebody would check by eye — every other test in that file is stated in
  *offsets*, so a switch to RYB that left the offsets alone would leave all of
  them green.
- **Both tetrads are named and neither may be left bare.** `Tetrad` is the square
  (0/90/180/270) and `RectangleTetrad` the double complementary (0/60/180/240) —
  a different set, not a rotation. The word names two things, so with both in one
  dropdown a row reading "Tetrad" would be a control lying about which it drew.
  The **variant** keeps its bare name because `tetrad` is what a preferences file
  has been writing since the enum existed; the **label** is what moved.
- **A `widgets::dropdown` alone on a line reads as a caption, not a control.** It
  draws no fill, so a trigger with nothing before it is a word and a chevron on
  the panel's own background. The harmony relation picker was read exactly that
  way — an artist asked for a triad and a tetrad that were already in the menu —
  and the fix is a small dim caption *above* it rather than a label beside it,
  because beside costs the width the longest option needs and `Dropdown` elides a
  label it cannot fit. The picker-mode switch escapes this only because it keeps
  a leading mark.
- **The triangle's Angle rail rotates and Swap white and black reflects, and
  neither reaches the other's arrangements.** Rotation walks the three rotations
  of the corner labelling; the reflection gives the other three. Together they
  reach all six and neither needs to grow. Because the shape is equilateral and
  the axis runs through a corner the *outline* does not move, so `Hub::contains`
  cannot tell the two apart: the hub that judges a press is handed the flag for
  construction's sake and is provably a no-op today — demonstrated by mutation,
  and said at both places rather than claimed as guarded.
- **The theme editor's token chip opens the picker, not a picker.**
  `settings::token_picker` draws `colorpicker::show` with the artist's own
  settings and with `picker_mode_switch` itself, so every setting it can move has
  a control on the same modal. That is the *opposite* of `show_sliders`' rule for
  the New document and Export dialogs, and the difference is exactly that those
  draw sliders alone. What is never shared is the colour — `Editor::hsv` is the
  paint in hand and a token is not paint. **The palette is written live and the
  file is not**: a `ThemeLibrary` write reaches the disk immediately, so the
  write waits for the pointer to come up or the picker to close.
- **A nested `egui::Modal` must not paint a second backdrop.** `Modal` dims the
  whole window by default, so one inside another puts the interface at 37% — and
  a colour picker over a theme editor exists precisely to be judged against that
  interface. `backdrop_color(Color32::TRANSPARENT)` keeps every modal property
  that matters: the backdrop is what makes a click outside close it, and it
  refuses input painted or not.
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
- **There is one dropdown, `widgets::dropdown`, and it is the Colour panel's
  picker-mode look**: an optional leading mark, the label, an optional figure, a
  chevron, no fill. There were four — that switch, a filled pill on the tool
  options strip, a full-width row in the brush library and five stock
  `egui::ComboBox`es — which made one gesture read as four controls. **The menu
  is `egui::Popup::menu` at every call site**, not a flag on `Editor::ui`: egui
  holds the open state against the trigger's own id, which is the scope the flag
  was standing in for, and it gets click-toggle, click-outside and Escape
  without any of that being written down. `DropdownWidth` is `Content`, `Fill`
  or `Exact` and all three are used — a trigger in a row sizes to itself, one
  alone on a line fills it, and the curve presets and the layer blend are sized
  by the control beside them. **There is deliberately no filled variant**: the
  strip already has a filled pill in `widgets::chip`, where the fill means *not
  a control*, so a second one that opens would teach the opposite.
  `metrics::DROPDOWN` is the height and `text::TINY` the font, everywhere.
- **The canvas dialogs are one form and two call sites** (`canvasdlg.rs`), and
  what a size *is* is `umber-core::canvassize`'s — the shapes, the sizes under
  each, the paper table, the rounding, which shape a canvas reads as, and the
  device's bound. Same division `CanvasCopy::plan` and `Clip::place` keep. They
  share `CanvasForm` and one body; two dialogs drifting apart is how "New" ends
  up offering a preset "Canvas settings" cannot express. **Both open on an aspect
  ratio and the sizes follow from it**: the shape a canvas is has nothing to do
  with whether it exists yet, and offering the sizes to one of the two is how the
  pair starts to drift — which it had, since presets used to be New's alone. Only
  the heading, the button label and the anchor block differ. They are drawn from
  `ui::draw`, not from a panel body, for the reason the brush library's modals
  are. The anchor control appears **only** when the size is actually changing —
  on a New document there is nothing to anchor, and on an unchanged size it
  would be a live knob that does nothing.
  - **A paper size is a physical size, so the resolution is half of it.** Pixels
    are `round(inches × dpi)`, half away from zero, and the resolution has to
    reach `Document`: 2480 × 3508 recorded at 72 dpi is not A4, it is an
    875 × 1238 mm poster. The rounding is a rule rather than a coin toss because
    at 72 dpi it reproduces the PostScript page sizes exactly (A4 595 × 842,
    Letter 612 × 792) — an authority fixed decades ago. Ties are reachable at odd
    typed resolutions, since Letter is 8.5 in wide, so rounding up is pinned. The
    dialog re-derives the pixels whenever the resolution moves — **once, at the
    top of the body**, not beside each control that can move it, or it draws the
    old pixels beside the new dpi for a frame, which is a millimetre readout
    wrong by the ratio rather than merely stale.
  - **`Sheet::pixels` is deliberately not clamped at `MAX_EDGE`.** Clamping made
    A3 at 1402 dpi come back as 16384 square: an A3 button lit over a perfect
    square, `read` filing it as 1:1, and `max_dpi` answering that it fitted. One
    clamp, three lies. A sheet too large is reported at its true size and refused
    by the caller's limit.
  - **`Aspect::holds` and `choose` share one rounding, and it is the lock's
    too.** Cross-multiplying exactly while `choose` rounds is wrong twice: a
    canvas 1601 wide cannot be exactly 16:9, so dragging the width made the row
    of sizes appear and vanish once per pixel, and `choose`'s own output was not
    a shape the same module recognised. The lock was a third copy, in `f32`, in
    the UI file, agreeing with the others only by the accident that
    `1608 / (16/9)` lands exactly on `904.5`.
  - **The device's `max_texture_dimension_2d` is what decides whether a canvas
    can exist, and a canvas dialog has to ask.** Past it, creating the layer
    array is a validation error, which is fatal. Sizes past it are not drawn and
    a sentence says what the machine holds; a sheet's resolution is capped so an
    unreachable size cannot be asked for; and `document()` clamps as a backstop,
    which makes the guarantee one function rather than nine call sites. The old
    dialog never asked because nothing it offered exceeded 3840 — the shape of
    this class of bug: correct until the day something reaches the limit.
- **The settings dialog is one size, whatever page is in front.** A header, one
  vertical `ScrollArea` with `auto_shrink([false, false])` and an explicit max
  height, a footer. Each pane used to size itself — two had no scroll area at
  all and simply stretched the modal, and the two that had one sized it from
  whatever space was left over, which is a different number on every page. The
  scroll area claiming its space whatever is in it is the whole fix, and being
  vertical it also cannot produce a horizontal bar. Panes must not add scroll
  areas of their own: nested scrolling makes the wheel mean two things.
- **`egui::DragValue` is the one stock widget used on purpose.** The design has
  no numeric field, and a canvas size is one of the few values here that people
  type exactly rather than feel for on a rail. Everything else in those dialogs
  is `widgets.rs`'s.
- **The brush list's samples are a real stroke, drawn by the real dab
  generator.** `umber_core::preview` is the path — a sweep with a full turn
  folded into it, so it crosses itself and has a direction that changes, which
  is the only way a rake, a chisel or an angle-following brush previews as
  anything but the bar every brush used to draw. It is a parametric curve rather
  than a traced bitmap for the reason everything else here is computed: an asset
  would have nothing to check it against, where the loop, the continuity, the
  absence of a cusp and the taper are all pinned by tests that need no window.
  `widgets::preview_dabs` runs it through `StrokeBuilder` in *document* units so
  no dab is distorted by the preview's scale, and seeds the RNG identically on
  every row, so two rows differ because their settings differ and the list does
  not shimmer as it scrolls.
- **The preview rasterises coverage with a `max` and applies opacity once**, and
  that is not optional now that the path overlaps itself: one translucent shape
  per dab compounds exactly as the wet-layer scheme exists to prevent. It is a
  deliberate second implementation of three rules for a thumbnail — a GPU pass
  per row is not the answer at 201 presets in a list that scrolls — and the only
  place in Umber where a second copy of any of this is allowed to live. The
  coverage is cached against the brush's address and validated by value, so an
  edit in the brush editor redraws next frame; a stamp brush's mask is still one
  texture per mask cached by `Arc` identity, and a row scrolled out of view
  returns before any of it.
- `ResponseCurve` is a fixed array of evenly spaced samples, not free control
  points. That keeps `Brush` `Copy`, makes sampling a lerp with no search, and
  means the editor's handles move only vertically — so the curve can never be
  dragged into mapping one pressure to two values. Do not "improve" it into a
  `Vec` of points without solving all three.

- **The Palette module's library is a directory of `.gpl` files, and that is a
  different argument from the brush library's.** `UserLibrary` is a directory
  because a bitmap tip does not go in a text file; a palette holds no bitmaps,
  so the question had to be asked again. It is still a directory because **the
  interchange format is the storage format** — one decoder and one encoder, the
  rule `docformat` states as "there must never be a second ORA reader" applied
  where the format Umber reads and the one it writes are the same. An edit
  rewrites one small file rather than an index holding every palette, and the
  files are ordinary files somebody can hand to GIMP. Anything Umber-specific
  goes in a `#` comment, which every other reader ignores exactly as they ignore
  the `umber-` attributes.
- **A swatch is eight-bit sRGB, not a `Color`.** A palette is stored and shared
  rather than painted with, so holding it in the form it is written in makes the
  round trip exact by construction; a linear `Color` would be quantised out and
  dequantised in, and the colour you clicked would not be the colour you got.
  The same argument the clipboard makes, through the same
  `Color::from_srgb_u8`/`to_srgb_u8` pair — never a second `powf`.
- **A stem is free only if no *file* holds it, not if no palette loaded.**
  `PaletteLibrary::occupied` keeps the stems of every `.gpl` seen and not read.
  Without it, a file the library has just warned it could not read is a name
  `free_id` hands straight out, and the next write renames over it — the artist
  is told their palette is unreadable and then it is destroyed, in the same
  session. `a_file_that_would_not_read_is_never_written_over` guards it.
  **`read_gpl` does not refuse an empty palette**, because `create` writes one
  out empty so that naming a palette is enough to keep it; a reader that refused
  its own output made every new palette vanish on the next launch.
- **A harmony is a function of hue alone**, which is why `umber_core::harmony`
  has no drawing in it and why the picker must read `Editor::hsv` rather than
  the colour — hue is undefined for greys, so a mode reading the hue off the
  colour would offer a red harmony for every grey. Saturation and value are
  carried across every member unchanged. That makes a harmony of a grey a row of
  identical greys, which is correct, and is why the swatch in hand is marked by
  a **bar under it rather than a border round it** — an accent border is
  invisible whenever the colour is near the accent, and a warm ochre is exactly
  what somebody reaches a harmony wheel for.
- **`color::wrap_hue` is the one door a hue comes through.** A bare `rem_euclid`
  gets two cases wrong and both end in `sin_cos` and then in vertex positions,
  where one NaN is a mesh egui discards whole — a picker that has silently
  stopped drawing. `NaN.rem_euclid` is NaN, and a tiny negative rounds up to
  exactly `360.0`, which `to_color` reads as the sixth sextant and paints
  magenta.
- **Nothing is shipped, and the rule a shipped half would have to follow is
  written down anyway.** A shipped palette is `include_str!`'d and replaced
  wholesale by every update, so anything the user decides about one cannot live
  where the palette is. Recorded before rather than after, because the failure
  is silent and months late — the argument `Library::collections` already makes.
- **A palette can be arranged, and a move is a permutation.**
  `Palette::move_swatch` takes the colour out and puts it back at the index it
  *lands* at, so every colour survives, none is duplicated, and the `.gpl`
  written afterwards is the same bytes in a different order. `can_move_swatch`
  sits beside it sharing the rule, the arrangement `plan_reorder`/`can_reorder`
  already keeps.
- **The drag is `swatchdrag.rs`, a model with no drawing in it**, and it is
  `layerdrag`'s shape with one axis more. It keeps the two hit tests for the
  same reason — the palette picker is a full-width dropdown directly above the
  grid, so a press that rounded would turn using it into dragging the first
  colour. What it does *not* keep is the clamp: **a drop reaches one gap and no
  further, never the grid's bounding box.** Eleven colours four across leave an
  empty cell at the end of the last row, inside the box and *exactly*
  equidistant from the last colour and from the one above it — an exact f32 tie,
  because `swatch_rect` uses one `step` for both axes — so nearest-cell answered
  on iteration order and "drop it at the end" put the colour three places from
  the end. A test found that, which is the right way round.
- **A press on a corner mark is not a press on the colour.** The marks sit
  inside the cell, so containment alone accepts one — and **egui calls a press a
  drag on *time***: `is_decidedly_dragging` is true once `max_click_duration`
  has passed with the button held, whatever the pointer did. Holding Remove
  while deciding and letting go a cell over therefore rearranged the palette
  instead of removing a colour, silently, with the file written on the spot.
  `drag_origin` subtracts both marks. The layer list has the same shape with the
  eye inside its row and gets away with it, because a reorder there records an
  `EditKind::MoveLayer` and **there is no undo for a palette anywhere in
  Umber**.
- **The write happens at the drop, and only where something changed.** A
  `PaletteLibrary` write reaches the disk immediately — that is the whole shape
  of a directory of `.gpl` files — so a drag that saved as it aimed would be a
  file write per mouse move. `edit_current` takes the `bool` the model returns
  and writes nothing on a `false`; ignoring it meant pressing Enter on an
  unchanged name rewrote the artist's palette.
- **The drop mark is a dashed accent ring in the gap *around* the cell,
  square-cornered.** Three departures from `panels::drop_slot`, each forced.
  Around rather than over, because that wash of the accent would **tint the
  colour**, and a grid whose colours are not the colours they say is the one
  thing this panel must never do. Dashed, because the grid already draws a solid
  accent outline meaning "this is the colour in hand". Square, because at a
  pixel and a half's offset a rounded ring traces the swatch's own outline.
  `drop_ring_rect` is the single statement of the geometry **because its guard
  has to measure what is drawn** — the test recomputed the expression at first,
  and widening the real ring to swallow the neighbour left every assertion
  passing.
- **A colour is named in the panel and not in the library modal.** The modal is
  the library *of palettes*; its rows are palettes and it draws a palette's
  colours as a band nobody can point at. The field goes under the grid, which is
  the last thing in the panel body, so nothing above it moves when it opens.
  **The field is settled before the grid's click lands** — one slot for a lost
  focus and a pressed mark is `library_list`'s recorded bug, and here it
  discarded the typed name on every click. **Only the name mark stays out while
  it is open**, never the remove mark: that one is destructive with no undo
  behind it and would sit one slip from the field being typed into.
- **`Swatch::name` goes through the file writer's own rule on the way in.**
  `clean_line` is what `to_gpl` writes by, so what the panel shows is what a save
  and a reopen give back. **Empty is a real answer for a colour and not for a
  palette**, which is why `one_line` — `clean_line` plus the `UNTITLED` fallback
  — is a separate function a swatch never uses. Not hypothetical: `to_gpl` used
  to test the raw name for emptiness *before* cleaning it, and a control
  character is not whitespace, so a colour named `"\u{7}"` out of somebody
  else's `.gpl` was written back out called "Untitled palette".
- **A harmony goes in whole or not at all, and its mark is in this panel's
  header.** `Palette::add_all` refuses a set that will not fit rather than adding
  what it can: half a relation is a fragment with nothing on screen to say which
  members are missing. It is here rather than beside the wheel because
  `colorpicker` draws pickers and knows nothing about a library. Every member
  **including the base** comes off `Editor::hsv` and not `Editor::color` — a
  harmony is a set of hues at one saturation and value, so a base taken from
  anywhere else would not be on the same wheel as the rest of it.
- **`Palette::columns` is the one field of this shape still not authorable.**
  `.gpl` carries it, `grid_columns` honours it as a maximum and the writer
  writes it back; nothing sets it. Named here so the next person does not
  rediscover it.
- **There is one adding mark and its glyph says what it will add.** In Harmony
  mode it is the harmony mark and puts the whole relation in through
  `Palette::add_all`; otherwise it is a plus and puts in the colour in hand. A
  plus that sometimes adds five colours is the control whose behaviour depends on
  state you cannot see — and the state it follows is the *Colour* panel's picker
  mode, which is kept in prefs and survives that module being taken out of the
  layout, so the mark itself has to say. Every member including the base still
  comes off `Editor::hsv`.
- **The grid is read-only until a pencil in the header says otherwise, and it
  resets every run.** There is no undo for a palette anywhere in Umber and a
  `PaletteLibrary` write reaches the disk on the spot, so a remove is
  unrecoverable — with a mark sitting *inside* a swatch a pixel from the colour
  it throws away, and egui calling a press a drag on **time** alone. With editing
  off the corner marks are not *allocated*, not merely unpainted, and no cells
  are collected, so the drag model is inert rather than invisible. Off is the
  safe state, so the flag lives in egui's temporary store: never written to
  prefs, and an evicted store brings the module back read-only. Taking a colour
  is not gated — what is left is a palette you can paint out of and cannot
  damage. **The gate has to be tested at every mark, including the header's**:
  `a_read_only_palette_cannot_be_changed_by_any_gesture` drives `panel`, and the
  adding mark is in `header_controls`, which nothing under `cfg(test)` called at
  all — deleting its gate left 1,955 tests green while a press wrote a colour
  into the artist's `.gpl`.
- **Import reads six formats and export writes one, and that asymmetry is the
  point.** `umber-core::palimport` converts *into* `.gpl` on the way in, so "the
  interchange format is the storage format" still holds: one storage decoder, one
  encoder, and the readers are conversions rather than a second library. Which
  six was decided by what the generators artists use actually hand out — Coolors
  exports a URL, CSS, SVG, PDF and `.ase` and **no `.gpl` at all** — not by a
  list of extensions. `.act` is refused because it pads with zeroes and a padded
  entry is indistinguishable from a real black; `.kpl` because its floats are
  stated against ICC profiles in the zip and Krita writes `.gpl` too.
- **The highest-value reader is not a file format.** A list of hex codes is what
  a Coolors URL, a CSS dump, a `.hex`, a Paint.NET `.txt` and a message in a chat
  window all are, so one tolerant parser reads all of them. Two rules carry it.
  **A bare hex run is a colour only on a line holding nothing else** — `facade`,
  `beefed`, `accede` and `deadbeef` are words made only of hex digits, and
  trusting a bare run in prose puts colours nobody chose into somebody's palette.
  **Eight digits are read by whether they were prefixed**: `#RRGGBBAA` is CSS, a
  bare `AARRGGBB` is Paint.NET, and one rule is right in both worlds where a mode
  flag per file type would have been wrong for whichever of the two somebody
  pasted rather than opened. CSS's four-digit `#RGBA` is deliberately not read at
  all: `#1234` is an issue reference and `#cafe` is a selector.
- **A URL is unwrapped only once it has been shown to be a palette.** Its last
  path segment is handed to the scanner as a line of *bare* codes, which switches
  off the rule above — so without a test there, `wikipedia.org/wiki/Facade` is a
  pink and a short git commit hash is two colours. Twelve hex characters in a
  path is a commit hash far more often than a pair of colours.
- **The paste is a field and not a button that reads the clipboard**, and
  `arboard` being already present is what makes that a decision. A one-click
  version reaches into the system clipboard and makes a file from whatever it
  found with nothing on screen between the two; a field shows what is about to be
  read, can be corrected, takes a link typed by hand, and needs no clipboard code
  — which keeps `sysclip`'s "no test may touch the real clipboard" *structural*
  here rather than obeyed.
- **A flat struct of loss counts has the same partial-exhaustiveness trap an enum
  does.** `Losses::any` is `self != default` and total by construction;
  `sentences` was six hand-written `if self.field > 0` blocks, so a seventh field
  would be *silently* absent — a paste raising a notice with a heading and no
  lines, and an import saying nothing at all. It opens with a destructuring `let`,
  which makes a seventh field a compile error. The derived reading and its
  hand-written twin is the shape to look for.

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

- **`UMBER_TEST_SOFTWARE=1` runs the whole suite on the software rasteriser,
  and it is how a CI failure is reproduced before it is pushed.** GitHub's
  runners have no graphics card, so every one of these tests runs there on WARP
  or lavapipe while this machine runs them on hardware. The two agree about what
  the shaders *do* and disagree in the last bit of floating point — so an
  assertion comparing exact bytes can pass here and fail there, which is how
  v0.0.5 was tagged broken. It reaches the adapter through `Gpu::with_adapter`
  and `gpu::Choice`, so the limits and the device description stay stated once;
  `Gpu::new` is unchanged and everything shipped still asks for the best adapter
  there is.
- **An assertion about pixels that have been through a shader may not promise a
  byte.** Where coverage is only ever 0 or 1 the arithmetic has nothing to round
  and exactness is real — `a_hard_edged_rectangular_lift_is_exact` is the
  pattern, and it holds on both adapters. Where an edge is antialiased, compare
  the **alpha** (linear 8-bit even in an sRGB format, hence `alphas`) and allow
  the one level the store can round by. Do not compare colour at a nearly
  transparent pixel at all: at an alpha of two, one level of it moves the
  encoded byte by six, which says nothing about the code.
- **Nothing here may assert wall-clock time on CI.** A runner is a machine
  nobody chose under a load nobody controls. `a_capture_of_a_large_document_
  never_costs_a_frame` states the machine-independent half — that the capture is
  *spread* across frames — and gates the millisecond figure on not being CI and
  not being a software adapter. v0.0.2 was tagged broken by the version that
  did not.

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

**A test that writes a process-global must take a lock, and the harness will
not tell you it does not.** `prefs::apply` publishes the undo budget through
`history::set_default_budget`, deliberately — a `History` is built by three
things that cannot see a `Prefs`, the shape `shortcuts::publish` also has. The
cost is that every test calling `apply` writes one variable while the harness
runs them on parallel threads. Measured before it was fixed:
`the_undo_budget_reaches_the_history_and_back` failed **10 runs in 40** at
sixteen threads — it publishes 1024 MB and asserts it, and three other tests
publish the default in between. **It passed every whole-workspace run**, because
six hundred other tests change the interleaving, which is the worst shape this
can take: green on the gate, red on whoever next runs `cargo test prefs`. The
fix is `prefs_lock()`, `gputest::lock`'s idiom for a global that is not a
device. When adding a test that touches one, filter the binary down to its own
module and run it twenty times before believing the suite.

**That rule is per test *binary*, and `umber-app` is a second one.**
`umber_app::gputest` is its copy, and it exists because the rule was not applied
there: `autosave`'s frame-loop test was the only thing in the crate that wanted
a device until `thumbs`' arrived beside it, and two tests each building and
tearing down their own — concurrently, which is what the harness does — killed
the binary with `STATUS_ACCESS_VIOLATION` at process exit on the ARM64 Windows
runner. Every test passed and the run failed on the way out, which is the worst
shape of this bug, and it is invisible on a desktop driver. Anything here that
wants a device takes `gputest::lock()` and holds the guard for the whole test.

`prefs_lock` is `pub(crate)` and lives beside `set_undo_budget` rather than
inside `prefs::tests`, because **two mutexes serialise nothing** — a lock only
orders the tests that take the same one, and `settings`' undo row writes the same
global through the same door. Anything that draws the General pane takes it.

**A guard that restates the panel's own rule inside the test can only agree with
itself.** This is `pressing_bold_actually_puts_a_heavier_mark_on_the_canvas`'s
lesson and it recurred four times in one session, so it is worth stating as a
method rather than as an anecdote:

- **Measure the output, never restate the rule.** Two canvas-dialog guards
  claimed to pin the dialog's bounds and drew nothing — one asserted about
  `max_dpi`'s return value while claiming to pin the field's range, the other
  copied the panel's own filter into its own loop. Deleting either filter left
  both green, and the visible result was a 16384 button on a 4096 device. What
  catches it is reading the text egui actually drew: `ctx.run_ui` returns a
  `FullOutput` whose `shapes` carry every galley, so asking whether a label is
  there is a genuine panel test that needs no window.
- **A guard on a palette is not a guard on the panel either, and neither
  contrast defect moved a palette.** `an_active_mark_reads_on_the_fill_it_is_
  drawn_on` measures `active_ink` and cannot see whether anything calls it:
  revert one line and every ratio still passes. So the worst call sites are
  measured off a headless pass instead — `inks_drawn` tessellates and keeps the
  opaque vertex colours, which is one field where a shape is a dozen types with
  three shapes of stroke among them.
- **A guard on a model is not a guard on the panel, and the panel is where the
  gate usually is.** `a_read_only_palette_cannot_be_changed_by_any_gesture`
  drives `panel`; the palette's adding mark is in `header_controls`, which
  nothing under `cfg(test)` called at all. Deleting its gate left 1,955 tests
  green while a press wrote to the artist's file. Enumerate the *call sites* of
  the rule, not the rule.
- **A widget drawn through `ui.new_child(max_rect(…))` allocates nothing in its
  parent**, so one taller than its slot paints over its neighbours instead of
  pushing them down — a defect no layout test can see. Measure it rather than
  looking: `inset_field` is 18.22 points at `text::TINY` against
  `metrics::SLIDER_ROW`'s 16.
- **A budget on the tool options strip is hand-measured, and a guard has to
  measure the *reserve*, not the glyphs.** The strip is one unwrapped row, so a
  budget a few points short draws a rail off the right edge rather than
  reflowing. That was theoretical while the readouts were painted labels sized to
  the figure showing; a field is sized to the widest figure its rail can produce
  and paints its galley at one end of that box, so up to three characters of
  allocated width carry no shape at all — and the group whose reserve hangs off
  the edge is exactly the last one drawn. Read the frame's own rect as well as
  the shapes, sweep a point at a time, and say out loud which strip the sweep
  declines to cover and why.
- **The cheap way to find out whether a test agrees for the wrong reason is to
  mutate the code it claims to cover.** Commit first, so `git checkout --`
  reverts the mutation and not your work — that collision is now routine enough
  to be worth the habit.

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

Three things follow, and all three were bugs:

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
- **No gesture may be decided inside a mouse event arm.** This is the general
  form of the other two, and it shipped three gestures a tablet could not reach:
  the Alt-drag brush resize, the Pan tool and the Zoom tool all worked under a
  mouse and did nothing under a pen — the resize worse than nothing, since the
  press fell through to painting. `gesture.rs` is the fix and it is a *model*
  with no winit in it, the same division `dock.rs` and `layerdrag.rs` keep:
  `gesture::press` says what a press means and `gesture::contact` says which
  touch events are presses at all, so `window_event`'s two families both supply
  observations and neither interprets them. A gesture added there reaches both
  pointers or neither. **Do not add a `match` on `Tool` to either arm.**
  - **The touch arm must consult the keyboard.** Alt, Shift and Space arrive
    normally under a pen — `ModifiersChanged` is not a mouse event — and the
    touch arm simply never read them, so Alt painted and Space did nothing.
  - **A contact is not a button, and the brush resize is the one place that
    matters.** The mouse's rule is "Alt with a button is the eyedropper, Alt
    without one is the resize"; a pen has no button-less drag *on the glass* to
    spell the second with. So a contact **continues** the resize where a button
    ends it — which is how Krita and Photoshop spell it on a tablet — and the
    eyedropper is Alt with the nib down and still, settled at the release by
    `gesture::is_tap`. `a_pen_press_resolves_to_what_a_mouse_press_would` pins
    every other cell of the matrix, and `alt_with_a_contact_carries_the_brush_
    resize_on` pins that this asymmetry does not leak back onto the mouse.
  - **One contact drives the Pan and Zoom tools; two fingers are still a
    pinch.** "One finger navigates nothing" is right for a hand that has landed
    on the glass with a brush in hand and wrong for somebody who went to the
    rail and *chose* the tool. The pinch keeps precedence because a second
    contact is tested first — and it now has to reset `Interaction` as well as
    the stroke, because there is a live pan a `cancel_stroke` would not clear.
  - **A hover goes through the same body a mouse move does**, because a hover
    *is* a mouse move with nothing held. `last_cursor` is saved and put back
    across it: it is the previous point of a *gesture*, and a pen waved about in
    mid-air is not one.
- **Settings → Input & pen records which gesture a press resolved to**, beside
  the route and the motion, because "the pen arrived and became the wrong
  gesture" is invisible in the other two columns — which is exactly why the
  three above shipped. `InputLog::note_gesture` takes the answer the one real
  `gesture::press` call gave and lands it only on a sample that is itself a
  press, since `note` records the left button and touches and nothing else.
  Same rule as `note_resolved`, and observation only.

**Umber hides the arrow itself under a pen, because winit will not.** `ui.rs`'s
`pen_cursor` asks egui for `CursorIcon::None`, and that request is dropped:
winit's `refresh_os_cursor` hides only when `CursorFlags::IN_WINDOW` is set and
in the `else` branch actively *un*-hides, and `IN_WINDOW` is set true only
inside `WM_MOUSEMOVE` — which a pen never produces, since `WM_POINTER*` ends at
`ProcResult::Value(0)` after `SkipPointerFrameMessages` and never reaches
`DefWindowProc`. `egui-winit` then dedupes on the last icon it set, so the lost
request is never retried. `syscursor` therefore calls Windows' `SetCursor`
directly. Three rules come with it:

- **It is a per-frame call, never a latch.** That is what preserves the property
  `CursorIcon::None` was chosen for over `set_cursor_visible(false)`: egui
  re-derives the cursor every frame, so the arrow comes back on its own when the
  pen goes away, where a latch's failure mode is a window with no pointer in it
  and no way to say so.
- **Focus belongs in the *request*, not beside the platform call.** Putting it
  at the platform call meant nothing ever un-hid: Alt-Tab away with a pen
  hovering and `pen_dot` was still `Some`, egui's icon was unchanged, its dedupe
  skipped `set_cursor`, and the NULL cursor stayed in force **across the whole
  desktop**. Folded into `Editor::pen_dot` via `Surroundings`, an unfocused
  window asks for a real `CursorIcon`, the dedupe passes and winit restores the
  arrow on the spot — one condition rather than two that can disagree. The
  reading is `ctx.input(|i| i.focused)` and **not** `i.viewport().focused`,
  which looks more direct and is always `None` because it is filled in by
  `update_viewport_info`, which `render` does not call.
- **`Memory::layer_id_at` is not a hit test while a modal is open** — it returns
  the modal's own layer for *every* point in the window, open canvas included.
  So anything reading `over_egui_area` gets `true` everywhere whenever a dialog
  is up. That is correct for `pen_dot` and for `ui_owns_pointer`, which both
  mean "refuse", and it silently killed the Settings readout, which meant
  "observe": the row could only ever say the ordinary pointer, and its tooltip
  then blamed the pen's position. The pane skips obscured frames and carries a
  **count** of frames that asked to hide, because `egui::Popup::menu` is an
  ordinary `Area` rather than a `Modal` — so walking to the pane through the
  menu records the pen over the menu and clobbers the one reading somebody
  opened it to see.
- **No test can fail if `syscursor` is deleted.** The CPU tests cover which
  cursor Umber *decides* to ask for; the platform call that is the actual fix is
  not testable here, and `pen_dot`'s "checkable without a window" must not be
  read as covering it.

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
checks the tree, the version and the notes, runs the same gates CI runs, pushes
`main`, **waits for CI to pass on that very commit**, and only then writes and
pushes an annotated `v<version>` tag. It uploads nothing, so it cannot
half-publish; `.github/workflows/release.yml` does the rest.

- **The tag waits for CI, and this is the rule the other three were learned
  from.** The gates in the script run on one machine, and every release that has
  gone wrong went wrong on a platform that machine is not: 0.0.2 on a timing
  assertion on macOS, 0.0.4 on code that only compiled on Windows, 0.0.5 on a
  GPU test that only rounds that way on hardware. All three were green locally,
  all three were tagged, and all three were found out afterwards. A local pass
  is therefore not evidence, and the tag waits for one that is. Nothing is spent
  on a red run: the commit is on `main` either way and the fix is another commit
  and another run of the script. It needs the GitHub CLI for this and says so;
  `-SkipCi` / `--skip-ci` opts out, and is for a machine without `gh` rather
  than for a hurry.
- **The README links to every file of the current release, and two tests keep
  it true.** GitHub's permanent `releases/latest/download/<name>` form needs a
  filename that never changes, and Umber's carry the version — which is worth
  keeping, because months later it is what says which build is in somebody's
  downloads folder. So the links name a version, and
  `the_readme_links_to_every_file_of_this_release` and its `..._no_download_
  link_that_is_not_...` twin fail the build when they name the previous one.
  Both directions are needed: the first catches a row that was not updated, the
  second a row left behind for a package that is no longer built. `ASSETS` in
  that file is the **third** statement of the asset names, after `release.yml`
  and `update::release::wanted_asset`; changing a name means changing all three.
- **`ci.yml`'s matrix must cover every runner `release.yml` builds on**, or the
  wait above is a gate with a hole in it. v0.0.5 was tagged on a green CI and
  then failed on `windows-11-arm`, which CI did not run at all. Adding a target
  to the release matrix means adding its runner to CI in the same commit.
- **The two scripts have to stay in step**, `release.ps1` and `release.sh`, the
  same arrangement `tools/fetch-brushes.*` keeps.
- **`release.ps1` wants PowerShell 7.** Under Windows PowerShell 5.1 a native
  command's stderr becomes an error record, so `$ErrorActionPreference = 'Stop'`
  aborts the script on cargo's ordinary progress output. Do not "fix" that by
  redirecting or by loosening the preference — the preference is what makes a
  failing gate stop the release.

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

### Verifying a release actually landed

The script's CI wait is what stops a *bad* tag being spent. It says nothing
about whether the workflow then built anything, and pushing the tag is the point
at which walking away has already gone wrong once. So:

- **Before running the script, re-run the whole suite with no escape hatch, on a
  quiet machine.** `CI=1` is legitimate for gating a merge while several agents
  are loading the box, and it is *not* legitimate for deciding a release: it
  runs only the machine-independent half of the timing assertions, which is
  exactly the half that was never the risk. If shaders changed, run
  `UMBER_TEST_SOFTWARE=1` over `umber-render` as well — CI has no graphics card,
  and the last bit of floating point is where hardware and lavapipe disagree.
- **Build the Windows installer and run it before tagging.** It is the one
  artefact whose failure is total — a setup executable that does not install is
  a release nobody can take — and it shipped broken in 0.0.8 because it had
  never once been launched. `wix` 5.0.2 with the UI and Util extensions is all
  it needs beyond the release build:
  ```sh
  cargo build --release -p umber-desktop
  mkdir -p wixassets dist
  cp assets/icons/umber.ico wixassets/
  cp packaging/windows/banner.bmp packaging/windows/dialog.bmp wixassets/
  sh packaging/windows/make-licence-rtf.sh LICENSE wixassets/licence.rtf
  wix build packaging/windows/umber.wxs -arch x64 \
    -ext WixToolset.UI.wixext -ext WixToolset.Util.wixext -pdbtype none \
    -d Version=$V -d BinDir=target/release -d DocDir=. -d AssetDir=wixassets \
    -o dist/umber-$V-x64.msi
  cargo run --release -p umber-app --example make-setup -- \
    target/release/umber.exe dist/umber-$V-x64.msi dist/umber-setup-$V-x64.exe
  ```
  Then **double-click it** — not `--install`, because a double-click is the case
  that was broken and the flag is the case that was not. `dist/` is ignored, so
  this leaves the tree clean.
- **`pwsh` may not be installed.** `release.ps1` needs PowerShell 7, because
  under 5.1 a native command's stderr becomes an error record and
  `$ErrorActionPreference = 'Stop'` aborts on cargo's ordinary progress output.
  Do not loosen the preference — that is what makes a failing gate stop the
  release. Run `sh tools/release.sh` instead, which is also the only thing that
  exercises the half of the pair a Linux or macOS machine would take.
- **Do a `--dry-run` first.** It runs every gate and every check and touches no
  remote, so the only thing left to fail afterwards is the network.
- **Watch the release workflow to completion, not just the CI wait.** They are
  different runs against different matrices: CI proves the code compiles and
  passes everywhere, the release workflow proves fifteen artefacts can be
  *built and packaged*, and the Flatpak and Arch jobs exist only in the second.
- **Then verify what was published, rather than assuming the green tick.**
  Every job's conclusion; the asset list compared against `ASSETS` in
  `crates/umber-desktop/tests/release.rs`, which is where their names are
  stated; that the release is not a draft and not a pre-release; that the notes
  are the changelog section verbatim rather than an empty body; and — this is
  the one worth doing by hand — **fetch some of the README's download links and
  check they answer 200**. The tests prove the README *names* the right files;
  only a request proves the files are there. A 404 on the front page is a worse
  first impression than no link at all.
- **Clean up afterwards.** Remove the agent worktrees, delete the merged
  branches and reclaim the target directories. A stale `.claude/worktrees` entry
  is what makes the *next* fan-out fail at `mkdir`.

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
  fixture, the install detection from injected readings, the archive handling
  from archives the test builds itself, and the dialog's whole state machine
  from `update::flow` — see below.

#### Installing on Windows, in Umber's own window

`umber-app::update::installer` is the model, `installwin.rs` opens the window
and touches the platform, `payload.rs` is the format a setup executable carries,
and `shell.rs` is the window itself. Both a first install and an update go
through the same three screens.

- **The window is `shell::Page`'s, and there is one host.** `crash/window.rs`
  had the only winit-and-wgpu-and-egui host in the crate and this would have
  been the second copy of it, so the host moved to `shell.rs` and the crash
  reporter became a `Page` beside the installer. A `Page` has no wgpu in it,
  which is what makes what a window *says* checkable without a device.
- **A running executable cannot be replaced, so the installer cannot be
  Umber.** That is the whole reason this is a second process, exactly as the
  crash reporter is: `--install-update` is the helper an update spawns, and
  `--install` is `umber-setup.exe`. Nor could the package do it — the MSI's
  "Start Umber" action is published on the exit dialog's Finish button, so a
  silent install has no UI sequence to fire it and nothing would relaunch
  anything.
- **The helper runs from a copy in the temporary directory.** Not tidiness: it
  would otherwise be a file *inside* the installation the package is replacing,
  which Windows Installer finds in use and either reboots around or has Restart
  Manager kill mid-window. `stage_helper` is the copy, and it is also why the
  helper is told where to relaunch Umber from — its own `current_exe` is the
  updater.
- **`msiexec /qn /norestart` with a verbose log, elevated through
  `ShellExecuteExW`'s `runas` verb.** The elevation is not optional and a plain
  spawn will not do it: a per-machine MSI started from an unelevated Umber fails
  as "you must be an administrator", *silently*, because `/qn` has no interface
  to say it in. `/norestart` is load-bearing too — a painting application that
  reboots somebody's machine to finish updating itself is indefensible, and a
  failed install is recoverable where a reboot is not.
- **The UAC prompt stays and is explained before it appears.** It is a Windows
  security guarantee and not ours to suppress. Saying so is the difference
  between a consent dialog somebody expects and one that arrives unexplained.
- **The bar is empty while `msiexec` runs.** Nothing reports progress out of a
  silent install, so `Step::progress` answers `None` and the track draws empty —
  the rule `Stage::progress` already follows for `HandingOver`. A bar that moved
  anyway would be inventing somebody's installation.
- **An update starts at once and setup waits to be asked.** An update was asked
  for and the artist is watching a countdown; setup was double-clicked by
  somebody who has agreed to nothing, so it opens on `Step::Ready` with Install
  and Cancel. Putting files on a machine because a window opened is the wrong
  way round.
- **`payload::append` and `payload::read` are the one statement of the format**,
  which is why `examples/make-setup.rs` is a Rust example rather than the pair
  of shell scripts `tools/` keeps: two scripts would be two more statements of a
  byte layout. The length is at the *end* because the start is not ours — a PE's
  length changes with every build — and Windows loads a PE by its headers, so
  appended bytes are ignored and `umber-setup.exe` still runs as Umber.
  **The magic is not a signature and must not be described as one**: it says
  something appended this deliberately, and nothing whatever about where the
  package came from. Umber does not sign its releases.
- **`umber-setup-<version>-<arch>.exe` is a fourth statement of an asset
  name.** `release.yml` builds it, `ASSETS` in `crates/umber-desktop/tests/
  release.rs` lists it and the README links it; `update::release::wanted_asset`
  deliberately does **not**, because the updater fetches the `.msi` and not the
  setup binary. The version in that filename is read back by `unpack_payload` to
  head the window, so the name is load-bearing rather than decorative.
- **It has now been run, and 0.0.8's shipped broken in three ways. All three
  were things the reasoning had already covered and got wrong.** Everything
  that can be decided without a release still is — the command line, the
  `msiexec` arguments, which steps hold the window open, that no step claims
  progress it cannot see — but the record below is what that turned out to be
  worth. Build one and run it before believing this path again:
  `wix build packaging/windows/umber.wxs …` then `make-setup`, both exactly as
  `release.yml` does them.
  - **Dispatch went by a flag nothing passes.** `--install` was the only way
    into setup, and setup is *double-clicked*, which passes no command line at
    all — so `umber-setup.exe` started the application. The comment beside the
    check said "No arguments" while the code required one. **What tells the two
    apart is the payload**, so that is what `installer::job` asks; the flag
    remains for asking deliberately. `payload::carried_by` reads sixteen bytes
    off the end and `umber.exe` pays one seek per launch to answer no.
  - **`msiexec` was handed quoted switches.** `parameters()` quoted every
    argument, and `msiexec` parses its own command line rather than going
    through `CommandLineToArgvW`: it will not read `"/i"` or `"/qn"`. It raised
    its usage dialog, which an elevated process draws where nobody can see it,
    and `WaitForSingleObject` waited on it for ever. Consent given, then
    nothing. **A path is quoted and a switch never is**, and the test that
    should have caught it asserted the opposite — `every_parameter_is_quoted`
    required `"/i"`, so the guard held the defect in place.
  - **The window was fixed at 260 points for steps that draw 142 to 204.**
    Measured, by running each `Step` through `run_ui` and taking the tallest
    shape. `Page::fits_content` is the opt-in, height only and never upward;
    see `shell.rs` for why width could not settle and why `used_rect` is the
    wrong reading here.
  - **`make-setup` now asks the written file `carried_by`, not only the bytes
    `read`.** Those are different questions and only the second one was being
    asked: the format was perfect in the build that shipped broken. A writer
    that checks its output against the *runtime's own* predicate is the rule
    worth taking from this, not the three bugs.
  - Still uncovered: the two `unsafe` calls, and whether `msiexec` actually
    installs. A green build says the artefact will be recognised, not that the
    package is good.

#### The update dialog

`umber-app::update::flow` is the model — which screen, which stage, the
countdown, and which actions this installation may be offered — and
`updatedlg.rs` paints it, with no drawing in the one and no decisions in the
other. Same division `dock.rs` keeps against `panels.rs`. It is what makes an
update testable at all: nobody here can cut a release to run the real thing
against, so an offer → download → unpack → install → countdown → restart, and
every failure and cancellation off it, are `Flow`'s tests and need neither a
window nor a socket.

- **There is one record of what an update is doing.** `Status` is the *check's*
  outcome and nothing else; the flow is the update's. `Status::Downloading` and
  `Status::Applied` used to exist beside a dialog that also tracked them, which
  is two things to keep in step and one of them eventually stale. About's update
  section reports and hands over; it does not run a second, smaller update.
- **Only the worker decides how an update ended.** Cancel *asks*
  (`Phase::Stopping`) and the thread answers. Marking the flow cancelled at the
  click would let the dialog say a release was not installed in the half second
  after it was — the one case a boolean on the button gets wrong.
- **A cancel is only offered while stopping costs nothing.** `Stage::can_stop`
  is true up to and including the length check, because the download is held in
  memory and nothing has been written; from the unpack on, the control comes off
  the screen rather than being drawn and refused. A stop landing mid-swap is the
  one outcome that costs somebody their installation.
- **Progress is throttled to one report per whole percent.** The rule that the
  check must wake the loop applies to every byte that arrives, and a wake per
  64 KiB chunk is five hundred full-interface redraws on a 30 MB release. A
  hundred is enough to move a bar.
- **The bar never animates over something it does not know.**
  `Stage::progress` returns `Option`, and `None` draws an empty track. The one
  place Umber genuinely cannot report progress — Windows' installer, once
  `msiexec` has the package — is `Stage::HandingOver`, a stage that *says* so,
  and then a completion screen that says it again. A progress bar that lies is
  the class of control this project refuses everywhere else.
- **Nothing on screen calls anything verified.** Umber does not sign its
  releases, so the stage that compares lengths is called checking the length,
  the footnote states HTTPS and an address from the API and a size, and
  `no_stage_calls_anything_verified` fails the build on "verif", "authentic",
  "secure", "signed" or "signature" appearing in a stage label.
- **"Never ask again" writes `check_on_startup`**, the same preference Settings,
  General shows and can undo. A second switch is two things that can disagree
  about whether Umber checks.
- **`Applied::Restart` restarts and `Applied::Installer` closes**, and the
  difference is not cosmetic: a copy Umber replaced itself is at its own path
  and can be started from there, while the MSI cannot touch Program Files until
  Umber is gone and offers to start the new version itself. `relaunch` therefore
  *reports* rather than exiting — `app.rs` exits only on `Ok`, so an update that
  could not start the new copy leaves the old one running instead of leaving the
  user with none. Same guarantee `swap_in`'s rename-then-replace makes.
- **The dialog cannot be dismissed while work is in flight.** Escape and the
  click outside are refused by `Flow::holds_work`; a modal that vanished
  mid-download would leave a thread running with nothing on screen to stop it.
- **`Phase::Stopping` carries the stage it was stopped from**, so the bar holds
  the reading it had while the worker answers. Emptying it reads as a reset, and
  the download is still running until the worker says otherwise. That is also
  why `working` takes a fraction and a line rather than a `Stage`: on that one
  screen the two come from different places.
- **The notes are the release's own**, out of the API reply — which is
  `CHANGELOG.md`'s section, published verbatim by the workflow. The changelog
  compiled into the binary describes the build already *running* and is exactly
  the wrong thing to show. They go in one vertical `ScrollArea` with
  `auto_shrink([false, false])` inside a box `theme::metrics` sizes, for the
  reason `BRUSH_LIBRARY` is fixed: it is text nobody here wrote.
- **The rehearsal menu is `debug_assertions` only.** Help → "Update dialog
  (debug)…" walks the model to each screen with a release that does not exist,
  because otherwise every one of them ships having been reasoned about and never
  looked at. A compile-time gate rather than a hidden preference, which is a
  thing somebody finds.

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

## Working in parallel

**Independent pieces of work get an agent each, run at the same time.** A
session that arrives with six unrelated tasks in it is six agents, not six turns
of one conversation — wall-clock time is the thing being spent here. This is a
standing instruction and it overrides the usual reluctance to delegate.

- **Trees are allowed and often better than a flat fan-out.** A planner feeding
  an implementer, a critic reading an implementer's diff before it is reported,
  a supervisor over several implementers — all of these are fair game. Use one
  where the task is large enough that a first draft nobody read is likely to be
  wrong.
- **Overlapping files mean a worktree each.** Two agents editing `panels.rs` in
  the same checkout will silently overwrite each other, and neither will notice.
  Give each `isolation: "worktree"`, have it commit on its own branch, and merge
  the branches one at a time afterwards.
- **Every worktree needs its own `CARGO_TARGET_DIR`, and this was learned the
  hard way.** Sharing one across concurrent worktrees looks like the obvious
  saving — `[profile.dev]` builds dependencies at `opt-level = 3`, so six fresh
  worktrees is six builds of wgpu — and it does not work. Cargo's lock
  serialises the *builds*, but the workspace crates are keyed such that one
  worktree is handed another's artefacts: the symptom is a compile error naming
  a module that plainly exists ("no `thumbnail` in the root"), which reads as a
  bug in your own change and is not one. Two agents' `umber.exe` also hold the
  same binary against relink, which produces `Access is denied` on the link step
  and tempts somebody into `taskkill`, killing another agent's session. Pay for
  the six dependency builds. Shell state does not survive between tool calls, so
  whichever directory is used has to be set on each invocation:
  `$env:CARGO_TARGET_DIR='…'; cargo test`.
- **An agent's report is not the same thing as a merged change.** The gates
  (`fmt --check`, `clippy`, `test`) run in the worktree *and* after the merge —
  a clean branch and a clean merge of several clean branches are different
  claims.
- **Brief every worktree agent to `git merge --ff-only main` as step zero.**
  This is the *fix*; the check below is only the diagnosis, and stating the
  diagnosis is what I did the first time it happened, after which it happened
  again the same day to two more agents. A worktree is a real git worktree, so
  the agent can reconcile itself in one command and usually fast-forward,
  because it has no commits of its own yet. Put it in the brief and the whole
  class goes away.
  **The sharpest confirmation is not a hash comparison — it is whether the
  module the brief is built on exists at all.** Both stale agents found their
  base by noticing that `textobj.rs`, which their brief forbade them to touch,
  was not in the tree. `git merge-base --is-ancestor HEAD main` returning true
  with a large gap is the one-command version.
  **And the real damage is not the merge, it is that the brief tells the agent
  to read a stale file.** One of them was told to read this file's "Text"
  section; its copy still said "Nothing is kept as text", which had been
  retracted the day before, so it would have designed against a premise the
  project had abandoned. A stale worktree does not merely lack code, it carries
  confidently wrong *instructions*.
  **A claim about a tree the agent cannot see must name the tree.** One reported
  "`SaveLayer::text` does not exist on main" when what it had verified was "does
  not exist here", and its sibling reported the opposite. Two agents
  contradicting each other about one field is what makes a supervisor check in
  one command instead of adjudicating; without the tree named, it reads as one of
  them simply being wrong.
- **Check every worktree's base before briefing it, because it is not
  necessarily current `HEAD`.** Three agents spawned in one message came out
  with two different bases: one from `main` as it then stood and two from
  where the *session* had started, 55 commits earlier. `git worktree list`
  and `git merge-base main <branch>` is the check, and it takes ten seconds
  against a merge that took five conflicted files in the document format.
  **The tell is the agent contradicting the brief about a constant.** "The
  brief said `umber-version` is 3; it is 2 in my base" reads like an agent
  that has got confused, and it is the most useful sentence such a report can
  contain — the agent is right about its own tree and the brief is describing
  a different one. Believe it and check.
  Two consequences worth pre-empting in the brief: the agent's copy of *this
  file* is stale too, so any rule added mid-session is invisible to it; and
  whether staleness costs anything is a `git diff --name-only <base>..main --
  <its files>` away. Zero overlap means carry on — a rebase for tidiness is
  pure risk.

### The shape that worked, for twelve agents at once

Fourteen features and two bug reports arrived in one session. What follows is
the arrangement that landed nine of them, and it is written down because the
parts that mattered are not the obvious ones.

- **Split the work by what it *needs*, not by how much of it there is.** Six
  pieces were self-contained enough to implement, and each got a worktree. Seven
  could not be built blind and got a **design document** instead — three because
  they rewrite the same three things (`EditBody`, `Float`, the single composite
  pass) so building them concurrently is agents overwriting each other and
  building them serially without a design first is three half-answers; three
  because nobody here has the hardware to verify them; one because "every open
  source font we can find" needed a number in megabytes before it needed code.
  `docs/layer-folders.md` was written before folders were, for this reason.
- **Design agents work in the main checkout and must not build.** Each creates
  one new file under `docs/` and touches nothing else, so there is no conflict
  to have; and the main `target/` belongs to whoever is merging. Tell them so
  explicitly, or one will run `cargo test` and fight the supervisor for the lock.
- **Forbid every agent from editing `CLAUDE.md`, `README.md` and `CHANGELOG.md`,
  and have them report the prose instead.** Twelve agents editing one paragraph
  is twelve conflicts, and these three are exactly the files several of them
  will want at once. The supervisor applies the collected prose in one commit at
  the end — which is also the only point at which anyone can see that four
  agents have made the *same* rule false in four different ways.
- **Every implementation agent spawns a critic on its own diff before
  reporting.** This is the highest-value instruction in the whole arrangement.
  In one session the critics caught: a paste that would put down a picture the
  artist never copied; a palette library that told you a file was unreadable and
  then renamed over it; a stroke on a mask reaching a path that writes four
  channels to a one-channel slice; and a canvas flip that could delete a
  selection unrecoverably, since undoing a flip is another flip.
- **A critic that returns nothing has not reviewed anything.** One died leaving
  a zero-byte transcript and its agent self-reviewed instead — and that branch
  was the one that turned out to have two real defects, including a diagnostic
  readout that could only ever give one answer while its tooltip explained the
  wrong reason. **Check that the critic actually reported**, and run an
  independent one over anything that reached `main` without a real review.
- **Cross-wire the agents that will collide, before they start.** Structural
  undo and the linked transform both need one undo entry holding several
  patches; the Android and pen-platform research both turn on what winit
  carries. Each was told the other existed and asked to propose the shared
  shape, which produced documents that reference each other instead of
  contradicting each other.
- **Then run a supervisor over the whole design set.** It found five collisions
  none of the six authors had flagged, including that three of them rewrite
  `Editor::layer_draws` and one changes its type. `docs/roadmap-review.md` is
  that pass, and it is deliberately shorter than any document it reviews.
- **Require every report to lead with what was *not* finished.** That is what
  surfaced the navigator not being built, `.psd` masks being impossible rather
  than merely undone, and a macOS clipboard path that could darken every soft
  edge — none of which would have appeared in a summary of what was achieved.
- **An agent's finding is a lead, not a fact — verify it before acting.** Two
  live bugs were reported by agents designing something else and both were real,
  but both were checked against the source and the specification first. That
  mattered once: a web search summary said ORA's `isolation` attribute defaults
  to `auto`, and the published specification says `isolate`. The agent was
  right and the search was wrong.
- **Agents stall.** Two stopped and said they were waiting for a notification
  that was never coming. A short, direct "nothing is waiting; run this command,
  commit, and report" unblocks them; do not spawn a replacement.
- **Merge one branch at a time, gates after each.** Every conflict in nine
  merges was a two-line import list, because the worktrees were split by remit
  — but the gates after each merge are what make the two claims separate.
- **Budget the disk, and reclaim a target directory the moment its branch is
  merged.** Seven concurrent `CARGO_TARGET_DIR`s came to about 42 GB and the
  machine ran out; one agent had to be told to use another drive, and another's
  critic could not run its gates at all. The symptom is `os error 112` or a
  linker complaining about disk space, and it reads like a broken change. Ten
  target directories came to about 83 GB in a later session; reclaim them at the
  end, and note that the worktrees themselves have to be **unlocked** before
  `git worktree remove --force` will take them.
- **`git checkout -- <path>` does not work inside `.claude/worktrees/`.** The
  `.gitignore` rule for that directory matches the path as evaluated from within
  a worktree that lives there, so git refuses to restore a tracked file and says
  so in terms that sound like the file is untracked. It bit an agent whose
  `.github/workflows/*.yml` had gone missing from the working directory while
  still present in `HEAD`, which fails
  `the_release_workflow_stages_every_asset_the_installer_names` — a test failure
  that looks like a real regression and is a missing file. `git cat-file -p
  HEAD:<path> > <path>` is the way back.
- **An orphaned worktree directory is detectable in one command, and there were
  ten of them.** A live worktree holds a `.git` **pointer file**; an orphan has
  none, so git walks up and resolves to the shared checkout — which is why
  `git rev-parse --show-toplevel` answering the repository root *from inside the
  worktree* is the tell, and why `git merge --ff-only main` there reports
  "Already up to date" while meaning something else entirely. `ls .claude/
  worktrees | wc -l` against `git worktree list | tail -n +2 | wc -l` is the
  audit: seventeen against seven, in this session. **Any agent still live in one
  of those commits to `main` without knowing**, so that count is the first thing
  to check when a worktree behaves oddly, and a dead agent in one must not be
  resumed.
- **The sync client can delete a *live* agent's whole worktree, and it has.**
  Two worktrees created in one round vanished mid-session — directory, branch,
  ref, reflog and `.git/worktrees/` metadata, with no remnant — while the seven
  older ones in the same folder survived. Nothing was lost only because neither
  agent had written code yet. **The mitigation when a task must not lose work to
  the environment is to run it in the shared checkout, one agent at a time, and
  commit early and often**; a commit is the only thing that survives a sweep.
  That costs the wall-clock parallelism this section otherwise exists for, so it
  is a fallback and not the default — but a task whose files barely overlap is a
  cheap one to serialise.
  **The harness and the filesystem then disagreed**: it went on refusing the
  agent's Bash calls as "isolated in the worktree …" that no longer existed. An
  agent in that state cannot fix itself and should not try.
- **An agent refusing a brief's instruction can be the agent being right, and
  this is the case to remember.** That agent had been told, correctly at the
  time, to run `git merge --ff-only main` as its first act. By the time it ran,
  its worktree was gone and its working directory had fallen back to **the
  shared checkout** — where that command would have operated on `main` itself,
  and where checking out its own branch to merge would have moved the artist's
  working tree onto an agent branch. It refused, ran only reads, reported, and
  asked for a new tree. **A brief is written against a state of the world that
  can change under it**, so "report rather than proceed" has to beat "do as
  briefed" whenever the two conflict — and a coordinator should say so rather
  than relying on judgement.
- **The sync client is a third party to every one of these worktrees.** On a
  machine where the checkout is inside OneDrive, files vanish and reappear
  underneath running agents: eleven untracked scratch copies were swept
  mid-session with nobody running anything, and `.git/worktrees/` held a lock
  that refused every removal until the target directories were deleted first. If
  a file reads back differently or `git status` shows something nobody did,
  suspect the sync before suspecting another agent, and tell the agents so.
- **Concurrency makes the wall-clock assertions flake, and `CI=1` is the right
  answer *while merging only*.** `a_capture_of_a_large_document_never_costs_a_
  frame` failed for four agents under load and passes alone. Gating a merge on
  its structural half is fine; deciding a release that way is not — see
  "Verifying a release actually landed".
- **Never let an agent edit a file with PowerShell's `Get-Content |
  Set-Content`.** On Windows it reads as ANSI and writes UTF-8 *with a BOM*, so
  one round trip adds a BOM and turns every em-dash into mojibake. It stays
  valid UTF-8, so `fmt`, `clippy` and the whole suite pass; the only symptom is
  a two-line change committing as 109 insertions. Say so in the brief.
- **An agent may drive only a process it started itself, by the handle it got
  at launch — never one it found by name.** An agent verifying a settings page
  wrote a script that took `Get-Process umber | Select -First 1`, matched the
  *user's own installed Umber*, and sent four synthetic clicks into it; two of
  them painted strokes onto a document with unsaved work. It stopped as soon as
  it noticed and deliberately did not send an undo into a window it should not
  have been driving, which was right — but the damage was already done and only
  the user could assess it. **The default in a brief is that an agent does not
  drive a GUI at all**: a `docshot::Stage` preview or a headless
  `egui::Context::run_ui` test answers almost every question a window would,
  and does it reproducibly. This one is not about tidiness — an agent reaching
  outside its worktree can reach the artist's own work.
- **Do not clean up worktrees while any agent might still be resumed.** A merged
  branch is not a finished agent: an agent that has reported can be sent a
  follow-up, and several were. Removing its worktree mid-session leaves an
  ordinary directory *inside* the shared checkout, which `.gitignore` hides, so
  every `git` command the agent runs silently operates on **the main
  repository** — the one that caught this reported `git status` clean while its
  file differed from `HEAD` by 128 lines, and it had by then created a branch in
  the shared checkout and switched the working tree onto it. Tie the cleanup to
  the agent being finished with, not to its branch being merged, and do the
  whole sweep once at the end.
- **A doc comment that names a call site is a claim, and a wave-one change is
  exactly where it is false.** Three methods in one branch said "called from
  `mirror_document`", "what a resize does" and "the question `begin_stroke`
  asks", and none of the three was reached from outside its own crate, because
  the wiring was another agent's file. The commit message repeated all three.
  Write `Transform::reseat`'s form instead: **nothing calls this yet**, and what
  goes wrong until it does. A split remit is what makes this the *normal* case
  rather than a slip, so it belongs in the brief.
- **Commit before mutating.** An agent undoing a mutation test with
  `git checkout -- <path>` destroyed its own uncommitted work in the same
  stroke, because the file held both. It redid it from context and nothing was
  lost, but the habit is free: mutation testing is now a routine part of how
  work is verified here, and `git checkout` is how a mutation is reverted, so
  the two collide by default rather than by accident.
- **A merged branch can still be wrong in the *combination*, and only an
  independent pass over `main` finds it.** Three branches each had a critic that
  found real defects, and all three were green. Merged, `MAX_SLOTS` moving from
  129 to 256 silently changed what `ensure_slots` does without a line of it
  being touched — the `.min(MAX_SLOTS)` had been acting as a *tight* bound, so a
  legal document went from allocating 129 slices to 256. No branch critic could
  have seen it; each branch was self-consistent. **Run a reviewer over the merge
  itself**, tell it that commits made *at merge time* have been reviewed by
  nobody, and ask it specifically for claims that were true on one branch and
  are false now — that class was three of its six findings.
- **Verify the claim that failed last time, personally.** When a review finds
  that a guard covered a *copy* of the code rather than the code, do not accept
  "fixed" on report — re-run the mutation yourself. Doing so is two minutes and
  it is the only way to know the second attempt did not repeat the first's
  mistake in a new place. It has already paid for itself once here: the
  reintroduced defect that had left twenty-eight tests green did, after the
  rework, fail the one test that was supposed to catch it.
- **A brief that forbids a change must name who to ask.** "Report it rather
  than change it" is not an instruction until the agent knows where reports go.
  Three agents were told to report rather than edit a contested file, none was
  told how to reach its lead, and one lost a request into a dead address —
  where it looked, from every side, as though the question had simply not been
  worth asking. **And a commitment made to a coordinator about a running agent
  is not in force until that agent has been told**; the moment to send it is
  the same turn the commitment is made. That one is easy to get wrong because
  the promise *feels* discharged when the coordinator has been told, and the
  coordinator is the one who asked.
- **A lead that says "no critic reviewed this" is worth more than one that says
  a review happened.** Several critics returned nothing at all this session,
  twice on one branch. The right response is to say so and run an independent
  one, not to substitute your own walk-through — and the leads who did that
  produced the honest map. Read it beside the note above about the branch that
  self-reviewed.
- **The lens that actually finds a thin module is "a field the file format
  round-trips that the interface cannot author".** The tempting one — that a
  module this file says nothing about is an unexamined module — was tried and
  is weak: eleven of twelve unmentioned modules turned out to be finished,
  because an argument about one module belongs in that module's own docs and
  this project puts it there. The narrow lens went four for four in a single
  pass: `Swatch::name`, `Palette::columns`, a stamp's name, and `Layer::name`.
  It needs no prior knowledge of the codebase, which is what makes it worth
  writing down. `sqlite.rs` and `themelib.rs` are the bar the thin ones should
  be measured against — the first because every one of its tests is an
  adversarial case for a reader of a file a stranger wrote, the second because
  it reasons about whether two ids differing only in case are one file or two
  depending on the platform.

## The README

**The README is for somebody deciding whether to paint in Umber, and for
nothing else.** It is the shop window, not the archive. Its shape is fixed: the
mark, one sentence saying what Umber is, a picture of the workspace, how to
install it, then the features — and then the honest list of what is missing, the
controls, how to build it and the licence.

- **Only what an end user would act on goes in.** A reader wants to know what
  Umber *does*, what it opens, what it will not do yet, and which file to
  download. Rationale is the thing to leave out, however good it is: this file
  and `docs/` are where an argument lives, and a README that explains why the
  scratch is `R8Unorm` has stopped being a shop window. The test to apply is
  whether a painter would change what they do on reading it.
- **Cut anything the application already explains, and anything self-evident.**
  This is the rule that keeps the file from growing back. A settings page that
  lists the themes does not need the README to list them; a shortcuts page with
  a search box does not need its search box described; a live pressure readout
  does not need a paragraph saying it shows pressure. Somebody who has opened
  Umber learns all of it in less time than reading about it takes, and somebody
  who has not is deciding whether to download, which a feature inventory does
  not help them do. Name the thing and stop. "Six themes and an interface
  scale" is a fact; "Shortcuts lists every command with a search field and lets
  you rebind" is the manual, and Umber does not ship one because it does not
  need one.
- **Write it the way a person writes, and this applies inside the application
  too.** Short paragraphs. Two or three sentences, then a picture or a table or
  a heading. **No em-dashes** in user-facing text: they are this file's habit
  and they read as fussy in a shop window and cramped in a dialog. A full stop
  and a new sentence almost always says it better. The same goes for every
  string the interface draws — a notice, a tooltip, an empty state — because a
  painter meets those more often than they meet the README.
- **The house voice is for this file and `docs/`, not for the product.** The
  argumentative register that makes an invariant memorable ("and that is not an
  oversight", "which is the failure written out in advance") is exactly wrong
  in front of somebody who just wants to paint. Keep it here. Keep it out of
  there.
- **A feature is shown, not only described.** Prefer a picture of the thing
  working, or a table, over a paragraph. `docs/images/brushes.png` says more
  about the brush library in one glance than the three sentences beside it, and
  the import list is a **table** because "which of my files can I bring?" is a
  lookup rather than a story. Two or three sentences beside a picture is the
  budget; a section that has grown past that is one that has started explaining
  itself.
- **The pictures are drawn by the interface they are pictures of.**
  `docshot.rs`, run by `cargo run -p umber-app --example docs-images`. A
  screenshot taken by hand goes stale in silence, and a README is exactly where
  nobody looks for the drift. Adding a feature section means adding its shot
  there. `docs/images/window.png` is the one exception and says so in that
  module: it is a photograph of a real session, which is the one thing that
  cannot be generated, and it has to be retaken by hand when the workspace
  changes shape.
- **What is not there yet is part of the shop window**, not an appendix to it.
  It is the section that makes the rest believable, it is linked from the
  opening note, and it is held to the same standard as everything else here: a
  feature that half-works is named, with what it does and does not do.
- **The download table is generated by hand and guarded by tests.** See
  "Releasing" — it names files of the current release, and two tests fail the
  build when it names the previous one's.

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
