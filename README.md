<p align="center">
  <picture>
    <source media="(prefers-color-scheme: light)" srcset="docs/images/banner-paper.png">
    <img src="docs/images/banner.png" alt="Umber" width="560">
  </picture>
</p>

A painting application built for one thing above all others: **latency** — the
shortest possible path between a pen moving and pixels changing.

Umber paints on the GPU, reads brushes written for MyPaint, GIMP, Krita and
Photoshop, opens documents from Krita and Photoshop, and saves to OpenRaster, so
nothing you make here is trapped here.

> **Early days.** Painting, layers, brushes, documents, selections, transforms
> and settings all work on desktop. There is no mobile build yet, and no text or
> shape tools. [What is not there yet](#what-is-not-there-yet) is honest about
> the rest.

![The Umber workspace: the tool rail, the canvas, and the Colour, Brushes,
Layers and History modules](docs/images/window.png)

## Installing

Download from the [latest release](https://github.com/Spillebulle/umber/releases/latest).
Every release carries builds for **x86-64 and ARM64**.

| Your system | Take |
|---|---|
| Windows | the `.msi` installer |
| macOS | the `.tar.gz` — one universal binary, Apple Silicon and Intel |
| Debian, Ubuntu, Mint | the `.deb` |
| Fedora, RHEL, openSUSE | the `.rpm` |
| Arch | the `.pkg.tar.zst` |
| Any other Linux | the **AppImage** — one file, nothing to install |
| Flatpak | the `.flatpak` bundle |

You need a GPU with Vulkan, Direct3D 12 or Metal. That is essentially any
machine from the last decade.

The Linux packages name the libraries Umber opens at runtime — the Vulkan
loader, libxkbcommon and the Wayland and X11 clients — so your package manager
pulls them in for you.

### Keeping it up to date

Umber asks GitHub which release is newest when it starts and tells you if there
is one. The first run says so *before* the first request goes out, and you can
switch the check off in **Settings → General**. The request carries nothing
about you or your work.

Whether it can then install the update depends on how you installed it:

| | |
|---|---|
| Portable zip or tarball | Replaced in place; the new build runs next start |
| AppImage | The one file is replaced |
| Windows `.msi` | The new installer is downloaded and handed to `msiexec` |
| `.deb`, `.rpm`, Arch | Named, with the command to run — never overwritten |
| Flatpak | Not checked at all; Flatpak keeps it current itself |

Those last two rows matter. Those files belong to a package manager that keeps
its own record of them, so writing over them is usually not permitted, makes
that record false, and is undone by your next system upgrade. Umber tells you
which manager owns the copy and what to type instead. A button that lies about
what it is about to do is worse than one that is not there.

**Releases are not signed.** A download is fetched over HTTPS from an address
GitHub's own API gave, and checked against the size that API reported. That
catches a truncated or substituted download; it is not the same as a signature,
and **Help → About** says so rather than implying otherwise.

## Brushes

Umber ships **239 presets**, grouped by what they *do* — pencils, inks, markers,
charcoal, paint, watercolour, airbrush, blenders, erasers, texture, foliage,
effects — rather than by which pack they came from. Nobody reaches for a brush
by remembering who drew it. Every one still shows its author and licence.

Six are Umber's own. 196 are the whole of
[mypaint-brushes 2.0.2](https://github.com/mypaint/mypaint-brushes). The other
37 come from CC0 and CC-BY Krita packs, and 19 of those stamp a **bitmap tip**
rather than an ellipse — a real brush mark, turning the way its author drew it.

### Bringing your own

**Import brushes…**, in the brush editor and in the library browser. Eight
formats:

| | |
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

A stamp arrives as a working brush — its picture, its spacing, its proportions —
and goes straight into your hand, because a stamp is unrecognisable in a list
and obvious the moment it makes a mark.

Anything leaning on something Umber cannot render is imported **anyway**, since
an approximation of a brush you chose beats a refusal — but you are told what
was dropped: a coloured stamp arriving as its silhouette, a brush pipe losing
its sequence, a Krita preset whose paper texture has nowhere to go. Only files
written by Krita's *other paint engines* are refused, by name, because a round
dab wearing `deformbrush`'s name would be invention rather than approximation.

Brushes you save live in a `brushes` folder in your data directory —
`%APPDATA%\Umber\data`, `~/.local/share/umber`,
`~/Library/Application Support/Umber` — kept well away from the shipped library,
which an update replaces wholesale. The presets are a text file; the stamps sit
beside them in `tips/` as ordinary greyscale PNGs you can open, replace or copy
between machines.

### Editing one

The brush editor reaches **every** field a brush has, across six sections: Tip,
Dynamics, Inputs, Scatter, Texture and Blending. Dab shape, jitter, airbrush
rate, colour pickup, paper grain, and pressure curves for size, opacity,
hardness and scatter.

Pressure is not the only thing driving them. How fast you are moving, how far
into the mark you are, which way it is heading and a throw of the dice per dab
can all reach size, opacity, hardness, scatter, ellipticity, angle and colour.
A stroke thins as it is flicked; a bristle clump changes shape stamp to stamp.

Blenders work: a brush with smudge picks colour up off the canvas and carries
it, and because the sample comes through the same pass that draws the screen,
scrubbing back and forth blends the brush's own wet paint.

## Documents

**File → New…** and **File → Canvas settings…** ask the same four questions:
how many pixels, on what background, at how many pixels per inch, and — when
you are resizing — which of nine anchors holds the existing artwork. Presets
run from a square 2048 to A4 at 300 dpi, each carrying its own resolution,
because 2480 × 3508 is A4 at 300 dpi and a meaningless pair of numbers at 72.

The background is transparent, white, black or a colour of your choosing, and
it is a property of the document rather than a filled bottom layer — so you can
change it afterwards, erasing cannot punch a hole through it, and "transparent"
stays expressible.

### Saving, and opening other people's work

**Umber saves to OpenRaster** (`.ora`) — the same format it reads. A `.ora` is a
ZIP of PNGs and a small XML file, so a document made here opens in **Krita,
GIMP, MyPaint, Drawpile and Pinta** too. Umber has no format of its own and
deliberately did not grow one.

**File → Open**, or a file dropped on the window, reads:

| | |
|---|---|
| `.ora` | OpenRaster — Krita, MyPaint, GIMP, Drawpile, Pinta |
| `.kra` | Krita |
| `.psd` | Photoshop |
| `.png` | a flat image |

Layers, names, opacity, visibility and blend modes come across wherever there is
an Umber equivalent. Anything lost on the way in — a flattened group, a dropped
mask, a blend mode with no counterpart — is named in a notice when the document
opens, not buried in a log.

Clip Studio, MediBang, Procreate, `.xcf` and layered TIFF are refused **by
name**. The rule is that an import producing subtly wrong pixels is worse than
one that refuses: a refusal sends you to export an `.ora`, while a wrong import
wastes an afternoon before you notice the colours moved.

Several documents are open at once, in tabs, each with its own layers, history
and view. **Export flat PNG** means what it says — one image, for showing
people. Save is what keeps the layers.

**A saved document carries its undo history**, so one reopened tomorrow can
still be stepped back through, redo included, with the times intact. It rides as
private entries every other OpenRaster reader walks straight past, so the file is
still an ordinary `.ora`. The limit is the newest 32 MB of edits: on a sketching
session that is under half a megabyte and free, and on an afternoon of
full-canvas painting it is the difference between a 9.7 MB file and a 22.1 MB
one. **Settings → General** switches it off, with the trade stated in megabytes
rather than in adverbs.

## The workspace

The Colour panel offers four ways to pick a colour. The wheel's triangle can
follow the hue as you turn it or hold still, whichever you prefer, and either
centre can be set to whatever angle you like to work at. Umber remembers all
of it, each shape's angle separately.

| Wheel, triangle | Wheel, square | Saturation / value | Sliders |
|---|---|---|---|
| ![](docs/images/picker-wheel.png) | ![](docs/images/picker-wheel-square.png) | ![](docs/images/picker-square.png) | ![](docs/images/picker-sliders.png) |

Panels are **locked while you paint** and rearranged in a mode of their own:
**Window → Customise layout**. In that mode every module is draggable by its
header — drop it into a docked column to stack it there, on a column's edge to
start a new column beside it, or over the canvas to leave it floating, where it
takes no space from the document. Several columns fit on each side, so Colour
can hold the far right at full height with Brushes beside it. The tool rail is a
module like any other: move it, resize it, float it or close it. `Esc` puts a
module back.

**Window → Modules** is every module there is, with a picture of each and a
sentence saying what it is for. Adding one hands it to the pointer and you click
where you want it. Among them is **History**: a viewable list of everything
painted on the document, with a marker showing where you stand — click any entry
to go there. Each row carries the mark of the tool that made it and how long
after the previous one it happened, so the pauses in a session are visible at a
glance; hover a time for the full date. Times are UTC, and an entry from a
document saved before Umber recorded them shows none rather than a made-up one.

Your arrangement is saved between runs.

### Settings

**Edit → Settings**. Interface scale, where pressure comes from, the two themes
— **Graphite** and **Paper** — and a four-way accent choice.

![The settings dialog, Themes pane](docs/images/settings-themes.png)

**Shortcuts** lists every command with a search field, and lets you rebind:
click a key to listen for a new one, add a second key for the same command, or
put one — or the whole table — back to its defaults. Giving a chord to a second
command does not quietly take it off the first; the clash is flagged on both
rows and left for you to settle.

![The settings dialog, Shortcuts pane](docs/images/settings-shortcuts.png)

**Input & pen** is where pressure comes from, next to a live reading of what
your tablet is actually sending. Counters for mouse against pen events, the
choice of pressure source, the pressure the device reported beside the pressure
Umber resolved from it, a trace of the last couple of hundred samples, and a
strip to scribble in. It is the page to open when a pen feels wrong: it says
whether the pen is reaching Umber as a pen at all, and whether pressure really
falls to zero as you lift off — and the switch to a speed-derived stand-in is a
few lines above the trace that shows what it did.

Settings are a plain `key = value` file you can read, in
`%APPDATA%\Umber\config`, `~/.config/umber` or
`~/Library/Application Support/Umber`; the dialog shows the exact path. A
missing, older or corrupt file can never stop Umber starting.

### Autosave

Umber writes your open documents out every five minutes. It waits for a gap
between strokes, so it never interrupts one, and it never pauses the canvas —
the pixels come off the graphics card a little at a time and the file is
written on a thread.

A document you have saved somewhere is written back to that file, replacing it.
A document you have never saved goes to a folder of Umber's own; so does a copy
of everything else, as a second chance. **Settings → General** has the switch,
the interval, and a button that opens that folder.

Those internal copies are deleted once they are a month old — you can change
that, or turn it off. Nothing Umber deletes is ever a file you chose the place
for.

Closing the window with unsaved work asks first, and names every document at
risk.

## Selections

The **Select** tool marks out where edits may land, in one of three ways,
chosen from the dropdown in the tool options strip:

| | |
|---|---|
| **Rectangle** | Drag a box |
| **Lasso** | Draw round it freehand |
| **Polygon** | Click point to point; click the first point again, or press `Enter`, to close |

Everything the brush and the eraser do is then clipped to it, edges included:
the outline is antialiased, so a diagonal is a diagonal rather than a staircase.
A click on the canvas with nothing enclosed clears the selection, as does
`Ctrl` + `D`.

Hold `Shift` while you draw and the new shape is **added** to what is already
selected — two separate areas can both be live, and two that touch become one.
Hold `Ctrl` (`Cmd` on a Mac) and it is **taken away** instead. `Alt` is not the
subtract key here as it is in some applications, because on Umber's canvas it
already picks up a colour and resizes the brush. The modifier is read when the
gesture starts, so you can let go of it part way through a lasso.

The outline stays drawn whatever tool you pick up, because it is how you know
your painting is being held back, and its dashes travel — the classic marching
ants. They march at sixteen frames a second rather than your monitor's rate,
and only while something is selected, so a document you are not touching still
sits still.

## Moving things about

The **Transform** tool picks a region up and lets you move, scale and turn it
before putting it down. Press inside the selection to take it — or anywhere on
the canvas, if nothing is selected, to take the whole layer. Then:

| | |
|---|---|
| Drag inside the box | Move it |
| Drag a corner or an edge handle | Scale it; the opposite handle stays put |
| Hold `Shift` while scaling a corner | Keep the proportions |
| Drag just outside a corner | Turn it about the centre |
| `Enter`, or a press outside the box | Put it down |
| `Esc` | Throw the move away |

Nothing is written to the layer until you put it down, so abandoning a move
costs nothing and undoing one restores both where the pixels went **and** the
hole they came from, in a single step. The marquee travels with the picture it
described. Scaling and rotation resample bilinearly, which is what the preview
shows — the picture you are dragging is the picture that gets committed.

`Ctrl` + `C` copies the selection, or the whole layer where there is none, and
`Ctrl` + `V` pastes it back as a floating region the transform tool is already
holding. A paste lands in the middle of the selection if there is one and
otherwise in the middle of what you are looking at, nudged back on to the canvas
if it would hang off. Something copied from a larger canvas is cropped to what
fits, and Umber says so rather than doing it quietly. The clipboard is Umber's
own: it carries between tabs but not yet to and from other applications.

## Controls

| Input | Action |
|---|---|
| Left drag | Use the selected tool |
| `B` / `E` / `S` / `T` / `H` / `Z` | Brush / eraser / select / transform / pan / zoom |
| `Ctrl` + `D` | Deselect |
| `Ctrl` + `C` / `Ctrl` + `V` | Copy / paste |
| `Shift` / `Ctrl` + drag with Select | Add the new shape to the selection / take it away |
| `Enter` / `Esc` while selecting | Close / abandon the outline |
| `Enter` / `Esc` while transforming | Put the picture down / throw the move away |
| `X` | Swap foreground and background colours |
| `Alt` + click | Pick the colour under the cursor |
| `Alt` + move, nothing held | Resize the brush, against a circle drawn at the size — right and up bigger |
| `[` / `]` | Decrease / increase brush size |
| Middle drag, or `Space` + drag | Pan |
| Wheel | Scroll the canvas up and down |
| `Shift` + wheel | Scroll it side to side |
| `Ctrl` + wheel | Zoom at cursor |
| `Ctrl` + `+` / `-` | Zoom in / out |
| `Ctrl` + `0` / `1` | Fit to window / 100% |
| `Ctrl` + `Z`, `Ctrl` + `Shift` + `Z` | Undo / redo |
| `Ctrl` + `S`, `Ctrl` + `Shift` + `S` | Save / save as… |
| Two-finger drag (touch) | Pan and pinch-zoom |
| Drag a panel header (layout edit mode) | Move that module |
| `Esc` while dragging | Put the module back |

Everything except the held modifiers is rebindable. On macOS `Ctrl` here is
`Cmd`, and the dialog names it that way.

**Pen pressure**: touch screens report it properly, and so do pens on Windows —
a pen arrives through Windows Ink carrying 1024 levels. **Pens on macOS and
Linux do not reach Umber through the window system yet**, so there it falls back
to a flat setting or a speed-derived approximation, chosen in Settings → Input &
pen; a mouse always paints at full pressure. The same page shows you which of
those is happening on your machine. A native tablet path for the other two
platforms is on the roadmap.

## What is not there yet

- **Text and shapes.** The tool rail has six tools where the design draws
  sixteen. Selections work — rectangle, freehand lasso and polygon, painting is
  clipped to them, and they can be added to and subtracted from — and what one
  holds can be moved, scaled and rotated. There is no intersect and no feather.
- **The system clipboard.** Copy and paste work inside Umber and between its
  tabs. Nothing goes to or comes from other applications yet.
- **Mobile.** Android and iOS are prepared for architecturally but have never
  been built or run. Do not believe anyone who says otherwise.
- **Automatic crash recovery.** Autosave keeps copies and Settings will open the
  folder, but nothing offers one back to you the next time Umber starts.
- **Structural undo.** Undo covers painting; adding, deleting or reordering a
  layer is not recorded, and deleting a layer clears the history.
- **Pen pressure on macOS and Linux**, as above. Windows works.
- Navigator, palette and harmony colour modes, per-brush blend modes, and
  stylus tilt.

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
