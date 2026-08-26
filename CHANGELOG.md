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

## 0.1.4 — 2026-08-13

A fix for 0.1.3, which would not start on some machines, smoother lines when
you zoom out, and a new set of icons.

### New

- **The icons are Lucide's.** Umber drew its own, and they are now taken from
  [Lucide](https://lucide.dev), the set used across this family of
  applications. Every mark is the same size and weight it was, and means the
  same thing; five of them stay hand-drawn because Lucide has no mark for a
  layer mask, a deselect, a resize corner or a colour harmony.
- **The canvas is filtered when you zoom out.** Thin lines used to break up and
  crawl as you panned, because each screen pixel showed one document pixel and
  ignored the rest it covered. Each pixel now shows the average of what it
  actually covers. Nothing changes at 100% and above, and nothing changes in what
  gets saved or exported.

### Fixed

- **Umber said it was out of graphics memory on cards with plenty free.** On
  Windows with an NVIDIA card, Umber could refuse to open at all, reporting
  "Out of Memory" on a card with several gigabytes free and almost nothing in
  use. 0.1.3 started asking the driver in advance whether an allocation would
  fit, so a card that was genuinely full could be told about in a sentence
  instead of a crash. That question turns out to be answered against the wrong
  pool of memory on NVIDIA machines without Resizable BAR, so Umber no longer
  asks it. A card that really is full is still reported, just at the point the
  driver says so.

## 0.1.3 — 2026-08-12

Big documents open. A 124 MB Clip Studio file with 54 layers at 20000 x 5000
used to be refused outright; it now opens and uses 1.54 GB of graphics memory
where it would have asked for 19.7 GB. Across a folder of 33 real documents the
saving is 55 GB down to 10 GB.

### New

- **A layer only pays for the parts of it you have painted on.** Layers are
  stored in tiles now, and an empty tile costs nothing. Measured across 33 real
  Clip Studio documents, about 13% of a layer holds paint, so this is most of
  the memory back. The bigger the document, the more it saves.
- **Opening a document no longer inflates it.** Every format Umber reads stores
  layers sparsely and Umber used to expand them to full pages on the way in. The
  same 33 documents went from 59 GB of memory while loading to 9 GB.
- **A document too large for your graphics card is refused with a sentence.** It
  used to stop Umber. The message says what the document needs and what you can
  do about it, and leaves your other work open.
- **Masks are more precise.** A layer mask used to lose about a quarter of its
  levels, so a smooth gradient could band where it reveals. Masks brought in
  from Krita and Clip Studio now arrive at exactly the strength their author
  set.
- **Saving and autosaving use far less memory**, and saving is a little quicker.
  A 24 layer document used to build the whole archive in memory before writing
  any of it.
- **A placed image says what it is.** An image imported into a Clip Studio
  document and left resizable used to be reported as a vector layer, which reads
  as though the file were damaged. Umber now names it and tells you to rasterise
  it.

### Fixed

- **Undoing a canvas flip could damage the document.** With any layer locked,
  the undo was refused but the history moved anyway, and the next undo then
  wrote part of the old picture back mirrored. That could not be undone.
- **A document that failed to load froze Umber.** The loading box had no Cancel,
  nothing else could be clicked through it, and the window could not be closed.
  It now says what happened and lets you carry on.
- **The autosave could stop for the rest of the session, silently.** Saving or
  flipping the canvas while an autosave was running left it stuck; it kept
  running every five minutes, and wrote nothing.
- **An edit during an autosave could produce a file that never existed** — some
  layers as they were, the rest as they are. Painting, undoing, clearing a layer
  and placing text could all do it.
- **A full disk while saving was a crash.** It is now reported and your existing
  file is left alone.
- **A malformed or hostile file could stop Umber before it read anything**, by
  claiming an enormous size in its header.
- **Layer thumbnails could show a painted layer as empty** on canvases wider
  than about 13700 pixels.

### Note

- **Documents with masks saved by this version need 0.1.3 or newer to open.**
  Older versions are refused rather than shown a mask that would be visibly
  wrong. Documents without masks, and everything older, open as before.

### Known limits

- On a fully painted layer the new tile storage costs about a quarter more
  memory than the old one, because a tile at the edge of the canvas is stored
  whole. It is a large saving on ordinary artwork and a small cost on a
  completely covered layer.
- Vector layers and adjustment layers still arrive as a note rather than as
  pixels. Both are named when the document opens.
- A folder's own opacity is folded into its layers, which is exact unless the
  layers inside it overlap.
- macOS has neither thumbnails nor "Open with" yet. Both need Umber to ship as a
  proper `.app` bundle first.

## 0.1.2 — 2026-08-10

Clip Studio documents that used to be refused now open, your files show their
artwork in the file manager instead of a blank page icon, and Umber has every
blend mode Photoshop and Clip Studio do.

### New

- **Thumbnails in the file manager**, on Windows and Linux. A folder of `.clip`,
  `.kra`, `.psd` or `.ora` files shows the pictures rather than a row of
  identical page icons. Umber reads the preview each file already carries, so it
  is quick and it never has to open the document.
- **Umber appears in "Open with"**, and double-clicking a document opens it.
  Umber is added as a choice rather than taking the file type over, so whatever
  opens your `.psd` files today still does.
- **Every blend mode.** Twenty-four of them, including Soft Light, the four
  colour modes and Clip Studio's Add (Glow). Soft Light in particular used to
  arrive as Overlay, which looks quite different.
- **A progress bar while a document loads.** Opening a large file used to freeze
  the window for as long as it took. It now reads the file in the background and
  tells you which layer it is on.
- **Canvases up to 32768 pixels**, where your graphics card allows it. The New
  document dialog offers what your machine can actually hold and says so when
  that is less. Bear in mind one layer that size is over four gigabytes, so a
  canvas that large holds very few of them.

### Fixed

- **Large Clip Studio documents opened.** A 15000 x 5000 file was refused with
  "the canvas is larger than Umber can open" when the canvas was well inside the
  limit. The real limit was on the whole stack, the message named the wrong
  thing, and folders were counted as though they held pixels.
- **Layers that hang off the page arrive.** Five layers of one document were
  dropped as unreadable because Clip Studio had stored them larger than the
  canvas, which is ordinary.
- **The paper layer comes across.** Every Clip Studio document opened on
  transparency where the artist had white paper behind their drawing.
- **A canvas measured in centimetres opens at its real size.** An A4 page at 600
  dpi arrived as a 21 x 29 pixel canvas and was then refused as empty.
- **A vector layer says what it is.** It used to report that the file did not
  hold its pixels, which reads like a damaged file. Clip Studio keeps vector
  layers as strokes; rasterise one before saving and it comes across.
- **The crash box** has the sad cat on it, and its padding is even.

### Known limits

- Vector layers and adjustment layers still arrive as a note rather than as
  pixels. Both are named when the document opens.
- A folder's own opacity is folded into its layers, which is exact unless the
  layers inside it overlap.
- A document needing more memory than your graphics card has will still stop
  Umber rather than being refused politely.
- macOS has neither thumbnails nor "Open with" yet. Both need Umber to ship as a
  proper `.app` bundle first.

## 0.1.1 — 2026-08-09

Clip Studio Paint documents open, there is an eyedropper that reaches the whole
screen, and every number you can drag can now be typed.

Six themes ship instead of two, four of them drawn from the greys of
applications you already use. The canvas size dialog has been rebuilt around
the shape you want first. And palettes can be brought in from wherever you
found them, including a list of hex codes pasted out of a chat window.

### New

- **Clip Studio Paint `.clip` documents open**, layers, folders and masks. That
  makes four applications Umber reads: Photoshop, Krita, Clip Studio and
  MyPaint, alongside OpenRaster and PNG.
- **A Krita group arrives as a folder** instead of being flattened away.
- **An eyedropper tool**, on `I`, and Alt with any other tool in hand. It reads
  the canvas, Umber's own interface, and anything else on your screen: press
  inside the window and drag out to whatever you want the colour of. A loupe
  above the pointer magnifies what is under it and marks the exact pixel a
  release will take. Reading outside the window is Windows only for now, and
  the tool options strip says so where it is not.
- **Every rail's figure can be typed.** Click the number on the brush size,
  opacity or stabiliser rail, on the tool options strip, or in the brush editor,
  and type what you want. A typed figure is not snapped and is not held to what
  the rail can reach.
- **Brush size goes to 1000 px** on the rail, and past it if you type it.
- **The undo memory limit can be typed, up to 32 GB.**
- **Four more themes**: Photoslop, Shit Studio Paint, Krita and MediaBog Pro,
  every grey sampled from the application it is named for.
- **The canvas size dialog, rebuilt.** Pick the shape first — 1:1, 16:9, 9:16,
  4:3, paper or your own — and the sizes under it follow. Paper sizes are
  physical sizes now, so A4 at 300 dpi is A4 and not a poster, and a size your
  graphics card cannot hold is not offered.
- **Palettes can be imported**, from `.gpl`, `.ase`, `.aco`, `.soc`, `.css` and
  plain lists of hex codes. There is a paste box too, so a Coolors link or a row
  of colours out of a chat window comes straight in.
- **A palette has an editing mode**, behind the pencil in its header. With it
  off the grid cannot be changed, because a palette is written to disk the
  moment you touch it and there is no undo for one.
- **Two more harmonies**: a square tetrad and a rectangle one, beside the triad
  and the complements. The `+` in the Palette module adds the whole relation
  when a harmony is showing.
- **A theme's colours open in Umber's own colour wheel** rather than a plainer
  picker, with your own picker settings on the same dialog.
- **The wheel's triangle can be swapped** light corner for dark.
- **The Layers module's commands moved into its header** — new folder, up, down
  and delete — so a short panel no longer scrolls them out of reach. The add
  mark is on the flags row, and "Layer settings" no longer needs saying.

### Fixed

- **The eyedropper took a blend of up to four pixels** rather than the one under
  the pointer. It has done since it shipped.
- **Import and Export on the Shortcuts page did nothing.** They were drawn and
  wired to nothing.
- **The relation picker for harmonies read as a caption**, so the triad and the
  tetrads could not be found. It has a border now.
- **A harmony's other colours were drawn as filled dots**, hiding the wheel
  under them. Every member is an open ring showing the colour beneath it, and
  the one in your hand wears a second ring.
- **Buttons touched each other** in the brush library, on the Shortcuts page and
  in the theme editor.
- **Marks over the canvas could be unreadable on a mid-grey theme.** The
  selection outline, the transform box and its handles took their colours from a
  token, and on the greys of the new themes the dark half and the light half of
  each pair came within a hair of each other. They are derived from the surface
  they land on now.
- **A Clip Studio brush's texture and interval settings were read even when the
  brush had them switched off**, so brushes painted through paper they did not
  have.
- Clip Studio bitmaps could ask for more memory than the canvas could ever use.
- A palette name typed and then clicked away was discarded.
- A rail's typed figure could be overwritten by the drag that closed the field.

### Known limits

- Reading a colour from outside the Umber window is **Windows only**. Picking
  inside the window works everywhere.
- MediBang `.mdp` is still not read. The format is understood; what is missing
  is a sample file that settles which end of its layer list is the top, and
  guessing would silently turn every multi-layer document upside down.
- Photoshop layer masks are still skipped, which is a limit of the library
  Umber reads `.psd` with.
- Harmonies are computed on the RGB wheel, so the complement of blue is yellow
  rather than the orange a painter's colour wheel gives. Krita, Clip Studio and
  Photoshop offer no harmonies at all, so there is nothing to match.

## 0.1.0 — 2026-08-09

Layers can carry effects, and text stays text.

A drop shadow or a stroke now sits on a layer as a setting rather than as paint
you cannot take back. Change the colour, the size or the angle whenever you
like, and the shadow follows the brush while you draw.

Text placed on the canvas can be set again. Reopen the document tomorrow and
the caption is still a caption: fix the typo, change the font, pick bold or
italic. Scaling it up is sharp now, because it is drawn again at the size you
dragged it to rather than being stretched.

### New

- **Layer effects: a drop shadow and a stroke.** Both are settings on the
  layer, not paint. A stroke can sit outside the edge, centred on it, or
  inside. A shadow takes a colour, an angle, a distance, a spread and a
  softness, and it multiplies against what is under the layer the way you would
  expect.
- **Effects follow the brush.** The shadow updates as you paint rather than
  appearing when you lift the pen.
- **Text stays text.** A caption is kept as text in the document, so it can be
  set again after a save and a reopen. Umber writes it beside the picture, so
  the file still opens anywhere that reads OpenRaster.
- **Bold and italic**, from a family's own faces. Umber will not fake a bold by
  thickening an outline or fake an italic by leaning one over, so where a
  family has neither it says which one is missing rather than inventing it.
- **A style picker** listing a family's real styles, including the named
  instances inside a variable font.
- **Text colour**, and a "Convert to paint" command for when you want the
  caption to stop being editable.

### Fixed

- **Scaling placed text was pixelated.** It was being stretched like a
  photograph. It is now drawn again through whatever scale and rotation you
  have given it, so it is sharp at any size.
- **Text blocks reserved far more room than they needed**, which is why a large
  caption could be refused for being too big when it was not.
- **A canvas flip left every drop shadow lit from the old direction.**
- **Clip Studio brushes ignored their Density setting entirely**, and their
  pressure-to-opacity curve arrived meaning the wrong thing, so brushes painted
  darker at a light touch and reached solid before full pressure.

### Known limits

Named here rather than left to be discovered.

- **Moving a placed caption with the transform tool turns it back into paint.**
  You are told, and it can be undone.
- **Text is only kept as text when it is placed on an empty layer**, and only
  up to the size of the canvas. Umber says so at the time.
- **Editing a caption is a button, not live typing.** The canvas updates when
  you ask it to.
- Effects other than the shadow and the stroke are not built yet.

## 0.0.9 — 2026-08-06

The Windows installer works. `umber-setup.exe` opened Umber instead of
installing it, and if you found your way past that it asked for permission and
then sat at nothing for ever. Both are fixed, and the window no longer opens at
twice the height of what is in it.

Colours can be typed. Stamps, papers and palettes can be named and rearranged.
And several ways of losing a stroke, a colour or a picture have gone.

### New

- **Type a colour.** The Colour panel's hex readout is a field now: `#RRGGBB`,
  `RRGGBB` or `#RGB`.
- **Arrange a palette.** Drag a swatch where you want it, give it a name, and
  drop a whole harmony in at once. Names and column count survive a save,
  because `.gpl` always carried them and nothing could set them.
- **Rename a stamp or a paper**, in the browser, and every brush that paints
  with one follows. Each row says how many brushes use it, so you can see what
  a delete would take with it.
- **Cut, Copy, Paste and Deselect are on the Edit menu**, and Zoom in and Zoom
  out on the View menu. Paste had no control anywhere before this: it could
  only be reached by somebody who already knew the key.
- Menu rows show their keyboard shortcut, including Undo and Redo.
- The Pan and Zoom tools say what they do, and name the gestures that reach
  them with a brush still in hand.

### Fixed

- **`umber-setup.exe` opened Umber rather than installing it.** It decided from
  a command-line flag that nothing passes when you double-click a file. It now
  goes by the package it carries.
- **Installing stopped at nothing after you gave permission.** Windows
  Installer was handed its switches in quotes, which it will not read, so it
  raised a dialog where an elevated program's windows cannot be seen and waited
  there.
- **The installer window was about twice as tall as what it showed.** It now
  fits the step it is on.
- **Brushes whose shape changes as you draw left part of the mark behind.** The
  missing edge stayed on screen until the next stroke, then arrived in that
  stroke's colour, and undo could not take it back. Thirty of the shipped
  brushes were affected. The charcoals were worst: they draw round at the start
  of a stroke, and only a tenth of the mark was being kept.
- **A brush wider than its size rail was shrunk by a click on that rail.**
  Fifteen shipped brushes are wider than the rail goes, the largest by more
  than twice. Some spacings, airbrush rates and stroke spans are outside their
  rails too, in both directions.
- **Three ways to lose a stroke.** A second mouse button pressed mid-stroke, a
  pen touching down while a mouse stroke was live, and Alt-Tabbing away while
  drawing all left the stroke unfinished. The last also stopped that document
  being saved automatically for the rest of the session, while its tab still
  showed unsaved work.
- **Escape in the colour field applied what you typed instead of abandoning
  it**, and picking a colour off the wheel put the old one straight back.
- **Renaming a stamp could delete it**, if it was a picture you had copied into
  the folder yourself rather than imported.
- Stamp and paper previews stayed in the old theme's ink after a theme change,
  which left them dark on dark.
- The font picker's count changed the moment you typed, because the two figures
  were counting different things. It named a face you have as one you do not.
- One command, one name: the View menu called "Fit to view" "Fit to window".

## 0.0.8 — 2026-08-05

Brushes now paint the way their authors drew them. Markers, calligraphy pens
and scrapers were laying down a row of separate nib marks instead of a
continuous stroke, textured brushes were painting at a quarter of the opacity
they were set to, and imported brushes were arriving far too faint. All three
had the same cause and all three are fixed.

On Windows, updating no longer shows you a Windows installer. Neither does
installing.

### New

- **Windows installs and updates in Umber's own window.** A new
  `umber-setup.exe` is one file: run it, press Install, and Umber is on your
  machine. Updating is the same window, and the new version opens by itself
  when it has finished. Windows still asks once for permission, because
  installing for everyone on the machine needs it.
- The `.msi` is still published for anyone deploying Umber across several
  machines.

### Fixed

- **Markers, calligraphy pens, scrapers and every other flat nib** painted as a
  comb of separate slivers rather than a stroke. Spacing was measured against
  the nib's long axis when what matters is how far it reaches along the
  direction you are drawing. Round brushes are unchanged.
- **Textured brushes painted at a fraction of their opacity.** A Clip Studio
  sketch pencil arrived at 27% of what it was set to. Its paper is meant to
  bite, and the marks between the bites are meant to build up as the stroke
  passes over them.
- **Imported brushes were far too faint.** A 4H pencil from MyPaint arrived
  painting at a seventh of its real strength; twenty-nine of the brushes Umber
  ships were affected, the faintest at 1.5%. Brushes that already painted at
  full strength are untouched.
- The Brush tweaks module had a column of three-dot buttons that looked like
  menus and did nothing when clicked. They are gone and the sliders have the
  space.

### Changed

- Three paragraphs of explanation came off the Brush tweaks and Text modules.

## 0.0.7 — 2026-08-05

Text on the canvas, a module for changing a brush mid-painting, themes you can
make yourself, and undo that survives deleting a layer. Plus a crash on Wayland,
a colour picker that moved the wrong marker, and Clip Studio brushes that
imported as fixed nibs when their authors had them following the stroke.

### New

- **Text.** Set a line or a paragraph in any font installed on your machine, or
  in a folder of your own. It lands on the canvas as a floating piece you can
  move, scale and turn before putting it down. Text is properly shaped, so
  ligatures and Arabic joining are right. You type it in the panel rather than
  on the canvas, and a character your font has no glyph for is named instead of
  quietly swapped.
- **Brush tweaks.** A module with six rails you can reach while painting:
  hardness, spacing, roundness, airbrush rate, angle and colour pickup. Each has
  a grip you hold and drag, like brush size above the canvas. All eight take a
  keyboard pair too, unbound until you choose one. They change the brush in your
  hand and not the brush you saved.
- **Themes of your own.** Copy Graphite or Paper and type any of its twenty
  seven colours as a hex. It is one small file, so you can export it or bring
  somebody else's in.
- **Stamps and papers have a library.** Import your own, browse what you have,
  and pick one while editing a brush. A paper is previewed tiled, so you can see
  whether its edges meet.
- **Deleting a layer no longer throws away your undo history.** Adding,
  deleting, reordering and renaming a layer are all undoable now, as is adding
  or removing a mask. Clearing a layer is the one command left that still
  clears the history.
- **A harmony wheel a painter would recognise.** It used to answer with the
  opposite colour in light rather than in paint, so blue's complement came out
  yellow instead of orange. It now uses the artist's wheel by default, with the
  additive one still available, and you pick two, three or four colours rather
  than naming a relation.
- **Coloured stamps.** GIMP pixmap brushes arrive in colour rather than as
  silhouettes.
- **258 presets**, up from 239. Krita brushes that ask for a paper texture now
  bring it with them.

### Fixed

- **Umber crashed on Wayland** when a window was dragged to the top of a second
  screen and dropped. It was reconfiguring the display surface while still
  holding a frame, which validation caught. Without validation the same gesture
  would have drawn into a surface that no longer existed.
- **Turning the hue in the colour wheel moved the saturation marker.** Three
  separate causes, the first being that the centre responded to presses well
  outside the shape it draws. There is also a toggle now for swapping the light
  and dark corners.
- **Clip Studio brushes imported as fixed nibs.** A sketching pencil whose
  author had it following the stroke arrived rigid, under a note about a
  stroke-speed setting that was never the problem. Their bitmap tips also come
  across at full resolution instead of from a 300 pixel preview.
- **The Start Menu shortcut and the taskbar button drew Windows' own icons**
  rather than Umber's mark.
- **The installer wears Umber's colours**, and its flow is one page.
- **The canvas scrollbars are always there.** They used to appear only when part
  of the picture was off screen, so zooming out until the whole canvas fitted
  left no way to shift it off centre. `Ctrl+0`, or "Fit" in the status bar, puts
  it back in the middle.
- **The brush editor is one size** whatever section is in front, instead of
  resizing as you moved between tabs.
- **Shipped brushes can be edited from the library**, not only from the panel.
- **The `+` in the Brushes panel makes a brush.** It used to do nothing.
- Eleven Krita brushes had been shipping without the paper texture their authors
  gave them, because of a misread setting name. They are refused for now rather
  than painting wrongly, and six have since come back with their paper.

## 0.0.6 — 2026-08-04

The clipboard now reaches other applications, an autosaved copy is offered back
after a crash instead of waiting in a folder for you to find it, selections gain
intersect and feather, and brushes gain blend modes. Clip Studio brushes were
importing at roughly two thirds of the opacity they were set to — three separate
causes, all fixed.

### New

- **The system clipboard.** Copy and paste pictures to and from other
  applications: a screenshot pastes straight onto the canvas, and a region
  copied out of Umber can be pasted anywhere else. Text works in every field
  too, and the crash box finally has a **Copy details** button.
- **Crash recovery.** If Umber stops without closing properly, the next start
  offers its autosaved copies back — naming each document and when it was last
  written. Opening one puts the painting in front of you and writes nothing over
  the file it came from until you save it there yourself. Saying no keeps every
  copy where it is.
- **Selections gain intersect, subtract and a feather.** Hold `Shift` to add,
  `Ctrl` to take away, both to keep only the overlap; or pick a mode on the tool
  strip. The feather softens an edge by as many pixels as you ask for, and an
  axis-aligned rectangle stays exact on both axes.
- **Per-brush blend modes.** A marker that multiplies into the paper without
  putting the whole layer into Multiply. Brushes that do not set one paint
  exactly as before, and cost nothing extra.
- **A Palette module.** Click a slot to keep the colour in hand, and save a
  whole set to a library. Palettes are plain GIMP `.gpl` files in a folder of
  their own, so anything that reads them can read yours.
- **A Harmony picker mode**, marking the complement, triad, tetrad, analogues or
  split-complement of the hue you are on.
- **Krita transparency masks import.** Opening a `.kra` brings its transparency
  masks across rather than reporting them lost. Krita's filter, transform,
  selection and colorize masks are still named rather than approximated, because
  Umber has no equivalent for them.

### Fixed

- **Clip Studio brushes imported far too transparent.** Setting opacity to 100%
  gave roughly 60%, and laying the stroke down again got closer to the colour
  you asked for. Three causes: a dynamic's floor was read for brush size but
  thrown away for opacity, so a brush meant to paint from six tenths at a
  feather touch painted from nothing; brush density never followed the pen; and
  a texture that was switched *off* still had its value in the file, so
  untextured brushes painted through paper they did not have — mottled, weak,
  and darker on every pass. Pressure now also reaches brush hardness, which was
  never read at all.
- **A document with more than 64 layer slices could not be transformed.** With
  33 masked layers, lifting, pasting and transforming all refused — with 63
  slices free — under a notice saying Umber had run out of room.
- **The Windows pointer stayed on screen under a pen.** Umber had been asking
  for it to be hidden since 0.0.5; the request was being dropped, because a pen
  does not produce the mouse message the window system requires before it will
  hide anything. Umber now hides it directly, and Settings → Input & pen reports
  what it asked for.
- **Umber's folders claimed to be isolated groups.** OpenRaster assumes a folder
  isolates its contents unless told otherwise, and Umber's folders do not — so a
  document with a Multiply layer inside a folder looked one way in Umber and
  another in Krita or GIMP. The file now says what Umber actually draws.
- **A damaged Clip Studio brush file could take Umber down**, and a Photoshop
  file with a compressed layer mask is now refused with a message rather than
  crashing partway through opening.

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
