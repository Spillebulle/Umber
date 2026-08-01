# Umber

A GPU-accelerated painting application in Rust, built for desktop and tablets.

Umber is being written for one goal above all others: **latency**. Every design
decision below trades convenience for the shortest possible path between a pen
moving and pixels changing.

> **Status: early.** The canvas, brush, eraser, layers, colour picker, brush
> editor, brush library, canvas settings, settings and PNG export work on
> desktop; layered
> documents can be saved and reopened, and documents written by Krita,
> Photoshop and any OpenRaster application can be opened. Preferences — theme,
> layout, input and shortcut bindings — persist across runs. There is no mobile
> packaging yet. See [Roadmap](#roadmap).

## Installing

Every [release](https://github.com/Spillebulle/umber/releases) carries built
packages for x86-64 and ARM64:

| Platform | |
|---|---|
| Windows | `.msi` installer |
| macOS | `.tar.gz`, a universal binary for Apple Silicon and Intel |
| Debian, Ubuntu, Mint | `.deb` |
| Fedora, RHEL, openSUSE | `.rpm` |
| Any Linux | `.flatpak` bundle, or an AppImage that needs nothing installed |
| Arch | `.pkg.tar.zst` (x86-64) |

The Linux packages name the libraries Umber opens at runtime — the Vulkan
loader, libxkbcommon and the Wayland and X11 clients — so your package manager
will pull them in. The AppImage carries what it can and uses the host's Vulkan
loader, because bundling that one would mean talking to the wrong driver.

There is no RISC-V build. It could only be cross-compiled and never run, and
nothing here has been tested on the architecture.

### Updating

Umber asks GitHub which release is newest when it starts, and tells you if there
is one. The first run says so before the first request goes out, and the check
is switched off in **Settings → General**; **Help → About** runs it on demand and
shows what came back. The request carries nothing about you or your work.

Whether Umber can then install the update depends on how it was installed:

| | |
|---|---|
| Portable zip or tarball | Replaced in place; the new build runs next start |
| AppImage | The one file is replaced |
| Windows `.msi` | The new installer is downloaded and handed to `msiexec` |
| `.deb`, `.rpm`, Arch | Named, with the command to run — never overwritten |
| Flatpak | Not checked at all; Flatpak keeps it current itself |

The last two rows are the important ones. Those files belong to a package
manager that keeps its own record of them, so writing over them is usually not
permitted, makes that record false, and is undone by the next system upgrade.
Umber says which manager owns the copy and what to type, and points at the
releases page. It does the same for anything it cannot identify: a button that
lies about what it is about to do is worse than one that is not there. The
Flatpak goes further and never asks at all — its sandbox is given no network,
deliberately, and Flatpak's own updater already does the job.

**Releases are not signed.** A download is fetched over HTTPS from an address
the release API gave, and checked against the size that API reported. That
catches a truncated or substituted download and is not the same as a signature;
About says so rather than implying otherwise.

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
canvas, and stacked modules (Colour, Brushes, Layers) in a sidebar. A fourth,
**History**, is not in the shipped arrangement and is added from the module
library.

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

The cross in a module's header removes it from the layout — only in edit mode,
so a stray click cannot make a panel vanish while you are painting. It is the
one mark in a header that takes something away, so it lights up in the warning
colour rather than the ordinary hover ink. **Window → Reset layout** puts
everything back where it started.

The arrangement is saved between runs (`%APPDATA%\Umber\layout.conf` on
Windows, `~/.config/umber/layout.conf` on Linux,
`~/Library/Application Support/Umber/` on macOS); an unreadable file is ignored
rather than being an error, and one written by a future version is refused
rather than misread. A file written by an *older* version simply lacks the
modules that did not exist then, and an absent module is a closed one — which
is why the History module is not in the shipped arrangement either. A default
that included it would have made a fresh install and an upgraded one disagree
about what the workspace holds.

### The module library

**Window → Modules** is every module there is: a picture of each, a sentence
saying what it is for, and a button to add it. It is how a removed module comes
back, and how one you have never opened is found — a list of four titles is
fine for flicking a familiar panel off and on, and useless for finding one.

The pictures are **painted**, not screenshots. Umber paints its widgets rather
than shipping images of them, and a bitmap of a panel would be stale the first
time that panel gained a control and wrong in the other theme immediately. Each
is a schematic in the palette's own colours, which is the shape your eye is
matching against the sidebar anyway.

Adding does not place the module: it hands it to the pointer, in layout edit
mode, and you **click where you want it** — a sidebar to dock it, anywhere else
to leave it floating. That is the same drop that moves a module already in the
layout, so there is one way to say where a panel lives rather than two, and
`Esc` abandons the add exactly as it abandons a move.

**Window → Panels** is still there as plain checkboxes, for flicking a familiar
module off and on without being put into a mode.

Neither this dialog nor the History module is in the design, for the same
reason the brush library's browser is not: the design draws three modules that
are always there, and once modules can be removed there has to be somewhere
they come back from.

### History

A viewable edit history, as a module you can dock, float or close like any
other. It lists what has been painted on the document oldest first, with a
marker showing where the document stands; entries behind it read as ink, those
ahead of it as the ghosts they are, and clicking either takes the document
there — an undo or a redo of however many steps it takes, which is what an
eight-step jump costs and no more.

It shows **strokes only**, and says so under the list. Umber's history covers
painting: adding, deleting or reordering a layer is not recorded, and deleting
one clears the list. A row appears only where something can actually be
restored, because a history naming an action that clicking it will not undo is
worse than one that admits its own edges. The first row is the document with
nothing applied — the only way back to a blank canvas — and once the memory
budget has aged the oldest entries out it says so rather than pretending to
reach the beginning.

The list survives being closed and reopened, because a saved document carries
its history — see below.

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
one open at a time. Four are live — **General** (interface scale, and whether
Umber checks for updates when it starts), **Pressure**
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

Umber ships **239 presets**. Six are its own; 196 are the whole of
[mypaint-brushes 2.0.2](https://github.com/mypaint/mypaint-brushes), and the
remaining 37 come out of three CC0 and CC-BY Krita packs — 18 procedural and 19
that stamp a **bitmap tip**, which the shipped library carries now. Every one
carries its author and licence through the conversion and shows them in the
library.

They are grouped by **style** — pencils, inks, markers, charcoal, paint,
watercolour, airbrush, blenders, erasers, texture, foliage, effects — rather
than by which pack or artist they came from. A pack arrives sorted by author,
which is the right way to credit it and the wrong way to browse it: nobody
reaches for a brush by remembering who drew it, and author-grouping put the
pencils in six different places. The author is still shown on every row.

The Brushes panel is the design's: a shortlist with the header's `＋`. Behind
the second mark is the **library browser**, which the design does not have — a
column that works for five brushes does not work for two hundred, so the
browser adds a search field, a collection picker, and per-brush rename and
delete. Brushes you save are marked with a dot and are the only ones those two
controls apply to.

Editing a brush changes it live, so the editor's footer offers to **save** what
you have made, either under a new name or over the brush you started from. Your
library is a `brushes` folder in the platform *data* directory —
`%APPDATA%\Umber\data`, `~/.local/share/umber`, `~/Library/Application
Support/Umber` — kept apart from the shipped library so that an update, which
replaces that one wholesale, cannot take your brushes with it.
If it cannot be read, everything that writes is disabled and the reason is
shown, rather than quietly starting your collection again over the top of it.

A folder rather than the single `brushes.ron` it used to be, because a brush can
now carry a **bitmap tip** and a bitmap does not go in a text file. The presets
are still a `brushes.ron`, now inside that folder, with the stamps beside them
in `tips/` as ordinary greyscale PNGs you can open, replace or copy between
machines. A `brushes.ron` from an earlier version is moved in on first run and
**left where it was** as well — a migration that deletes the only copy of your
collection has to be right first time.

**Importing** reads eight formats and files them by style like everything else:

| | |
|---|---|
| `.myb` | MyPaint |
| `.gbr`, `.gpb` | GIMP and Krita stamps |
| `.gih` | GIMP animated brushes — one preset per cell |
| `.vbr` | GIMP parametric brushes, reproduced *exactly* |
| `.kpp`, `.bundle` | Krita presets and whole resource bundles |
| `.abr` | Photoshop 1, 2, 6.1 and 6.2 |
| `.ron` | an Umber library |

A stamp arrives as a working brush — its picture, its spacing and its
proportions — and goes straight into your hand, since a stamp is unrecognisable
in a list and obvious the moment it makes a mark. A `.bundle` brings a whole
pack at once, tips and all, along with the author and licence its `meta.xml`
states.

Anything that leans on something Umber cannot render is imported anyway, because
an approximation of a brush you chose beats a refusal — but the notice names
what was dropped: a coloured stamp arriving as its silhouette, a brush pipe
losing its sequence, a Krita preset whose paper texture or masking brush has
nowhere to go. Only a file written by one of Krita's *other paint engines* is
refused outright, and by name, because a round dab wearing `deformbrush`'s name
would be invention rather than approximation. The generated library holds itself
to the stricter rule and refuses anything with a loss at all, since nothing
shipped under an author's name should paint unlike their brush.

**Five packs are fetched**, four of them new: MyPaint's, David Revoy's 2025-01
Krita bundle, Raghavendra Kamath's, GDQuest's — CC-BY, so every one of those
carries its credit — and rubberduck's 60 GIMP stamps. That is 269 stamps and 116
Krita presets you can import today, and 37 more brushes in the shipped library.

**A stamp brush can build up.** A sparse photographic texture stamp is not a
solid disc: GIMP and Krita composite every dab, so the mark is the *overlap* of
many faint stamps. Umber takes a `max` of coverage across a stroke — the whole
reason it never goes blotchy — which caps a stroke at the mask's own brightest
texel. For the CC0 GIMP pack that is 0.49, so its brushes painted half as
strongly as their author drew them. `Brush::build_up` switches the dab pass to a
second blend that composites instead, and which of the two a stamp needs is
*measured* rather than guessed: `cargo run -p umber-core --example measure-stamp`
prints both figures per file, and the `.gbr` importer runs the same measurement.
The default is unchanged and stays exactly what it was.

**Twenty shipped brushes carry a bitmap tip.** One is Umber's own — a sparse
chalk stipple, drawn by `examples/build-bitmaps.rs`, deliberately faint enough
that it needs build-up to paint at full strength. The other nineteen are Revoy's,
Raghukamath's and GDQuest's stamps: their masks travel as ordinary greyscale
PNGs in `crates/umber-core/assets/tips/`, embedded beside the library and named
from the preset, so two brushes cut from one stamp share a file and a single GPU
upload. Fifteen masks carry the nineteen brushes, at 624 kB and 664 kB of
release binary.

Every one of them turns the way its author drew it. A stamp is not
rotationally symmetric, so a bitmap tip is live for all three of the dab's angle
states — held at a fixed angle, turning to follow the stroke, or rolled per dab
— and Krita's rake lean, its compound rotation sensors and GIMP's angular brush
pipes are all read for what they mean.

**rubberduck's stamps still do not ship**, though all 269 import. The obstacle
is not the engine and it is not the size: it is that the pack's CC0 is declared
on its download page rather than inside the download, and redistributing 17
masks of somebody else's artwork in every release is a larger claim than
converting them on one machine. `docs/brush-sources.md` records the
measurements, every pack considered, and the one line that would reverse it.

**Paper grain.** An optional tiling texture bitten into dab coverage, which is
what makes a pencil catch on the tooth of the paper. It is anchored to the
document rather than to the brush, so a second stroke lands in the same pits as
the first. Three papers ship — a fine tooth, a canvas weave and a coarse rough —
drawn rather than photographed, for the same licence reason and because a
photograph does not tile.

**Dabs have shape.** A dab is an ellipse with an angle, not a circle, and it can
scatter off the stroke and vary its own size — so a chisel is a chisel, a spray
can sprays, and a charcoal stick catches on the paper. 109 of the 196 shipped
brushes use at least one of those; before, every one of them painted a round
dot whatever its name promised. The angle has three states rather than two: held
fixed (a broad nib, which is what makes calligraphy thick and thin), turned to
follow the stroke (a rake, keeping its bristles across the line of travel), or
rolled per dab (grain, a watercolour fringe, charcoal). 31 brushes want the
third, and without it a long dab repeated down a stroke reads as machined ruling
rather than as a loaded brush.

**Pressure drives more than size and opacity.** Hardness and scatter follow it
too, each through a curve of its own. That is not a refinement: 69 of the 196
shipped brushes soften their edge under a light hand, and 38 change how much
they scatter — 16 of them stating *no* constant scatter at all, so before this
they imported as perfectly smooth lines wearing the name of something granular.

**And pressure is not the only thing driving them.** MyPaint states every brush
setting as a base value plus one mapping per input, and Umber now reads the
whole sum: how fast you are moving, how far into the mark you are, which way it
is heading, and a throw of the dice per dab, onto size, opacity, hardness,
scatter, ellipticity, angle, colour pickup and the colour itself. 132 of the 196
shipped brushes use at least one, and three of them used to arrive
*invisible* — they state their opacity entirely as a mapping, and only the base
value was being read. A stroke thins as it is flicked, a bristle clump changes
shape stamp to stamp, and a brush that mixes when you lean on it mixes.

**The brush editor reaches all of it.** Tip, Dynamics, Inputs, Scatter, Texture
and Blending: every field a brush has, colour pickup and dab shape and jitter and
airbrush rate and paper included. The samples in the brush list are stamped from each preset's own
settings under a pressure ramp rather than drawn from its opacity and hardness,
so in a list two hundred entries long a spray looks like a spray.

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

**A document is a canvas size, a background and a resolution.** *File → New…*
and *File → Canvas settings…* are the same four questions, once for a document
that does not exist yet and once for the one in front: how many pixels, on what
background, at how many pixels per inch, and — only when the size is
changing — which of nine anchors the existing artwork is held by. Presets cover
the sizes worth one click, from a square 2048 to A4 at 300 dpi, and each carries
its own resolution, because 2480 × 3508 is A4 at 300 dpi and a meaningless pair
of numbers at 72. An aspect lock makes one edge drive the other, and a readout
beside them says what the canvas measures in millimetres or inches.

The **background** is a document property rather than a filled bottom layer, so
it can be changed afterwards, erasing cannot punch a hole through it, and
"transparent" stays expressible. It composites *under* the stack inside the same
single pass the layers use, which is one multiply-add per pixel and is exactly
nothing when there is no background — and it means the flat PNG export, the
eyedropper and a blender picking colour off the canvas all see it without a
second code path.

**Resizing** reallocates every texture the document owns and copies the artwork
across, every layer together. It clears the undo history, and says so before you
click: undo stores rectangles of the canvas, and a rectangle means different
pixels on a different one.

**Umber saves to OpenRaster** (`.ora`) — the same format it reads. It has no
container of its own and deliberately did not grow one: everything an Umber
document holds is a canvas size and a stack of layers with a name, an opacity, a
visibility and a blend mode, which is exactly what ORA is, and inventing a
second spelling of it would have meant a second reader to keep in step. A `.ora`
is a ZIP of PNGs and a small XML file, so a document made here opens in Krita,
GIMP, MyPaint, Drawpile and Pinta as well. Four extra attributes carry what
baseline ORA cannot — the selected layer, a document-format version, Umber's own
name for a blend mode where the SVG one is only approximate, and which layer is
really the background — and every other application ignores them, as XML readers
do. Resolution needs no attribute of Umber's: ORA already has `xres` and `yres`,
and inventing one beside a standard would mean other applications ignoring a
number they already understand.

**The background is written to the file twice, on purpose.** The obvious
extension — a colour named in an attribute — would have every other application
open the document on transparency, and a white painting on a checkerboard in
Krita is not a dramatic failure, which is exactly what makes it a bad one:
nobody notices until they export. So it also goes in as a real opaque bottom
layer carrying the pixels, tagged so that Umber's own reader turns it back into
the property rather than a layer you never made. An older Umber opens the file
and shows the same picture, which is why the format version did not have to be
bumped for it. `docs/document-format.md` has the whole argument.

**Ctrl+S** saves, **Ctrl+Shift+S** saves under a new name, and the tab strip's
dot clears when it lands. Layers are written cropped to what they actually
contain, so a sketch is a few hundred kilobytes rather than a few megabytes. The
file is built whole and renamed into place, so a save interrupted by a full disk
cannot leave a broken file where the last good one was. Closing a document with
unsaved work offers to save it, and closes the tab only if a file was really
written. What does not go in the file: the camera, and — since Umber has none —
groups, masks and adjustment layers. `docs/document-format.md` records the whole
of it, including why the round trip is byte-exact and where the format will have
to grow.

**The undo history goes in the file too**, so a document reopened tomorrow can
still be stepped back through and the History module comes back where you left
it, redo stack and all. It rides as private entries every other OpenRaster
reader walks straight past, so the file is still an ordinary `.ora`. Two things
made that harder than it sounds. A patch belongs to a *layer*, not to the
texture slot it happened to be recorded against — slots are recycled — so the
file names layers by their place in the stack and refuses to restore anything it
cannot match exactly; a history replayed into the wrong layer would be far worse
than none. And size is the real problem: 32 MB of the newest edits is the limit,
which on a sketching session is under half a megabyte and free, and on an
afternoon of full-canvas painting is the difference between a 9.7 MB file and a
41.5 MB one. That is why **Settings → General** can switch it off, with the
trade stated in megabytes rather than in adverbs.

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

**Export flat PNG** is still there and still means what it says: one image, for
showing people. Save is what keeps the layers.

### Not built yet

Taken from the design but not implemented, roughly by size:

- **Drag-to-reorder tools** in the rail, and **saved workspaces**: the two
  parts of the design's layout edit mode still outstanding. The rest of it is
  built — see [Layout edit mode](#layout-edit-mode).
- The brush editor's **Wet edges** section. Tip, Dynamics, Inputs, Scatter,
  Texture and Blending are built; that one has no engine behind it, so it is not
  drawn rather than drawn empty.
- The Navigator overlay, Palette and Harmony colour modes, and per-brush blend
  modes. Also MyPaint's `lock_alpha`, `colorize` and `custom_input`, and stylus
  tilt — `docs/brushes.md` costs each one and says why it is not built.
- **Autosave and recovery.** Saving works and does not lose work when it fails;
  it does not yet protect you from not having done it.
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
| `Ctrl` + `S`, `Ctrl` + `Shift` + `S` | Save / save as… |
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
  and a stale entry would otherwise be replayed into the wrong layer. Resizing
  the canvas clears it for the same kind of reason: a rectangle of the old
  canvas means different pixels on the new one. Every entry carries what it
  was, and the two stacks read as one timeline, which is what the History
  module lists; a jump to a point in it is that many single steps, because
  there are no snapshots to jump to. A save writes the newest 32 MB of it into
  the document, keyed by stack position rather than by slot, and refuses to
  restore a history that does not match the stack that loaded.
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
- Structural undo, so layer add/delete/reorder joins the history — and stops
  the History module having to explain that it lists strokes and not layers
- Getting the save off the drawing thread. It reads every layer back from the
  GPU with a blocking call, so a large document pauses for a moment — the one
  place left where Umber does the thing it exists not to do.
- Autosave and crash recovery, now that there is a format to write them in
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
- Scatter that reacts to pen speed
- Per-brush blend modes
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

## Licence

GPL-3.0-or-later. Umber is free software: you may use, study, share and modify
it, and anything you distribute that builds on it must be free in the same way.
See [LICENSE](LICENSE).

Note that GPL-3.0 is incompatible with the Apple App Store's terms. As the sole
copyright holder the author can distribute Umber there under separate terms;
that remains possible only while contributions are covered by an agreement
allowing it.
