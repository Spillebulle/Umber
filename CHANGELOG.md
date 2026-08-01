# Changelog

What each release brings. Newest first.

This file is the **source of the release notes**: `tools/release.ps1` and the
release workflow both read the section for the version being tagged and publish
it verbatim, so there is one place to write them and no way for the notes on
GitHub to drift from the notes in the repository.

A section starts at `## <version>` and runs to the next `## `. The topmost
version here must match `workspace.package.version` in `Cargo.toml`; the test
`the_changelog_describes_this_version` fails the build if it does not, so a
release cannot be cut without notes.

## 0.0.2 — 2026-08-01

Documents look after themselves now: they autosave, they remember their undo
history, and closing the window asks before it discards anything. Umber also
tells you when there is a newer version, a document is a canvas size *and* a
background *and* a resolution, and nineteen stamp brushes ship that previously
could only be imported.

### Your work is harder to lose

- **Autosave**, every five minutes and configurable. It waits for a gap between
  strokes and never pauses the canvas: the pixels come off the graphics card a
  little at a time and the file is written on a thread. Measured at under two
  milliseconds on the worst frame of a large document.
- A document with a file is written back to it; every document, saved or not,
  also gets a copy in a folder of Umber's own, which Settings can open.
- Those internal copies expire after a month by default, adjustable or off.
  **Nothing Umber deletes is ever a file you chose the place for.**
- Closing the window with unsaved work asks first, and names every document at
  risk rather than counting them.
- **A saved document carries its undo history**, so one reopened tomorrow can
  still be stepped back through, redo included. Bounded at the newest 32 MB —
  under half a megabyte for a sketching session — and switchable off.

### Documents

- **File → New…** and **File → Canvas settings…**: pixel size with presets, an
  aspect lock, resolution with a millimetre and inch readout, and a background
  that is transparent, white, black or a colour of your choosing.
- The background is a property of the document rather than a filled bottom
  layer, so it can be changed afterwards and erasing cannot punch a hole in it.
- Resizing an existing canvas, with nine anchors for where the artwork lands.
- Resolution rides on OpenRaster's own `xres`/`yres`, so other applications
  read it rather than ignoring an invention of ours.

### Brushes

- **Nineteen brushes that stamp a bitmap tip now ship**, from David Revoy's,
  Raghavendra Kamath's and GDQuest's packs — 239 presets in all, up from 221.
- Krita presets import their dab rotation properly: a compound sensor list, the
  rake's lean, and rotations Umber cannot drive are now named rather than
  silently switched on. Five presets that imported as combs no longer do.
- A GIMP pipe's angular selection is named for what it is.

### Keeping up to date

- Umber asks GitHub for the newest release when it starts and tells you if there
  is one. Switchable off, and the first run says so before it asks.
- It installs the update where doing so is legitimate — a portable copy, an
  AppImage, an `.msi`. A `.deb`, `.rpm`, Arch or Flatpak install belongs to a
  package manager, so Umber names the manager and the command instead of writing
  over files it does not own.
- **Help → About**, with the version, the licence, how this copy was installed,
  and an honest statement that releases are not signed.

### Interface

- A **History** module, movable like any other: what was painted, in order, with
  the tool that made each mark, the gap since the one before, and the exact date
  and time on your own clock. Click an entry to go there.
- A **module library** — Window → Modules — showing every module with a picture
  and a description. Adding one hands it to the pointer to place.
- Document tabs are drawn as leaves of a folder, joined to the strip below.
- The colour wheel and its triangle are antialiased.
- The picker remembers which of its four modes you left it in, and whether the
  wheel's triangle follows the hue.
- Settings moved from Window to Edit. Four fixes to the dialog: the interface
  scale slider no longer runs away from the pointer, the theme cards no longer
  touch or spill past their corners, the shortcut list fills the pane, and the
  dialog's top-left corner is clean.
- The tool rail has its padding back.

### Known limits

Everything 0.0.1 said still applies, except that saving now has an autosave
behind it. Also:

- **Crash recovery is not automatic.** The autosave copies are written and can
  be opened by hand from the folder Settings points at; nothing offers them to
  you on the next start.
- **An explicit save still blocks the frame.** Autosave does not — it reads the
  document a piece at a time — but Save itself has not been moved onto that path
  yet.
- Adding, deleting or reordering a layer is still not undoable, so the History
  list shows strokes only and says so.

## 0.0.1 — 2026-08-01

First release. Umber is an early but usable painting application: it paints, it
opens and saves layered documents, and it ships a large brush library. It is
desktop-only and has never been built for mobile.

### Painting

- GPU canvas built for latency, on Vulkan, D3D12 or Metal via wgpu.
- Brush and eraser. A stroke accumulates into a scratch layer and bakes once, so
  overlapping dabs never compound — a stroke crossing itself is no darker than
  one that does not.
- Pressure drives size, opacity, hardness and scatter, each through a curve of
  its own with its own floor.
- Beyond pressure: speed, slow speed, stroke position, direction and a per-dab
  dice roll can drive any of ten targets, including ellipticity, angle, colour
  pickup and the colour itself.
- Dabs have shape — an ellipse with an angle that can be held fixed, follow the
  stroke, or be thrown per dab — and can scatter off the line and vary their own
  size.
- Bitmap tips: a stamp brush paints its picture rather than a disc, at its own
  proportions.
- Build-up, for the sparse textured stamps that GIMP and Krita composite dab by
  dab. Without it such a brush can never paint stronger than its own brightest
  texel.
- Paper grain bitten into dab coverage, anchored to the document so a second
  stroke lands in the same pits as the first. Three papers ship.
- Colour pickup: a brush can lift colour off the canvas and carry it, including
  its own wet paint. The read is asynchronous, so no frame waits on the GPU.
- Layers with blend modes, opacity and visibility, composited in a single pass.
- Undo and redo, storing only the rectangle a stroke damaged.

### Brushes

- **221 presets ship**, every one carrying its author and licence: all 196
  MyPaint 2.0.2 brushes, 19 procedural brushes from three Krita packs, and six
  of Umber's own. All CC0 except the GDQuest set, which is CC-BY and credited.
- Grouped by style — pencils, inks, markers, charcoal, paint, watercolour,
  airbrush, blenders, erasers, texture, foliage, effects — rather than by pack.
- **Eight brush formats read**: `.myb` (MyPaint), `.gbr` and `.gpb` (GIMP and
  Krita stamps), `.gih` (GIMP animated brushes), `.vbr` (GIMP parametric, which
  Umber reproduces exactly), `.kpp` and `.bundle` (Krita presets and whole
  resource bundles), `.abr` (Photoshop), and `.ron` (an Umber library).
- An import that loses something says what it lost. Only a file written by a
  paint engine Umber has no equivalent for is refused outright, and by name.
- A brush editor reaching every setting a brush has, across six sections, and a
  library you can search, save into, rename and organise.

### Documents

- **OpenRaster** (`.ora`) is the native format — the same decoder that opens
  Krita's and MyPaint's files, so there is no second reader to drift.
- Opens `.ora`, `.kra`, `.psd` and `.png`; exports PNG.
- Several documents open at once in tabs, each with its own layers, history and
  camera.
- A save writes to a temporary neighbour and renames, so an interrupted write
  cannot replace your last good file with a truncated one.

### Interface

- Panels dock, stack, resize, tear off and float, in a layout edit mode of its
  own. The arrangement persists between runs.
- Two themes and four accents; nothing hard-codes a colour.
- Settings with rebindable shortcuts, clash flagging, and a search.
- Colour picker in three modes: hue wheel with a triangle or square centre,
  saturation/value square, and RGB sliders. The triangle can follow the hue or
  hold still.

### Known limits

- **Desktop only.** Android and iOS are architecturally prepared but have no
  build scaffolding and have never been run.
- **Pen pressure is not read from graphics tablets.** Touch screens report real
  pressure; on desktop the fallback is a flat value or a speed-derived
  approximation. A native tablet path is not built.
- **Saving blocks the frame.** A large document pauses visibly while it is
  written, and there is no autosave or crash recovery.
- Deleting a layer clears the undo history.
- 338 brushes out of the fetched packs import but do not ship, because they need
  a bitmap tip and the masks are far larger than the library.
