# How Umber is built

Notes for anyone reading or changing the source. The [README](../README.md) is
for people who want to paint; this is the part that was in it and should not
have been.

`CLAUDE.md` at the repository root is the fuller version of this — every
invariant that has to hold, most of them learned from a bug. This file is the
tour; that one is the contract.

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
   4-vertex quad, so a thousand dabs cost a single draw call. A brush that asks
   to *build up* — which a sparse texture stamp must — swaps that blend for one
   that composites each dab over the last. It is a change of blend state and
   nothing else: same shader, same scratch, same single commit.
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

The stack is **flat** — there are no folders. Layers can be ticked and operated
on together, and a *link group* carries its members through the stack as a unit,
which covers much of what folders are reached for; nesting and a folder's own
opacity and blend applying to its contents do not exist.
`docs/layer-folders.md` has the design and what it would cost.

### Other decisions worth knowing

- **Colour is linear everywhere** inside the engine. Blending in sRGB space
  darkens midtones. Conversion happens only at the edges.
- **The surface is deliberately *not* sRGB.** egui emits colours that are
  already gamma-encoded, and an sRGB surface would encode them a second time.
  The canvas shader does the encode explicitly instead.
- **Brush sizes are in document pixels**, so painting at 12% zoom lays down
  exactly the pixels you would get at 100%.
- **Undo stores the cells a stroke touched, not whole layers and not the box
  it spans.** A full snapshot per stroke would be 16 MB at 2048², exhausting a
  gigabyte in about sixty strokes; a bounding rectangle is barely better on a
  large canvas, where a thin diagonal across a 10000² document reserved 381 MB
  to record a few million pixels. Damage is accumulated on a 64-pixel grid and
  a patch holds only the cells the dabs reached, clipped to the box so a small
  mark can never cost more than it used to — 6.8 MB for that diagonal, and a
  depth of 75 strokes rather than one. It is not unlimited: a wash that really
  does cover a 10000² canvas is 381 MB of pixels however it is described.
  Undo covers painting only — adding, deleting or reordering a layer is *not*
  undoable yet, and deleting one clears the history, because slots are recycled
  and a stale entry would otherwise be replayed into the wrong layer. Resizing
  the canvas clears it for the same kind of reason: a rectangle of the old
  canvas means different pixels on the new one. Every entry carries what it
  was and when it happened, and the two stacks read as one timeline, which is
  what the History module lists; a jump to a point in it is that many single
  steps, because there are no snapshots to jump to. A save writes the newest
  32 MB of it into the document, keyed by stack position rather than by slot,
  and refuses to restore a history that does not match the stack that loaded.
- **The History module's time column is a gap, not an age.** `History::gap_at`
  measures from the entry before, which is a property of the pair and so does
  not go stale — an age would need the panel repainted every second to stay
  true. Both the kind and the time travel with an entry as undo and redo move
  it between the stacks (`Edit::made_at`), so stepping through the list neither
  renumbers nor re-times it. A row's icon is the icon of the *tool* that made
  the mark, and `panels::edit_icon` is exhaustive over `EditKind` on purpose:
  the list must not be able to grow rows for actions the engine cannot restore.
- **Times are wall-clock, UTC, and optional.** `umber_core::time::Timestamp` is
  Unix milliseconds — an `Instant` means nothing outside the run that produced
  it, and these are written into documents. `Timestamp::since` returns `None`
  rather than clamping when the later stamp precedes the earlier one, because
  an NTP correction or a changed clock is not an interval an artist spent, and
  a plausible number in place of one we do not have is the worse failure. An
  entry from a document written before timestamps existed keeps `None` all the
  way to an empty column. Calendar arithmetic is Hinnant's `civil_from_days`,
  about twenty lines, tested across 1970, 2000 and 2100 and against an
  independent calendar for every day of forty years — no date crate. The one
  thing one would add over this is *local* time, and that is a platform
  question rather than a calendar one: `umber-app/src/localtime.rs` asks the
  operating system for the offset **at that instant**, so a document spanning a
  daylight-saving change does not gain an hour halfway through an afternoon. It
  goes through `libc` and `windows-sys`, both already in the tree, so it adds
  no crate to the build. A platform that will not answer falls back to UTC, and
  both forms name the zone they are in.
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
conversion, undo accounting, and the document round trip — a stack built in
memory, written to an `.ora` and read back byte for byte. The renderer tests are **headless GPU tests**:
they create a device with no surface, stamp real dabs, commit, and read pixels
back to assert on them. They skip rather than fail on a machine with no adapter.

The most important of them is `overlapping_dabs_do_not_compound`, which pins
down the wet-layer invariant described above. It has already caught one real
bug: paint and erase were sharing a blend state, and erase was *adding* opacity
rather than removing it.

## Roadmap

Next, roughly in order:

- **Signed releases.** Umber updates itself in place, and the only thing
  standing behind a download today is HTTPS and a length check. A signature and
  a public key compiled into the application is what would make that a
  guarantee; until then About says exactly what is and is not promised.
- **Layer folders.** The stack is flat. A *pass-through* folder — a container
  that groups and hides, with no opacity or blend of its own — needs no shader
  change and no file-format version bump, and is where this should first ship;
  a folder that fades or blends its contents is group compositing and needs an
  accumulator stack in `composite.wgsl`. `docs/layer-folders.md` is the whole
  design, the invariants it collides with and the order to build it in.
- Structural undo, so layer add/delete/reorder joins the history — and stops
  the History module having to explain that it lists strokes and not layers
- Getting the *explicit* save off the drawing thread. It still reads every
  layer back with a blocking call, so a large document pauses for a moment —
  the one place left where Umber does the thing it exists not to do. The
  machinery to fix it now exists: `CanvasRenderer::begin_capture` reads a whole
  document without stalling a frame, which is how autosave works, and Save
  could use the same path once it has an answer for what the interface shows
  while it waits.
- Crash recovery — noticing an internal autosave newer than the file it belongs
  to, and offering it on the next start. The copies are written; nothing reads
  them back automatically, so today a recovery means opening one out of the
  folder by hand.
- Tile-based sparse canvas storage, for very large and infinite canvases
- Android and iOS build scaffolding
- Native tablet pressure on desktop
- **Ellipticity driven by an input.** Scatter, hardness and the dab's angle all
  respond to pressure or chance now; the dab's *ratio* still comes from a fixed
  value, so 15 brushes that state it only as a mapping import as round ones.
  `docs/brushes.md` records why lifting a constant out of those mappings would
  make them wrong in a new way rather than right.
- **`lock_alpha`** — painting only where the layer already has coverage. Nothing
  in the shipped library needs it; it is worth building as a painting feature in
  its own right.
- **The system clipboard.** Copy and paste move pixels inside Umber and between
  its tabs today; nothing goes to or comes from another application. What stands
  in the way is a dependency that has to hold to the standard `ureq` does — no C
  toolchain, and working on the aarch64 cross-builds and inside the Flatpak
  sandbox — rather than anything about the pixels, which `umber_core::clipboard`
  already holds in the straight-alpha sRGB form an interchange format wants.
- **Selecting by colour**, and growing or shrinking a selection by a distance.
  `umber_core::selection` has the four boolean modes and a feather; what it has
  no answer for is a selection derived from the picture rather than drawn.
- Scatter that reacts to pen speed
- **rubberduck's stamps in the shipped library.** Three packs' stamps ship now;
  this one's do not, because its CC0 is declared on the OpenGameArt page rather
  than inside the download, and shipping a mask is redistributing artwork rather
  than describing it. All 269 import today. `docs/brush-sources.md` has the
  measurement — 17 brushes, 1.2 MB — and the one line that would reverse it.
- **The Gimp Brushcollection**, 1022 CC0 stamps whose licence *is* verifiable
  from the download. What stands between it and the library is curation rather
  than machinery: every brush has an empty name and a spacing of 0, and the
  repository is 158 MB with no tags.
- **A cell chosen per dab**, which is what would make a `.gih` one brush rather
  than five. The dab pass binds one tip per pass, deliberately; this needs it to
  bind a small array and the dab instance to carry an index into it.
- Krita's other paint engines — `spraybrush`, `hairybrush`, `deformbrush` and
  the rest. A preset written by one is refused by name rather than approximated.
- A paper texture of your own. Three ship; `GrainPattern` is a closed enum
  because `Brush` is `Copy`, and reading a fourth off disk needs a variant that
  names a file.
- Tilt support
- Stroke prediction to hide remaining latency

## Releasing

Cutting a release is pushing a tag; everything else follows from it.
`CLAUDE.md` has the whole procedure and the reasoning behind each part of it.

```sh
pwsh tools/release.ps1 0.0.2 -DryRun
pwsh tools/release.ps1 0.0.2
```

## The rest of the documentation

| | |
|---|---|
| [`brushes.md`](brushes.md) | Every brush setting, what it does, and what MyPaint's own files mean |
| [`brush-sources.md`](brush-sources.md) | Where the shipped brushes come from, and every pack considered |
| [`document-format.md`](document-format.md) | Why OpenRaster, and exactly what Umber writes into one |
| [`document-import.md`](document-import.md) | What each importer reads, and why some formats are refused |
| [`layer-folders.md`](layer-folders.md) | The folder design, what it collides with, and why it is not built yet |
