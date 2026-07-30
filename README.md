# Umber

A GPU-accelerated painting application in Rust, built for desktop and tablets.

Umber is being written for one goal above all others: **latency**. Every design
decision below trades convenience for the shortest possible path between a pen
moving and pixels changing.

> **Status: early.** The canvas, brush and eraser work on desktop. Layers,
> file saving and mobile packaging do not exist yet. See [Roadmap](#roadmap).

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
| Windows | D3D12 / Vulkan | Working |
| Linux | Vulkan | Should work; not yet tested by the author |
| macOS | Metal | Should work; not yet tested by the author |
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

## Controls

| Input | Action |
|---|---|
| Left drag | Paint |
| `B` / `E` | Brush / eraser |
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
umber-core      document model, brush, dab generation, camera, undo — no GPU types
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
2. **Composite pass.** Layer and scratch are combined and drawn under the camera
   transform. One fullscreen triangle.
3. **Commit.** At pointer-up the scratch is baked into the layer *once*, over
   only the rectangle the stroke actually touched, and the scratch is cleared.

Stroke opacity is therefore applied exactly once, at commit — which is why
`Brush::opacity` is deliberately excluded from per-dab coverage. The composite
and commit shaders implement the same blending maths; if they ever diverge, the
stroke visibly jumps at pointer-up.

The scratch texture is `R8Unorm` rather than RGBA: a stroke has a single colour,
so only coverage needs storing. That is a 4× bandwidth saving on the hottest
texture in the frame.

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

- Layers, with blend modes and a layer panel
- Saving and loading documents
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
