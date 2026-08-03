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

## 0.0.5 — 2026-08-03

Layer folders, a selection that carries its own commands, and three bugs that
had no business shipping: opening the brush library could take the application
down, half the canvas gestures did nothing at all with a pen, and moving part of
a picture left a faint outline of the selection behind.

### Three things that were broken

- **Opening the brush library crashed Umber.** A texture was handed back to the
  graphics card while a drawing command still named it, which is a fatal error
  and takes the process with it. The library and the Brushes panel also shared
  one preview per brush while showing it at two sizes, so each was throwing away
  the other's picture every frame.
- **A pen could not reach half the canvas gestures.** Alt-drag to resize the
  brush, the Pan tool, the Zoom tool, Alt to pick up a colour, the space bar,
  and the polygon selection's rubber band all worked with a mouse and did
  nothing with a pen — the resize worse than nothing, since the press fell
  through and painted. What a press *means* is now decided in one place for both
  kinds of pointer, so a gesture reaches both or neither.
- **Moving a selection left a ghost of it behind.** Lifting pixels through a
  selection applied its edge a second time, so a one-pixel outline of the
  marquee stayed on the layer and the piece that moved was fainter than it
  should have been. Copying had the same fault, at a quarter of the mark it was
  taken from.

### Layers

- **Folders.** Group layers, nest them, fold them shut. A folder's eye and lock
  reach everything inside it, and they travel as one when moved. They are saved
  as ordinary nested stacks, so another application opens the file and shows the
  identical picture.
- **Thumbnails show what is on the layer**, scaled to fill the row rather than
  the whole canvas shrunk into it, so a single stroke reads as a single stroke.
- **Tick boxes**, and one line of buttons that act on everything ticked: show,
  hide, lock, unlock, link, group, delete.
- **Link groups.** Up to six sets of layers that move through the stack
  together, each with a colour of its own, so a row says *which* set it belongs
  to and not merely that it belongs to one.
- **Dragging a layer says where it will land**, as a dashed outline stepped in
  to the nesting it would take — inside a group, or beside it. Dragging past
  either end of the list means the top level, which is how a layer comes out of
  a folder and how a second top-level folder gets made.
- A layer inside a locked folder now shows a padlock. It refused strokes before
  and said nothing about why.

### Selections

- **Deselect, Copy and Cut, on buttons over the canvas**, beside the selection
  itself. They go away while a selection is being transformed, where the flip
  buttons take that place.
- **Cut**, which there was not one of. Copy and paste already worked; cut takes
  exactly what it leaves behind, so an antialiased edge does not lose a rim to
  rounding.

### When something goes wrong

- **A crash gets a real dialog** instead of a window that vanishes: what
  happened, which documents were rescued and which were not, the full technical
  details in a box you can expand and select, where the report was written, and
  a button to start Umber again. It is drawn by a fresh process, so it still
  works when what died was the graphics device.
- **The Windows build no longer opens a console window** behind the
  application.

### Updating

- **Updating is a dialog with real progress**, rather than a silent download and
  an installer appearing. It shows the release's own notes, the version you are
  on and the one you would move to, and then what it is actually doing —
  downloading, unpacking, installing — with a countdown to restart at the end
  that can be stopped. "Never ask again" is the same setting Settings has.

### Known limits

- **A folder has no opacity or blend mode of its own.** It composites exactly as
  its contents do in place, which is why an older Umber and other applications
  read the file correctly. Group compositing is a larger change and is not
  built.
- **Linked layers still do not transform together.** They move through the stack
  together; a transform reaches one layer.

## 0.0.4 — 2026-08-02

Masks, locks and links on the layer stack; brushes you can make yourself,
including drawing the stamp on the canvas; an export dialog with five formats;
and a transform that turns the way you expect it to.

### The layer stack

- **Layer masks.** Paint a greyscale mask to hide and reveal part of a layer
  without touching its pixels. A switch on the layer row says whether a stroke
  lands in the layer or in its mask, and the row shows which you are aimed at.
  Taking a mask off clears the undo history, and the button says so first.
- **Clipping masks.** Clip a layer to the one below it, so it only shows where
  that layer has paint. A run of clipped layers answers to the nearest unclipped
  one under it.
- **Locking.** A locked layer refuses strokes, transforms, clearing, deletion
  and canvas flips. Nothing offers you an action it will then refuse — the
  controls go quiet and say why.
- **Linking.** Linked layers move through the stack together. They do not
  transform together yet; the README says why.
- **Drag the layers to reorder them**, as well as the arrow buttons.
- The blend mode and opacity controls now say they belong to the selected layer.
  They always did; nothing about them said so.

### Making brushes

- **New brush**, beside Import in the library.
- **Give any brush a bitmap stamp** — one already in your library, or an image
  you import.
- **Draw the stamp yourself.** Umber opens a canvas set up for it and says at
  the top which brush it is for. What you paint is the coverage: colour is
  ignored, opacity is the strength, and the eraser takes coverage back off.
- **Collections you make yourself**, to file brushes into.
- The preview beside each brush is now a real stroke with a loop in it, drawn by
  the engine that draws on the canvas — so a rake, a chisel or a brush that
  follows the stroke previews as what it is, instead of as the same flat bar
  every other brush drew.

### Exporting

- **PNG, JPEG, TIFF, GIF and BMP**, from a dialog that names what each one
  costs before it writes: the colour transparency is painted onto, GIF's 256
  colours, JPEG discarding a little more every time. A document with nothing to
  lose is told so rather than warned at.

### The transform tool

- **Rotation worked out badly and now works.** It applied the whole angle
  between your hand and the grab point on *every* frame, so the box spun away
  from the pointer, always in the same direction. Turning now follows the
  pointer: grab at 45° and move to 50°, and it turns five degrees.
- **Turn from anywhere outside the box**, not only from a small ring at the
  corners.
- **Flip buttons** above the box, and dragging a handle past the opposite side
  flips as well.

### Elsewhere

- **Flip the whole canvas**, horizontally or vertically, without losing your
  undo history — the flip is recorded as an edit like any other.
- **The undo memory limit is yours to set**, in Settings. It was a fixed 512 MB
  per document.
- **The settings window stops changing size** as you move between its pages.
- **Type the number.** The colour wheel's angle and the interface scale can both
  be typed as well as dragged, and each lands on useful steps — 45° and 25%.
- The colour wheel's triangle is no longer jagged when you turn it.
- Every dropdown in the interface is the same control; there were four.
- The dashed outlines in layout edit mode have corners again.

## 0.0.3 — 2026-08-01

Getting around the canvas, and a crash on large documents. The wheel now
scrolls, there are scrollbars when part of the picture is off-screen, zoom has
keyboard shortcuts that work on your own keyboard rather than an American one,
and a drawing tablet draws.

### A large canvas no longer takes the application down

- **A document bigger than about 8000 pixels square crashed at the end of the
  first stroke.** Reading pixels back off the graphics card asked for one buffer
  the size of the whole layer, and a 10000² canvas is 400 MB against a 256 MB
  limit — which is a fatal error, so the painting went with it. Every readback
  now comes back a band of rows at a time.
- The same fault was in the flat PNG export and in the autosave, where it had
  not fired yet. All three are fixed together.
- Nothing changes for an ordinary canvas: where the whole document fits, it is
  read in exactly the one copy it always was.

### Getting around the canvas

- **The wheel scrolls the canvas** up and down, and **Shift and the wheel**
  scrolls it side to side. Zoom moves to **Ctrl and the wheel**. A horizontal
  wheel works too, which it did not before.
- **Scrollbars** appear along the bottom and the right of the canvas whenever
  part of the document is outside the view — whether it is too big to fit or
  merely pushed under a panel — and are not drawn at all when it is not.
- **Ctrl+plus and Ctrl+minus zoom**, and they zoom the *canvas*: they used to
  scale the whole interface, and Ctrl+0 did both at once, fitting the document
  and resetting the interface scale in the same press. Interface scale is a
  slider in Settings and stays there.
- Trackpad scrolling uses the trackpad's own resolution rather than rounding it
  into wheel notches.

### Keyboards that are not American

- **Punctuation shortcuts follow the key your keyboard actually prints.** They
  were bound to US key *positions*, so on a Nordic layout the key marked `+`
  zoomed out and the key marked `-` did nothing; German, French and Spanish
  layouts each moved them somewhere else again.
- Letters and digits deliberately still go by position. One known consequence:
  on a QWERTZ keyboard the key marked `Z` is where `Y` sits on an American one,
  which is Umber's second Redo shortcut — so Ctrl+Z there redoes. Not fixed yet.

### Drawing tablets

- **A pen draws.** On Windows a pen arrives by a different route from a mouse
  and never reports a cursor position through the usual one, so every pen press
  was tested against wherever the mouse had last been left — usually the menu
  bar — and thrown away. A pen display did nothing at all on the canvas.
- Pressure from the pen is used, at the 1024 levels Windows carries, however
  many the tablet itself distinguishes.
- A single touch on a panel no longer cancels a stroke the other hand is in the
  middle of, and a pen hovering above the glass is no longer mistaken for a
  finger — which used to stop drawing entirely until Umber was restarted.

### Known limits

Everything 0.0.2 said still applies. Also:

- **There is no brush cursor.** Other painting applications show a ring the size
  of the brush as you hover; Umber shows the ordinary arrow.
- **Pen pressure is Windows only.** macOS and Linux have no tablet path yet and
  fall back to full pressure or the speed-derived approximation.
- Scrollbar drag has not been exercised on a touch screen.

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
