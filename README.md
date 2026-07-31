# Umber

A GPU-accelerated painting application in Rust, built for desktop and tablets.

Umber is being written for one goal above all others: **latency**. Every design
decision below trades convenience for the shortest possible path between a pen
moving and pixels changing.

> **Status: early.** The canvas, brush, eraser and layers work on desktop.
> File saving and mobile packaging do not exist yet. See [Roadmap](#roadmap).

## Building

Requires a recent stable Rust toolchain (1.92+) and a GPU with Vulkan, D3D12 or
Metal support.

```sh
cargo run --release
```

Debug builds compile dependencies with optimisations (see `[profile.dev]` in
the workspace manifest) — an unoptimised wgpu makes the canvas too slow to
evaluate.

### Platform notes

| Platform | Backend | State |
|---|---|---|
| Windows | D3D12 / Vulkan | Working; run interactively |
| Linux | Vulkan | Builds and tests pass in CI; not yet run interactively |
| macOS | Metal | Builds and tests pass in CI; not yet run interactively |
| Android | Vulkan | Architecture is ready, build scaffolding is not written |
| iOS / iPadOS | Metal | Architecture is ready, build scaffolding is not written |

**Linux** needs the usual windowing development headers. On Debian/Ubuntu:

```sh
sudo apt install libwayland-dev libxkbcommon-dev libxkbcommon-x11-dev \
                 libx11-dev libxrandr-dev libxi-dev libvulkan-dev
```

On Arch:

```sh
sudo pacman -S wayland libxkbcommon libx11 libxrandr libxi vulkan-icd-loader
```

## Interface

The workspace follows the **Umber app** screen of the design project: a menu
bar, a tool options strip, a two-column tool rail, the canvas, and a docked
column of stacked panels (Colour, Brushes, Layers). The whole layout mirrors
for left-handed use, so the rail sits under the drawing hand.

The Colour panel implements three of the design's five picker modes — a hue
ring with a switchable triangle or square centre, a saturation/value square
with a hue bar, and RGB sliders. Palette and Harmony are not built.

Two themes ship — **Graphite** (near-black, the default) and **Paper** (warm
neutrals) — under *View*. Colours, type scale and metrics live in
`crates/umber-app/src/theme.rs`; nothing else hard-codes a colour, so a third
theme is a table of values.

The design's sliders, pill toggles, segmented pickers, tool icons and brush
previews are painted directly (`widgets.rs`, `colorpicker.rs`) rather than
restyled out of egui's stock widgets, which have a look of their own that
fights the design.

### Not built yet

Taken from the design but not implemented, roughly by size:

- **Layout edit mode** — dragging panels out to float, dock zones, tear-off and
  re-docking, drag-to-reorder tools. This is the design's "advanced endgame"
  and is a large project in an immediate-mode UI.
- **Settings dialog** — theme cards, shortcut editor.
- The brush editor's **Texture** tab. Tip and Dynamics are built.
- Document tabs (single-document only), the Navigator overlay, Palette and
  Harmony colour modes, and per-brush blend modes.
- The design shows a sixteen-tool rail; Umber has four. The missing twelve are
  not drawn rather than shown as buttons that do nothing.

The design specifies **Archivo** for UI text. That font is not bundled, so
egui's default face is used; typography is the one part of the design that is
approximated rather than matched.

## Controls

| Input | Action |
|---|---|
| Left drag | Use the selected tool |
| `B` / `E` / `H` / `Z` | Brush / eraser / pan / zoom |
| `X` | Swap foreground and background colours |
| `[` / `]` | Decrease / increase brush size |
| Middle drag, or `Space` + drag | Pan |
| Wheel | Zoom at cursor |
| `Ctrl` + `0` / `1` | Fit to window / 100% |
| `Ctrl` + `Z`, `Ctrl` + `Shift` + `Z` | Undo / redo |
| Two-finger drag (touch) | Pan and pinch-zoom |

## Architecture

Four crates, layered so the engine can be tested without a GPU and the GPU can
be tested without a window:

```
umber-core      document model, brush, dab generation, camera, layers, undo — no GPU types
umber-render    wgpu: textures, pipelines, shaders
umber-app       winit event loop, input translation, egui tool panel
umber-desktop   thin binary for Windows/macOS/Linux
```

### How a stroke is drawn

The interesting part, and the reason Umber is structured the way it is.

A stroke is a dense row of overlapping stamps ("dabs"). The naive approach —
compositing each dab straight onto the layer — is wrong in a way that is
immediately visible: every overlap darkens, so a semi-transparent stroke comes
out blotchy, far more opaque than requested, and darker still wherever the
stroke crosses itself.

Umber instead uses a **wet layer**:

1. **Dab pass.** New dabs since the last frame are stamped into a scratch
   coverage texture using a `max` blend, so coverage saturates at 1.0 no matter
   how many dabs land on a pixel. All dabs in a frame are instances of one
   4-vertex quad, so a thousand dabs cost a single draw call.
2. **Composite pass.** The layer stack and the scratch are combined and drawn
   under the camera transform. One fullscreen triangle.
3. **Commit.** At pointer-up the scratch is baked into the active layer *once*,
   over only the rectangle the stroke actually touched, and the scratch is
   cleared.

Stroke opacity is therefore applied exactly once, at commit — which is why
`Brush::opacity` is deliberately excluded from per-dab coverage. The composite
and commit shaders implement the same blending maths; if they ever diverge, the
stroke visibly jumps at pointer-up.

The scratch texture is `R8Unorm` rather than RGBA: a stroke has a single colour,
so only coverage needs storing. That is a 4× bandwidth saving on the hottest
texture in the frame.

### Layers

Layers live in slices of a single GPU **texture array**, and the whole stack
composites in **one pass** — the fragment shader walks the array bottom to top.
An extra layer therefore costs a loop iteration, not a render pass and a
fullscreen bandwidth round trip. Blend modes (Normal, Multiply, Screen,
Overlay, Add) use the W3C compositing formulas on premultiplied colour.

Each layer owns a **slot** — its array slice — assigned at creation and never
changed. Stack order is just the order of a `Vec`, so reordering layers is a
pointer shuffle rather than 16 MB of texture copies per move. Growing past the
allocated slice count reallocates and copies, so it doubles rather than growing
by one.

The in-progress stroke is blended *inside* the stack at the active layer's
position, not on top of the finished composite. Painting underneath a Multiply
layer would otherwise preview wrongly and then jump on release.

### Other decisions worth knowing

- **Colour is linear everywhere** inside the engine. Blending in sRGB space
  darkens midtones. Conversion happens only at the edges.
- **The surface is deliberately *not* sRGB.** egui emits colours that are
  already gamma-encoded, and an sRGB surface would encode them a second time.
  The canvas shader does the encode explicitly instead.
- **Brush sizes are in document pixels**, so painting at 12% zoom lays down
  exactly the pixels you would get at 100%.
- **Undo stores damaged rectangles, not whole layers.** A full snapshot per
  stroke would be 16 MB at 2048², exhausting a gigabyte in about sixty strokes.
  Undo covers painting only — adding, deleting or reordering a layer is *not*
  undoable yet, and deleting one clears the history, because slots are recycled
  and a stale entry would otherwise be replayed into the wrong layer.
- **GPU limits are `downlevel_defaults`**, so a desktop build cannot silently
  start depending on capabilities an Android or iOS device will refuse.

### Pressure support

Pressure is a first-class input, but where it comes from varies:

- **Touch screens** (Android, iPad) report real pressure, which winit surfaces
  as `Force`.
- **Desktop pen tablets do not currently report pressure through winit's mouse
  events.** Until a native tablet path exists (Windows Ink / `WM_POINTER`,
  Wacom drivers), desktop strokes fall back to a flat 1.0 or a speed-derived
  approximation, selectable in the Pressure section of the tool panel.

The `PressureSource` enum exists precisely so native tablet APIs can be slotted
in later without touching the brush engine.

## Testing

```sh
cargo test
```

The engine tests are pure CPU and cover dab spacing, camera transforms, colour
conversion and undo accounting. The renderer tests are **headless GPU tests**:
they create a device with no surface, stamp real dabs, commit, and read pixels
back to assert on them. They skip rather than fail on a machine with no adapter.

The most important of them is `overlapping_dabs_do_not_compound`, which pins
down the wet-layer invariant described above. It has already caught one real
bug: paint and erase were sharing a blend state, and erase was *adding* opacity
rather than removing it.

## Roadmap

Next, roughly in order:

- Saving and loading documents
- Structural undo, so layer add/delete/reorder joins the history
- Tile-based sparse canvas storage, for very large and infinite canvases
- Android and iOS build scaffolding
- Native tablet pressure on desktop
- Textured and shaped brushes, tilt support
- Stroke prediction to hide remaining latency

## Licence

GPL-3.0-or-later. Umber is free software: you may use, study, share and modify
it, and anything you distribute that builds on it must be free in the same way.
See [LICENSE](LICENSE).

Note that GPL-3.0 is incompatible with the Apple App Store's terms. As the sole
copyright holder the author can distribute Umber there under separate terms;
that remains possible only while contributions are covered by an agreement
allowing it.
