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
| `umber-core` | document, brush, dab generation, camera, layers, undo | wgpu, winit, egui |
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

### Undo

Stores the RGBA bytes of the rectangle a stroke damaged, not whole layers (a
full 2048² snapshot per stroke would exhaust a gigabyte in ~60 strokes).

The capture happens at **commit** time, not stroke start: the layer is untouched
until commit, so reading it there yields exactly the pre-stroke pixels, and by
then the damaged rect is known. `read_layer_rect` blocks on the GPU, which is
acceptable once per stroke but must never move into the drawing loop.

## Interface

Layout and tokens come from the "Graphite" screens of the Umber design project.

- **Never hard-code a colour.** Everything comes from `theme::Palette`, which is
  what makes the second theme a table of values rather than an edit sweep.
- **`theme::metrics` holds the design's fixed sizes.** Use them instead of
  re-typing 264.0 or 36.0 at the call site.
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

## Testing

`umber-render/tests/gpu_pipeline.rs` runs real GPU work headlessly: it creates a
device with no surface, stamps dabs, commits, and reads pixels back. These tests
catch shader and blend-state bugs that no CPU test can. They **skip** rather
than fail when no adapter exists, so they stay meaningful on CI runners.

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

## Conventions

- British spelling in user-facing strings and docs ("colour", "stabilisation").
- Comments explain *why*, especially where a simpler-looking alternative is
  wrong — most of this file's invariants are also recorded at their call sites.
- This is a GPL-3.0-or-later project. New files carry no licence header, but the
  licence is deliberate: derivative work must stay free.
