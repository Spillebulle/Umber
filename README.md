# Umber

A GPU-accelerated painting application in Rust, built for desktop and tablets.

Umber is being written for one goal above all others: **latency**. Every design
decision below trades convenience for the shortest possible path between a pen
moving and pixels changing.

> **Status: early.** The canvas, brush, eraser, layers, colour picker, brush
> editor, brush library, settings and PNG export work on desktop, and documents
> written by Krita, Photoshop and any OpenRaster application can be opened.
> Preferences — theme, layout, input and shortcut bindings — persist across
> runs. Umber has no document format of its own and no mobile packaging yet.
> See [Roadmap](#roadmap).

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
bar, a document tab strip, a tool options strip, a two-column tool rail, the
canvas, and stacked modules (Colour, Brushes, Layers) in a sidebar.

### Layout edit mode

Panels are locked while you paint, and rearranged in a mode of their own —
**Window → Customise layout**, as the design has it. The canvas is paused for
as long as the mode is on, which is both what the design says and the reason a
panel dragged across the canvas cannot leave a stroke behind it.

In that mode every module is draggable by its header. Drop it in either sidebar
to dock it — a dashed *dock here* block shows where it will land, and a module
dropped between two others is inserted there. Drop it over the canvas and it
becomes a floating window that **hovers**: it takes no space from the document,
so moving one never shifts where a stroke lands. The tool rail is draggable by
its own grip and snaps to whichever side of the window you release it on.
`Esc` during a drag puts the module back where it came from.

Sizes are draggable at any time: the boundary between two stacked modules, a
sidebar's inner edge, and a floating module's bottom-right corner. Everything
has a minimum, so a module cannot be squashed out of existence.

Modules are hidden by the close mark in their header and brought back from
**Window → Panels**, which also has **Reset layout**. The arrangement is saved
between runs (`%APPDATA%\Umber\layout.conf` on Windows,
`~/.config/umber/layout.conf` on Linux, `~/Library/Application Support/Umber/`
on macOS); an unreadable file is ignored rather than being an error, and one
written by a future version is refused rather than misread.

There used to be a global left-handed flag that mirrored the whole workspace.
It is gone. With every part of the workspace going where you put it — the tool
rail included — a handedness switch is a worse version of the same feature.

The Colour panel implements three of the design's five picker modes — a hue
ring with a switchable triangle or square centre, a saturation/value square
with a hue bar, and RGB sliders. Palette and Harmony are not built.

Two themes ship — **Graphite** (near-black, the default) and **Paper** (warm
neutrals) — under *View*. Colours, type scale and metrics live in
`crates/umber-app/src/theme.rs`; nothing else hard-codes a colour, so a third
theme is a table of values.

### Settings

The settings dialog follows the design's shape: a left rail of six panes with
one open at a time. Four are live — **General** (interface scale), **Pressure**
(where pressure comes from and how the speed-derived model responds),
**Themes** and **Shortcuts**. **Input & pen** and **Performance** are in the
design but have nothing behind them, so they are shown greyed with a tooltip
saying why rather than opening onto controls that do nothing. The same applies
inside the panes: the design's "New theme" card, its theme editor's Save and
Export, and the shortcut Import and Export are drawn dead, because themes are a
compiled table of values and there is no theme or keymap file format yet. The
theme editor's colour rows *are* real — they read the palette in use, and the
design's four-way **accent** choice is live and persists.

**Shortcuts** lists every command, with a search field, and lets you rebind:
click a key to listen for a new one, press Escape to cancel, add a second key
for the same command, clear one, or put a command — or the whole table — back to
its defaults. While a field is listening, key dispatch to the canvas is
suspended, so pressing `B` to bind it does not also select the brush.

Every shortcut is a chord matched *exactly*, so plain `Z` (zoom tool) and
`Ctrl` + `Z` (undo) are different bindings rather than one shadowing the other.
Giving a chord to a second command does not quietly take it off the first:
following the design, the clash is allowed and then flagged on both rows —
*flagged, never silently dropped* — leaving you to settle it. `Space` and
`Escape` cannot be bound; `Space` pans while you draw and `Escape` is what
cancels a rebind, and a capture field you cannot escape from is a trap.

Settings persist across runs in a plain `key = value` file in the platform
configuration directory — `%APPDATA%\Umber\config`, `~/.config/umber`,
`~/Library/Application Support/Umber`; the dialog shows the exact path. Only
settings that differ from the defaults are written, so a later change to a
default still reaches you. A missing, older or corrupt file can never stop the
app starting: every line is parsed independently and anything unreadable falls
back to its default. Writes happen on a background thread once you stop
dragging, so no frame ever waits on a disk.

The design's sliders, pill toggles, segmented pickers, tool icons and brush
previews are painted directly (`widgets.rs`, `colorpicker.rs`) rather than
restyled out of egui's stock widgets, which have a look of their own that
fights the design.

### Brushes

Umber ships **201 presets**. Five are its own; the other 196 are the whole of
[mypaint-brushes 2.0.2](https://github.com/mypaint/mypaint-brushes), which is
CC0, each carrying its author and licence through the conversion and showing
them in the library.

They are grouped by **style** — pencils, inks, markers, charcoal, paint,
watercolour, airbrush, blenders, erasers, texture, foliage, effects — rather
than by which pack or artist they came from. A pack arrives sorted by author,
which is the right way to credit it and the wrong way to browse it: nobody
reaches for a brush by remembering who drew it, and author-grouping put the
pencils in six different places. The author is still shown on every row.

The Brushes panel is the design's: a shortlist with the header's `＋`. Behind
the second mark is the **library browser**, which the design does not have — a
column that works for five brushes does not work for 201, so the browser adds a
search field, a collection picker, and per-brush rename and delete. Brushes you
save are marked with a dot and are the only ones those two controls apply to.

Editing a brush changes it live, so the editor's footer offers to **save** what
you have made, either under a new name or over the brush you started from. Your
library is a `brushes.ron` in the platform *data* directory — `%APPDATA%\Umber\
data`, `~/.local/share/umber`, `~/Library/Application Support/Umber` — kept
apart from the shipped library so that an update, which replaces that one
wholesale, cannot take your brushes with it.
If it cannot be read, everything that writes is disabled and the reason is
shown, rather than quietly starting your collection again over the top of it.

**Importing** reads MyPaint `.myb` brushes and Umber's own `.ron` libraries, and
files them by style like everything else. A `.myb` that leans on something Umber
still cannot render — `colorize`, `lock_alpha` — is imported anyway, because an
approximation of a brush you chose beats a refusal, but the notice names what
was dropped. The generated library holds itself to the stricter rule and refuses
those outright, since nothing shipped under an author's name should paint unlike
their brush.

**Dabs have shape.** A dab is an ellipse with an angle, not a circle, and it can
scatter off the stroke and vary its own size — so a chisel is a chisel, a spray
can sprays, and a charcoal stick catches on the paper. 109 of the 196 shipped
brushes use at least one of those; before, every one of them painted a round
dot whatever its name promised. Where the angle is driven by stroke direction
the dab turns to follow the line, which is what separates a rake from a broad
nib: a nib holds its angle through a curve, and that is what makes calligraphy
thick and thin.

**Blenders work.** A brush with MyPaint's `smudge` picks colour up off the
canvas and carries it, and because the sample is taken through the same
composite pass the screen uses, scrubbing back and forth blends a brush's own
wet paint rather than only what was already on the layer. The read is
asynchronous — a blocking one every frame is exactly what this project exists to
avoid — so the colour lags a frame or two, which is invisible against
`smudge_length`, MyPaint's own and much longer delay. Airbrushes work too: a
brush can deposit paint on a clock rather than only by distance travelled, so
holding the pen still keeps spraying.

### Documents

**File → Open**, or a file dropped on the window, reads documents written by
other applications: **OpenRaster** (`.ora`), **Krita** (`.kra`), **Photoshop**
(`.psd`) and flat **PNG**. Layers, names, opacity, visibility and blend modes
come across where they have an Umber equivalent. Anything lost on the way in —
a flattened group, a dropped mask, a blend mode with no counterpart — is named
in a notice when the document opens, not left in the log.

Clip Studio (`.clip`), MediBang (`.mdp`), Procreate, `.xcf` and layered TIFF
are refused by name — `docs/document-import.md` records why each one was left
out. The governing rule is that an import producing subtly wrong pixels is
worse than one that refuses: a refusal sends you to export an ORA, while a
wrong import wastes an afternoon before you notice the colours moved.

Several documents are open at once, in the design's tab strip. Each has its own
layers, history and camera, and its own GPU storage — switching tabs moves that
state wholesale rather than reloading anything. Closing a document with unsaved
work asks first, and shows you the document before it asks.

There is still no way to *write* a layered file: PNG export is flat, and Save
is drawn disabled with a tooltip saying so. Reading someone else's format never
needed a format of our own; writing one does.

### Not built yet

Taken from the design but not implemented, roughly by size:

- **Drag-to-reorder tools** in the rail, and **saved workspaces**: the two
  parts of the design's layout edit mode still outstanding. The rest of it is
  built — see [Layout edit mode](#layout-edit-mode).
- The brush editor's **Texture** tab. Tip and Dynamics are built.
- A document format of Umber's own. Other applications' files open (see
  [Documents](#documents)) and PNG export works, but nothing can be saved and
  reopened with its layers intact.
- The Navigator overlay, Palette and Harmony colour modes, and per-brush blend
  modes.
- The design shows a sixteen-tool rail; Umber has four. The missing twelve are
  not drawn rather than shown as buttons that do nothing.

**Archivo**, the typeface the design specifies, is bundled under the SIL Open
Font License — see `assets/fonts/`. It is a variable font and `ab_glyph` does
not apply variation axes, so what renders is the Regular master; egui's
`strong()` changes colour rather than weight, so no bold face is ever asked for.

Icons are **drawn as vectors** (`icons.rs`) rather than taken from a font.
Unicode symbols would have been simpler, but Archivo carries none of them, so
they would silently become blank boxes — and platform fallback would render
them at a different weight and size on each OS.

## Controls

| Input | Action |
|---|---|
| Left drag | Use the selected tool |
| `B` / `E` / `H` / `Z` | Brush / eraser / pan / zoom |
| `X` | Swap foreground and background colours |
| `Alt` + click | Pick the colour under the cursor |
| `[` / `]` | Decrease / increase brush size |
| Middle drag, or `Space` + drag | Pan |
| Wheel | Zoom at cursor |
| `Ctrl` + `0` / `1` | Fit to window / 100% |
| `Ctrl` + `Z`, `Ctrl` + `Shift` + `Z` | Undo / redo |
| Two-finger drag (touch) | Pan and pinch-zoom |
| Drag a panel header (layout edit mode) | Move that module to a sidebar, or over the canvas |
| `Esc` while dragging | Put the module back where it came from |

Every row above except the held modifiers — `Space`, `Alt` and the mouse
gestures — is rebindable in the Shortcuts tab of the settings dialog. On macOS
`Ctrl` in this table is `Cmd`, and the dialog names it that way.

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

- A document format — saving and reopening a layered file. Reading other
  applications' files already works; this is the writing half.
- Structural undo, so layer add/delete/reorder joins the history
- Tile-based sparse canvas storage, for very large and infinite canvases
- Android and iOS build scaffolding
- Native tablet pressure on desktop
- **Shape driven by pressure or randomness.** The dab is an ellipse with scatter
  and jitter now, but only from fixed values: 23 brushes vary their ellipticity
  through an input mapping and still import as round ones, and scatter that
  reacts to pen speed is ignored.
- Tilt support
- Stroke prediction to hide remaining latency

## Licence

GPL-3.0-or-later. Umber is free software: you may use, study, share and modify
it, and anything you distribute that builds on it must be free in the same way.
See [LICENSE](LICENSE).

Note that GPL-3.0 is incompatible with the Apple App Store's terms. As the sole
copyright holder the author can distribute Umber there under separate terms;
that remains possible only while contributions are covered by an agreement
allowing it.
