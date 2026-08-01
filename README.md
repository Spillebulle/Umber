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

> **Early days.** Painting, layers, brushes, documents and settings all work on
> desktop. There is no mobile build yet, and no selection, text or transform
> tools. [What is not there yet](#what-is-not-there-yet) is honest about the
> rest.

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
still be stepped back through, redo included. It rides as private entries every
other OpenRaster reader walks straight past, so the file is still an ordinary
`.ora`. The limit is the newest 32 MB of edits: on a sketching session that is
under half a megabyte and free, and on an afternoon of full-canvas painting it
is the difference between a 9.7 MB file and a 41.5 MB one. **Settings → General**
switches it off, with the trade stated in megabytes rather than in adverbs.

## The workspace

The Colour panel offers four ways to pick a colour. The wheel's triangle can
follow the hue as you turn it or hold still, whichever you prefer, and Umber
remembers which one you left it on.

| Wheel, triangle | Wheel, square | Saturation / value | Sliders |
|---|---|---|---|
| ![](docs/images/picker-wheel.png) | ![](docs/images/picker-wheel-square.png) | ![](docs/images/picker-square.png) | ![](docs/images/picker-sliders.png) |

Panels are **locked while you paint** and rearranged in a mode of their own:
**Window → Customise layout**. In that mode every module is draggable by its
header — drop it in either sidebar to dock it, or over the canvas to leave it
floating, where it takes no space from the document. The tool rail moves too,
and snaps to whichever side you release it on. `Esc` puts a module back.

**Window → Modules** is every module there is, with a picture of each and a
sentence saying what it is for. Adding one hands it to the pointer and you click
where you want it. Among them is **History**: a viewable list of everything
painted on the document, with a marker showing where you stand — click any entry
to go there.

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

Settings are a plain `key = value` file you can read, in
`%APPDATA%\Umber\config`, `~/.config/umber` or
`~/Library/Application Support/Umber`; the dialog shows the exact path. A
missing, older or corrupt file can never stop Umber starting.

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
| Drag a panel header (layout edit mode) | Move that module |
| `Esc` while dragging | Put the module back |

Everything except the held modifiers is rebindable. On macOS `Ctrl` here is
`Cmd`, and the dialog names it that way.

**Pen pressure**: touch screens report it properly. **Desktop pen tablets do
not report pressure through the window system Umber uses**, so on desktop it
falls back to a flat setting or a speed-derived approximation, chosen in
Settings → Pressure. A native tablet path is on the roadmap.

## What is not there yet

- **Selections, text, shapes and transforms.** The tool rail has four tools.
- **Autosave and recovery.** Saving works and does not lose work when it fails;
  it does not yet protect you from not having done it.
- **Mobile.** Android and iOS are prepared for architecturally but have never
  been built or run. Do not believe anyone who says otherwise.
- **Structural undo.** Undo covers painting; adding, deleting or reordering a
  layer is not recorded, and deleting a layer clears the history.
- **Native desktop pen pressure**, as above.
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
