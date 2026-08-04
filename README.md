<p align="center">
  <picture>
    <source media="(prefers-color-scheme: light)" srcset="docs/images/banner-paper.png">
    <img src="docs/images/banner.png" alt="Umber" width="560">
  </picture>
</p>

<p align="center">
  A painting application built for one thing above all others: <b>latency</b> —
  the shortest possible path between a pen moving and pixels changing.
</p>

<p align="center">
  Paints on the GPU · reads brushes from MyPaint, GIMP, Krita and Photoshop ·
  opens Krita and Photoshop documents · saves to OpenRaster
</p>

![The Umber workspace: the tool rail, the canvas, and the Colour, Brushes,
Layers and History modules](docs/images/window.png)

> **Early days.** Painting, layers, brushes, documents, selections, transforms
> and settings all work on desktop. There is no mobile build yet, and no text or
> shape tools. [What is not there yet](#what-is-not-there-yet) is honest about
> the rest.

## Install

**Umber 0.0.6.** Take the file for your system, or browse the
[release itself](https://github.com/Spillebulle/umber/releases/latest) for the
notes and the checksums.

| Your system | x86-64 | ARM64 |
|---|---|---|
| Windows | [`.msi` installer](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-x64.msi) | [`.msi` installer](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-arm64.msi) |
| macOS | [`.tar.gz`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-universal-apple-darwin.tar.gz) — one universal binary, both slices | *(the same file)* |
| Debian, Ubuntu, Mint | [`.deb`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber_0.0.6_amd64.deb) | [`.deb`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber_0.0.6_arm64.deb) |
| Fedora, RHEL, openSUSE | [`.rpm`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-1.x86_64.rpm) | [`.rpm`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-1.aarch64.rpm) |
| Arch | [`.pkg.tar.zst`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-bin-0.0.6-1-x86_64.pkg.tar.zst) | — |
| Any other Linux | [AppImage](https://github.com/Spillebulle/umber/releases/download/v0.0.6/Umber-0.0.6-x86_64.AppImage) — one file, nothing to install | [AppImage](https://github.com/Spillebulle/umber/releases/download/v0.0.6/Umber-0.0.6-aarch64.AppImage) |
| Flatpak | [`.flatpak` bundle](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-x86_64.flatpak) | — |
| Windows, no installer | [`.zip`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-x86_64-pc-windows-msvc.zip) | [`.zip`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-aarch64-pc-windows-msvc.zip) |
| Linux, no package | [`.tar.gz`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-x86_64-unknown-linux-gnu.tar.gz) | [`.tar.gz`](https://github.com/Spillebulle/umber/releases/download/v0.0.6/umber-0.0.6-aarch64-unknown-linux-gnu.tar.gz) |

You need a GPU with Vulkan, Direct3D 12 or Metal — essentially any machine from
the last decade. The Linux packages pull in the libraries Umber opens at
runtime, so your package manager handles the rest.

Umber checks for new versions when it starts and offers you the release notes
before anything is downloaded. You can turn that off on the first run, or in
**Settings → General**.

## Brushes

<img src="docs/images/brushes.png" alt="The Brushes module: a searchable list of
presets, each drawing its own stroke" align="right" width="300">

**239 presets ship with Umber**, grouped by what they *do* — pencils, inks,
markers, charcoal, paint, watercolour, airbrush, blenders, erasers, texture,
foliage, effects — rather than by which pack they came from.

Every row draws **a real stroke made by the real brush**, with a loop and a
taper in it, so a rake, a chisel or an angle-following brush looks like what it
is instead of like a flat bar.

196 are the whole of
[mypaint-brushes 2.0.2](https://github.com/mypaint/mypaint-brushes); 37 more
come from CC0 and CC-BY Krita packs, 19 of those stamping a **bitmap tip**
rather than an ellipse. Six are Umber's own. Each keeps its author and licence.

Blenders work: a smudging brush picks colour up off the canvas and carries it,
and scrubbing back and forth blends its own wet paint. **A brush can carry its
own blend mode** — a marker that multiplies into the paper, a highlighter that
screens — without putting the layer it paints on into that mode.

<br clear="right">

### Bring your own

**Import brushes…** reads nine formats:

| Format | From |
|---|---|
| `.myb` | MyPaint |
| `.gbr`, `.gpb` | GIMP and Krita stamps |
| `.gih` | GIMP animated brushes — one preset per cell |
| `.vbr` | GIMP parametric brushes, reproduced *exactly* |
| `.kpp` | Krita presets |
| `.bundle` | Krita resource bundles — a whole pack at once, tips and all |
| `.abr` | Photoshop 1, 2, 6.1 and 6.2 |
| `.sut`, `.sutg` | Clip Studio Paint sub-tools — a group arrives whole |
| `.ron` | an Umber library |

A brush leaning on something Umber cannot render is imported **anyway** and you
are told what was dropped — an approximation of a brush you chose beats a
refusal.

### Make your own

**New brush** starts from scratch, and any brush can take a bitmap stamp: one
from your library, a picture you import, or one you **draw yourself** on a
canvas Umber sets up for it. The editor reaches every field a brush has, across
six sections — Tip, Dynamics, Inputs, Scatter, Texture and Blending.

Pressure is not the only thing driving them. Speed, how far into the mark you
are, direction and a throw of the dice per dab can all reach size, opacity,
hardness, scatter, ellipticity, angle and colour.

## Layers

<img src="docs/images/layers.png" alt="The Layers module: a folder holding two
layers, a linked pair below it, and a tick box on every row" align="right"
width="300">

Blend mode, opacity and visibility, plus:

- **Masks** — greyscale coverage that hides and reveals without touching a
  pixel. Paint the layer or paint its mask; the eraser reveals.
- **Clipping** — a layer that only shows where the one below it does.
- **Locks** — no strokes, transforms or clearing until it comes off, and the
  controls go quiet rather than ignoring you.
- **Link groups** — up to six colour-coded sets that move through the stack
  together.
- **Folders** — group, nest, fold shut. A folder's eye and lock reach
  everything inside it, and it travels with its contents.

Every row shows **what is actually on that layer**, scaled to fill the chip
rather than the whole canvas shrunk into it, so a sketch on a large canvas is
still legible. Tick as many rows as you like and show, hide, lock, link, group
or delete them all at once.

Drag a layer sideways as well as up and down to nest it; a dashed outline shows
where it lands and at what depth before you let go.

<br clear="right">

## Colour

Four ways to pick one. The wheel's triangle can follow the hue as you turn it or
hold still, and either centre can be set to whatever angle you like to work at —
Umber remembers each shape's angle separately.

| Wheel, triangle | Wheel, square | Saturation / value | Sliders |
|---|---|---|---|
| ![](docs/images/picker-wheel.png) | ![](docs/images/picker-wheel-square.png) | ![](docs/images/picker-square.png) | ![](docs/images/picker-sliders.png) |

A fifth mode, **Harmony**, marks the complement, triad, tetrad, analogues or
split-complement of the hue you are on and lets you take one with a click.

The **Palette** module keeps the colours you want to come back to: click a slot
to store what is in hand, and save a whole set into a library. Palettes are
plain GIMP `.gpl` files in a folder of their own, so anything that reads them
can read yours, and Umber imports and exports the same format it stores.

## Selections and moving things

**Select** marks out where edits may land — rectangle, freehand lasso, or
polygon point-to-point. Everything the brush and eraser do is clipped to it,
antialiased edges included, so a diagonal is a diagonal rather than a staircase.
Hold `Shift` to add to a selection, `Ctrl` to take away, and both to keep only
where the two overlap. A **feather** softens the edge by as many pixels as you
ask for.

Three buttons sit just above the marquee — **deselect**, **copy** and **cut** —
and they stay on screen even when the selection is scrolled half out of view.

**Transform** picks a region up and lets you move, scale and turn it before
putting it down:

| | |
|---|---|
| Drag inside the box | Move it |
| Drag a corner or edge | Scale it; the opposite handle stays put |
| `Shift` while scaling a corner | Keep the proportions |
| Drag outside the box | Turn it |
| `Enter` | Put it down |
| `Esc` | Throw the move away |

Nothing touches the layer until you put it down, so abandoning costs nothing and
undoing restores both where the pixels went **and** the hole they came from, in
one step. Copy, cut and paste work across tabs, and to and from other
applications — a screenshot pastes straight in, and a region copied out of Umber
can be pasted anywhere else.

## Documents

**Umber saves to OpenRaster** (`.ora`) — the same format it reads — so anything
you make here opens in Krita, GIMP, MyPaint, Drawpile and Pinta. There is no
format of its own, deliberately.

| Opens | |
|---|---|
| `.ora` | OpenRaster — Krita, MyPaint, GIMP, Drawpile, Pinta |
| `.kra` | Krita |
| `.psd` | Photoshop |
| `.png` | a flat image |

| Exports | |
|---|---|
| PNG, JPEG, TIFF, GIF, BMP | flattened, with what each format will cost *this* document named before it writes |

Anything lost on the way in — a flattened group, a dropped mask, a blend mode
with no counterpart — is named in a notice when the document opens rather than
buried in a log. Formats that would import subtly wrong are refused by name
instead.

Several documents are open at once in tabs, each with its own layers, history
and view. **A saved document carries its undo history**, so one reopened
tomorrow can still be stepped back through.

## The workspace

<img src="docs/images/settings-themes.png" alt="The settings dialog, Themes
pane" width="49%"> <img src="docs/images/settings-shortcuts.png" alt="The
settings dialog, Shortcuts pane" width="49%">

Panels are **locked while you paint** and rearranged in a mode of their own:
**Window → Customise layout**. Drag any module by its header into a column, onto
a column's edge to start a new one, or over the canvas to leave it floating. The
tool rail is a module like any other. Your arrangement is saved between runs.

Two themes — **Graphite** and **Paper** — four accents, and an interface scale.
**Shortcuts** lists every command with a search field and lets you rebind;
labels follow your own keyboard, so a Nordic layout shows the key that actually
zooms.

**Input & pen** shows a live reading of what your tablet is sending — the
pressure the device reported beside the pressure Umber resolved, a trace of the
last couple of hundred samples, and a strip to scribble in. It is the page to
open when a pen feels wrong.

## Your work is hard to lose

- **Autosave** every five minutes, waiting for a gap between strokes so it never
  interrupts one and never pauses the canvas.
- Documents you have saved are written back; ones you have not go to a folder of
  Umber's own. Those copies expire after a month, adjustable or off. **Nothing
  Umber deletes is ever a file you chose the place for.**
- Closing with unsaved work asks first, and **names** every document at risk.
- If Umber stops without closing properly, the next start **offers its copies
  back** — naming each document and when it was last written. Opening one puts
  the painting in front of you and writes nothing over the file it came from
  until you save it there yourself. Saying no keeps every copy where it is.
- If Umber does stop, a window opens saying so and — more to the point — whether
  your work was written down and where, with the technical details folded out
  and written to a file you can attach to a bug report. Nothing is sent
  anywhere.

## Controls

| Input | Action |
|---|---|
| Left drag | Use the selected tool |
| `B` / `E` / `S` / `T` / `H` / `Z` | Brush / eraser / select / transform / pan / zoom |
| `Ctrl` + `D` | Deselect |
| `Ctrl` + `C` / `X` / `V` | Copy / cut / paste |
| `Shift` / `Ctrl` / both + drag with Select | Add to the selection / take away / keep the overlap |
| `Enter` / `Esc` while selecting | Close / abandon the outline |
| `Enter` / `Esc` while transforming | Put the picture down / throw the move away |
| `X` | Swap foreground and background colours |
| `Alt` + click | Pick the colour under the cursor |
| `Alt` + move, nothing held | Resize the brush — right and up bigger |
| `[` / `]` | Decrease / increase brush size |
| Middle drag, or `Space` + drag | Pan |
| Wheel / `Shift` + wheel | Scroll up and down / side to side |
| `Ctrl` + wheel | Zoom at cursor |
| `Ctrl` + `+` / `-` / `0` / `1` | Zoom in / out / fit / 100% |
| `Ctrl` + `Z`, `Ctrl` + `Shift` + `Z` | Undo / redo |
| `Ctrl` + `Shift` + `H` / `V` | Flip the canvas left-to-right / top-to-bottom |
| `Ctrl` + `S`, `Ctrl` + `Shift` + `S` | Save / save as… |
| Two-finger drag (touch) | Pan and pinch-zoom |

Everything except the held modifiers is rebindable. On macOS `Ctrl` here is
`Cmd`, and the dialog names it that way.

**Pen pressure** works on touch screens and on Windows, where a pen arrives
through Windows Ink carrying 1024 levels. **Pens on macOS and Linux do not reach
Umber through the window system yet** — there it falls back to a flat setting or
a speed-derived approximation, and a mouse always paints at full pressure.

## What is not there yet

- **Text and shapes.** Six tools where the design draws sixteen.
- **Mobile.** Android and iOS are prepared for architecturally but have never
  been built or run. Do not believe anyone who says otherwise.
- **Structural undo.** Undo covers painting, transforms and canvas flips; adding,
  deleting or reordering a layer is not recorded, and deleting a layer clears the
  history.
- **Transforming a linked set.** Link groups move together through the stack;
  moving several layers at once on the *canvas* is a larger change.
- **A folder's own opacity and blend mode.** Folders hold layers and their eye
  and lock reach inside, but a group opacity needs group compositing.
  [`docs/layer-folders.md`](docs/layer-folders.md) has the design and its cost.
- **Photoshop's layer masks.** Krita's transparency masks come across; a `.psd`
  mask is reported as lost rather than converted, and Krita's filter, transform,
  selection and colorize masks are named rather than approximated.
- **Pen pressure on macOS and Linux**, as above. Windows works.
- A navigator, and stylus tilt.

## Building from source

```sh
cargo run --release
```

You need Rust 1.92 or newer. [`docs/architecture.md`](docs/architecture.md) has
the platform prerequisites, how the stroke pipeline works and why it is built
that way; `CLAUDE.md` has every invariant that has to hold.

## Licence

**GPL-3.0-or-later.** Umber is free software: you may use, study, share and
modify it, and anything you distribute that builds on it must be free in the
same way. See [LICENSE](LICENSE).

The shipped brushes carry their own licences — CC0 and CC-BY — and their
authors' names travel with them.
[`docs/brush-sources.md`](docs/brush-sources.md) records every one.

**Archivo**, the typeface, is bundled under the SIL Open Font License.
