# Umber's document format

Umber saves to **OpenRaster** (`.ora`). It did not invent a format, and this
document records why, what a saved file contains, what survives a round trip,
what does not, and where the code is.

The writer is `crates/umber-core/src/docformat/`. The reader is
`crates/umber-core/src/docimport/openraster.rs` — the same one that opens
Krita's and MyPaint's ORA files, because there is only one format here and it
deserves only one reader. Both live in `umber-core`, so neither has a GPU or
windowing type and the tests run in milliseconds without an adapter.

## Why not a format of Umber's own

A painting application writing its own container is close to a reflex, and it
was resisted on three grounds.

**There is nothing to invent.** Everything an Umber document holds is a canvas
size and a bottom-to-top stack of layers, each with a name, an opacity, a
visibility, a blend mode and RGBA8 pixels. That is, precisely, the OpenRaster
[layer stack](https://www.openraster.org/baseline/layer-stack-spec.html). A
`.umber` would have been the same information under a different name.

**A second format is a second reader.** Umber already reads ORA, and that reader
is exercised by the import tests and has been run against MyPaint's own files. A
private container would have meant a decoder with no independent implementation
to check against — and two things to keep in step whenever the layer model
grows.

**Work should not be hostage to the application that made it.** A `.ora` is a
ZIP of PNGs plus a small XML file. It can be picked apart with a file manager,
and it opens in Krita, GIMP, MyPaint, Drawpile and Pinta. For an application
this young, that is worth more than any convenience a bespoke format could buy.

The cost, stated plainly: when Umber grows something ORA has no word for —
groups, masks, adjustment layers — the format has to be extended rather than
simply widened. The extension mechanism below is designed for exactly that, and
the version number exists so an older Umber refuses such a file instead of
opening it with pieces missing.

## What is in the file

```
mimetype                    "image/openraster", stored uncompressed, first
stack.xml                   the layer stack
data/layer000.png           top layer
data/layer001.png           …down to the bottom
data/background.png         the document background, when it has one — see below
mergedimage.png             the flattened composite, required by the spec
Thumbnails/thumbnail.png    at most 256 px on its long edge, also required
umber/history/index.json    the saved undo history, when there is one — see below
umber/history/0000-000.png  …and one PNG per piece of each recorded edit
                            (an entry that stores no pixels — a canvas flip —
                            writes none)
```

`stack.xml` for a two-layer document:

```xml
<?xml version='1.0' encoding='UTF-8'?>
<image w="2048" h="2048" xres="300" yres="300" version="0.0.3" umber-version="1"
        umber-history="umber/history/index.json">
 <stack>
  <layer name="Ink" src="data/layer000.png" x="0" y="0" opacity="1.0000"
         visibility="visible" composite-op="svg:multiply" umber-selected="true"/>
  <layer name="Paper" src="data/layer001.png" x="0" y="0" opacity="1.0000"
         visibility="visible" composite-op="svg:src-over"/>
  <layer name="Background" src="data/background.png" x="0" y="0" opacity="1.0000"
         visibility="visible" composite-op="svg:src-over" umber-background="#ffffff"/>
 </stack>
</image>
```

A document with a transparent background has no `data/background.png` and no
last row — it writes exactly the file it always did.

**The first layer in a stack is the uppermost**, per the specification, and
`LayerStack` is bottom first. The writer reverses; the reader reverses back.
Getting that wrong inverts the whole document and is invisible on a symmetrical
test image, which is why there is a test for it on each side.

Layers are written **cropped to their non-transparent bounding box** and placed
with `x`/`y`, which is what ORA is designed for. A sketch is mostly empty
canvas, so this is the difference between a file of a few hundred kilobytes and
one of a few megabytes — and, more usefully, seven-eighths less PNG encoding,
which is what a save actually spends its time on. A layer nobody has painted on
is written as a single transparent pixel rather than as nothing, so that it
survives rather than disappearing from the stack.

## Umber's five extra attributes

Five things Umber knows have nowhere to go in baseline ORA. They are written as
extra attributes, which every XML reader ignores if it does not recognise them,
so the file remains an ordinary `.ora` everywhere else. The `umber-` prefix keeps
them clear of anything the specification may add later.

| Attribute | On | Meaning |
|---|---|---|
| `umber-version` | `<image>` | Revision of *these extensions*, not of ORA. |
| `umber-selected` | one `<layer>` | The layer that was being painted on. |
| `umber-blend` | a `<layer>` | Umber's own mode name, where the SVG one is inexact. |
| `umber-background` | the bottom `<layer>` | That layer *is* the document background, and this is its colour. |
| `umber-history` | `<image>` | Names the entry describing a saved undo history. |
| `umber-clip`, `umber-lock`, `umber-link` | a `<layer>` | Clipped to the one below; locked; in a link group. Written only when set. |
| `umber-link-group` | a linked `<layer>` | *Which* link group, as a number — see below. |

**Resolution is deliberately not one of them.** ORA's `<image>` already carries
`xres` and `yres`, in whole pixels per inch, and that is where a document's DPI
is written and read. Inventing `umber-dpi` beside a standard attribute would
mean other applications ignoring a number they already understand. Umber holds
one resolution rather than one per axis, so both are written with the same
value; a file whose two differ — which the format allows and nothing here can
represent — is read by its horizontal, rather than by an average nobody wrote
down.

`umber-blend` exists for one mode. Umber's **Add** clamps the sum of straight
colour; ORA's nearest name, `svg:plus`, is Porter-Duff addition on premultiplied
colour. They agree wherever both layers are opaque and part company at soft
edges — so the import table (`docimport::blend`) rates `plus` as *approximate*,
which is right for a file from another application and wrong for one Umber wrote
itself. The attribute lets the reader take Umber at its word, so reopening your
own document does not announce a loss that never happened. Saving one still
warns, because other applications *will* composite it slightly differently.

`umber-link-group` is written **beside** `umber-link` rather than instead of it,
and neither moved `umber-version`. A link decides what travels with what when a
layer is dragged; it changes no pixel. So a build that reads only the old flag
opens the same picture and merely has one linked set where this build has three
— which is exactly what that build did with the file *it* wrote. Read the other
way, a file carrying the flag and no group is one written before groups existed,
and every linked layer in it joins group zero: the single set it always was.

`umber-version` is bumped only when a revision stores something an older Umber
would drop without knowing it had. A file whose number is higher than the
running build's is **refused**, before a pixel is decoded, with a message saying
so and pointing at the two ways out: update Umber, or open it in another
OpenRaster application — which still works, because the file is still an ORA.
Additions an older build can safely ignore do not need a bump.

## The undo history

A history that dies with the session means a document reopened tomorrow cannot
be stepped back through, however carefully it was built up. Nothing about the
format prevents carrying one: an ORA is a ZIP, and a ZIP may hold entries nobody
else reads. So the patches go in under `umber/`, described by a manifest and
pointed at by `umber-history`. Every other reader — Krita, GIMP, MyPaint — walks
straight past all of it, which is what keeps the file a plain `.ora`.

Three things had to be got right.

**A texture slot is not a layer.** `PixelPatch::slot` is a slice of the layer
texture array, and slots are **recycled** — which is why a deleted layer's
slice is parked inside the undo entry that could put it back rather than
returned to the pool. A slot written into a file and read back into a
*different session's* allocation is that same bug with no such defence. So
a slot is never written: each entry names a **stack position**, bottom first,
which is the order the file itself is in and the order the reader rebuilds.
`SaveHistory::new` makes that mapping once, at save time, and refuses the whole
history if any patch names a slot no layer holds; `ImportedDocument::open` turns
the positions back into whatever slots the reopened stack allocated.

**Anything that does not line up exactly is dropped.** The manifest carries the
canvas size and the layer names in order, as a fingerprint of the stack the
positions index into, and the reader compares them against the layers that
actually *loaded* rather than against what `stack.xml` described — a layer that
failed to decode shifts every position after it. A mismatched canvas, a renamed
layer, a missing or wrong-sized patch, or a manifest from a later revision each
drop the whole history and append an `ImportWarning`. There is no half-restored
state: the entries are a sequence in which each restores the pixels the next one
expects, so one missing from the middle is not a shorter history but a wrong
one. A history that replays into the wrong layer is far worse than no saved
history.

**Size is the whole problem.** Memory allows 512 MB of raw RGBA; writing that
verbatim would produce multi-gigabyte documents from an ordinary session. The
file gets its own budget — 32 MB of *encoded* patch data, oldest dropped first,
which is the direction the in-memory budget already ages entries out in — and
the encoder runs newest-first and stops there, so a session far over it pays
neither the time nor the space for what it will not keep. Patches are stored as
PNG at `Compression::Fast`, the same encoder the layer images use, because PNG
filters each row against the one above before it deflates:

| session (2048² canvas) | raw patches | Deflate | PNG (fast) |
|---|---|---|---|
| 120 full-canvas strokes | 54.8 MB | 18.3 MB (3.0×), 0.85 s | 12.3 MB (4.5×), 0.12 s |
| the same, heavily grained | 61.2 MB | 30.9 MB (2.0×), 1.44 s | 30.6 MB (2.0×), 0.19 s |
| 300 small sketching strokes | 2.6 MB | 0.34 MB (7.7×), 0.10 s | 0.38 MB (6.8×), 0.01 s |

Deflate is what the ZIP would have done for nothing, so it is the alternative
worth measuring, and it loses on size everywhere but the sketch — where the
difference is 40 kB — and on time by an order of magnitude everywhere.
`cargo run --release -p umber-core --example measure-history` is where those
numbers came from, and it is checked in so they can be re-measured rather than
trusted.

The raw column is smaller than it once was, and the ratios with it: a patch is
now the cells a stroke reached rather than the rectangle round them, so most of
what PNG used to squeeze out for nothing — canvas the stroke never went near —
is no longer stored in the first place. The bytes on disk are unchanged or
better.

What it costs end to end, on a one-layer 2048² document:

| session | file | save | open |
|---|---|---|---|
| 300 small strokes | 2.67 → 3.15 MB | 0.07 → 0.09 s | 0.03 → 0.04 s |
| 40 full-canvas strokes | 5.28 → 6.97 MB | 0.12 → 0.14 s | 0.05 → 0.07 s |
| 120 full-canvas strokes | 9.68 → 22.13 MB | 0.13 → 0.25 s | 0.05 → 0.18 s |

Opening is in there because the patches have to be decoded before the document
appears, and that is time the artist spends waiting. The budget now keeps all
120 of those strokes, all 120 heavily grained ones — 30.6 MB, only just — and
all 300 of the sketching session with room to spare. It used to bind at 62 of
the 120, and the number was left where it is: what a file will carry is a
judgement about documents on somebody's disk, not a consequence of how well
this release happens to pack them.

The last row is still why this is a **preference** rather than a policy: an
afternoon of full-canvas painting more than doubles the document, and somebody
synchronising their work to a network drive is entitled to refuse that. Settings, General has the switch. It is on by default,
because the common case is the first row and because a feature nobody knows to
switch on is one nobody gets.

The patch PNGs hold layer-texture bytes **unconverted** — sRGB-encoded with
alpha premultiplied — rather than the straight alpha the layer images are
converted to. That conversion exists so other applications can read them;
nothing else reads these, so converting would be two passes over a hundred
megabytes to arrive back where it started, and it would put a rounding step
between undo and the pixels it restores.

Nothing here reaches the GPU. The patches have been in memory since they were
captured at commit time, so a save's blocking readbacks are exactly what they
were before.

### When each edit was made

Every manifest entry carries an optional `at`: milliseconds since the Unix
epoch, UTC. That is what lets a reopened document's History list still say how
long the artist spent between one mark and the next, rather than losing the
shape of an afternoon the moment the file is closed.

A number rather than a formatted date, because parsing a date is a place to be
wrong and every consumer of the field wants the integer anyway. UTC because a
local time would have to name a zone, and reading it back on a different machine
would then mean carrying a time-zone database to interpret it.

Optional in *both* directions, and that is the whole compatibility story. An
entry with no recorded time — one that came out of a document written before
this existed and is on its way back into one — omits the field entirely, so such
a history is written byte for byte as it was before. And a manifest that never
had the field reads back as "not known" rather than failing to parse, which
would have cost the document its whole history over a column the picture does
not depend on. The History module draws an empty cell for it; a time invented at
import — the file's modification date, say — would be indistinguishable from a
recorded one, and that is the failure worth avoiding.

### Why this does not bump `umber-version`

The bar is that a revision storing something an older build would drop
*silently* must be refused by it. An older Umber ignores an archive entry it has
never heard of and opens the document with an empty history — which is precisely
what every build before this one did with every file. Nothing about the picture
is lost. Refusing to open somebody's painting because it carries a history they
do not need would be a plainly worse trade than the one it avoids.

A separate `history::VERSION` governs the manifest's own layout instead, and a
manifest from a newer one is **discarded**, not refused: the document still
opens, exactly as it would have before histories were saved at all.

That number answers to the same bar, one level down: it moves for a revision an
older build would **misread**, and not for one it can simply ignore. Adding the
per-entry timestamp did not move it. Serde skips a field it has never heard of,
so a build predating it restores every patch and every position exactly as
before and merely shows no times; bumping would instead have made that build
throw the whole history away — all the pixels, to avoid losing the clock. The
test `a_manifest_from_a_newer_revision_is_discarded_and_not_refused` pins the
behaviour a genuine bump would rely on, so that raising the number stays a safe
thing to do when something eventually earns it.

Revision **2** is the first thing that earned it. A patch became a set of
pieces — the cells of the canvas a stroke actually reached, rather than the
rectangle round them — and an entry now lists them. A build reading only
revision 1 would ignore that list, take the entry's first PNG for the whole
rectangle, and write it back over pixels that were never part of the edit:
a document quietly damaged by an undo, which is the one outcome worth losing a
history over. It discards the history and opens the picture whole instead.
This build still reads revision 1, where an entry with no pieces is one piece
covering its rectangle.

Revision **3** is the second, and it is the sharper case. An entry can now be a
**canvas flip**, which carries no pixels at all: a flip preserves the canvas
size, so nothing already recorded stops being valid, and it is its own inverse,
so undoing one is flipping again. That works because the timeline is stepped
rather than seeked — by the time an older patch is reached the flip above it has
already been undone, and the patch applies verbatim at the rectangle it names.
A build that reads only revision 2 does not know the kind, and what it would do
is not "one entry short": every entry older than the flip was recorded in the
opposite orientation, so dropping the flip would write each of those patches
back mirrored. It discards the whole history and opens the picture whole.
`umber-version` is **not** bumped for this, for the reason above — an older
build opening with an empty history is exactly what every build before histories
did.

## The background, and why it is a real layer in the file

A document's background — transparent, white, black or a colour — is a
*property*, not a layer. Filling the bottom layer instead is what a painter
would do by hand, and it is the wrong model here: it cannot be changed
afterwards without repainting, erasing on that layer punches a hole through to
the checkerboard, and "transparent" stops being expressible. So it composites
**under** the stack, inside the one pass the layers already use.

ORA has no word for that, and the obvious extension — an attribute on `<image>`
naming a colour — has a cost that is easy to miss: **every other application
would open the document on transparency**. A white painting arriving in Krita on
a checkerboard is not a dramatic failure, which is exactly what makes it a bad
one. Nobody notices until they export.

So the colour is written **both** ways:

- A full-canvas opaque `<layer>` named "Background", at the bottom of the stack,
  carrying the real pixels for everyone else.
- `umber-background` on that layer, which is how Umber's own reader knows to turn
  it back into the property rather than into a layer the painter never made.

They cannot drift apart, because the writer produces both from one value. What
each reader sees:

| | |
|---|---|
| This Umber | the attribute; the layer's PNG is never even decoded |
| An older Umber | one extra opaque layer, and **the same picture** |
| Krita, GIMP, MyPaint, Pinta | one extra opaque layer, and the same picture |

That last row is the whole argument, and the middle one is why `umber-version`
is **not** bumped for this. The rule is that a revision storing something an
older build would drop *silently* must be refused by it — and nothing is dropped
here. An older build shows every pixel in the right place, with the background
merely editable. Refusing to open somebody's document would cost more than that
degradation does.

The honest caveats, both of them consequences rather than defects:

- A document edited and re-saved by another application comes back to Umber with
  the background as an ordinary layer, because that application will not have
  kept an attribute it does not understand. The picture is unchanged; the
  property is not.
- The file costs one canvas-sized PNG of a solid colour. That is a few kilobytes
  after deflate — every row after the first filters to zeroes — plus one
  canvas-sized buffer built while the archive is. Beside the layer readbacks a
  save already blocks on, it does not register.

Since the background is an extra `<layer>`, a document already at
`LayerStack::MAX` writes `MAX + 1` of them. The reader takes the background out
of the list *before* it counts, which is what stops Umber writing a file it
would then refuse to open.

## Pixels, and why the round trip is exact

Umber's layer textures hold sRGB-encoded colour with **alpha premultiplied in
linear space** — `commit.wgsl` renders that, and `composite.wgsl` samples it.
ORA, like every interchange format, stores **straight** alpha. So every layer is
converted on the way out and back on the way in, by `docimport::srgb`, which
owns both directions precisely so they cannot drift apart:

```
stored   = srgb_encode( srgb_decode(source) * alpha )   // opening a file
source   = srgb_encode( srgb_decode(stored) / alpha )   // saving one
```

On the bytes a layer texture can actually contain, those are exact inverses:
`saving_and_reopening_does_not_move_a_pixel` drives every reachable
(colour, alpha) pair through both and asserts the byte lands on itself. A
document therefore does not drift a level per save, which it very easily could
have.

What is *not* reversible is the other direction, and it is worth being clear
about: a premultiplied byte at alpha 1/255 has only fourteen reachable values,
so colour in near-transparent pixels is quantised when a file is opened. That
loss belongs to the texture format, not to this one, and it has already happened
by the time anything is saved.

## What round-trips

Written, read back, and asserted on in `docformat`'s tests:

- Canvas size, background and resolution.
- The whole stack, bottom to top, including its order.
- Per layer: name (including one full of XML metacharacters), visibility,
  opacity, blend mode, and every pixel byte for byte.
- Which layer was selected.
- Empty layers, and layers painted in one corner.
- The undo history: both stacks, the position within the timeline, how many
  older entries the budget had already dropped, when each edit was made, and
  every patch byte for byte — on the layer it was recorded against, whatever
  slot that layer ends up with. A canvas flip too, which carries no pixels and
  comes back as the entry that mirrors the picture again.

## What is not saved

- **The camera.** A reopened document is framed to fit, like any other.
- **Layer groups, adjustment layers, per-layer blend options.** Umber has none
  of these yet. When it grows them, `umber-version` is the mechanism — which is
  exactly what layer masks and clipping used it for: both are saved, both change
  the picture in a build that ignores them, so a document carrying either
  declares revision 2 and an older Umber refuses it rather than opening it with
  the masks quietly gone. A document with neither still declares revision 1 and
  still opens anywhere.
- **Anything about the application** — tool, brush, palette. Those are
  preferences and live in the config directory.

## Failure, and not losing work

The archive is built whole in memory and written to a temporary neighbour
(`sketch.ora.saving`), which is then renamed into place. A save that fails
halfway — a full disk, a pulled drive — would otherwise leave a truncated
archive where the artist's last good version used to be, which is the one
failure a save must not have.

Refusals happen before anything is written:

| Refusal | Reason |
|---|---|
| More than `LayerStack::MAX` layers | The file would be valid ORA and unopenable here. |
| A layer buffer that is not canvas-sized | A caller bug that would shear the image. |
| No layers at all | Not a document. |

`SaveError` and `SaveWarning` both `Display` as finished sentences written for
the user, like `ImportError` and `ImportWarning`, and the UI shows them
verbatim.

## In the application

- **File → New…** and **File → Canvas settings…** are the same four questions —
  size, background, resolution, and where existing artwork is anchored when the
  size changes. The first three are what this file carries; the anchor is a
  one-off instruction to the renderer and is not saved, because it describes a
  move rather than a document.
- **File → Save** (`Ctrl+S`) writes to the file the document came from, and asks
  for one the first time.
- **File → Save as…** (`Ctrl+Shift+S`) always asks.
- **File → Open** already read `.ora`, so it reads Umber's own documents with no
  new code at all — which is, in miniature, the whole argument for this choice.
- The tab takes the file's name, and the status bar shows the full path. The
  modified dot clears on a successful save.
- Closing a document with unsaved work offers **Save**, Export, Discard and
  Cancel. Save closes the tab only if a file was actually written: a cancelled
  file dialog is not permission to discard.
- **Settings → General → Save the undo history in the document** turns the
  history off. Its note states the trade in megabytes; see above for where those
  numbers come from.

Both shortcuts are in the same table as every other one (`shortcuts.rs`) and can
be rebound in the settings dialog.

A save reads every layer off the GPU with `read_layer_rect`, which blocks, and
holds all of them at once — 16 MB per layer at 2048². That is the price of a
format that keeps layers. It is paid on an explicit Save and never on the
drawing path.

## Autosave writes the same file by a different route

An autosave produces a byte-identical archive — same encoder, same atomic
write — and gets there without ever blocking a frame, because it happens on a
timer rather than because somebody asked for it. Three things differ, and each
is argued at its call site in `umber-app/src/autosave.rs`:

- **The pixels come off the GPU asynchronously**, through
  `CanvasRenderer::begin_capture`: one layer in flight at a time, through one
  reused staging buffer, read out four megabytes per frame. Measured on a
  2048-square eight-layer document that adds about a millisecond to the worst
  frame, against a whole readback of roughly 90 ms.
- **The encode and both writes happen on a background thread.**
  `docformat::write_encoded` is `save`'s atomic write split out for it, so one
  archive reaches two destinations without being encoded twice — and so there
  is one temp-and-rename implementation rather than two.
- **The undo history is not written.** `SaveDocument::history` is `None`. It is
  up to 32 MB of PNG-encoded patches, and re-encoding all of it unattended
  every five minutes is a cost nobody asked for; an autosave exists so the
  painting is not lost, not so the afternoon can be replayed.

A document that has a path is written to that path — **overwriting it without
asking**, which is what an autosave is — and to an internal copy beside it. One
that has never been saved is written only to the internal copy: Umber has not
been told where the painter wants it, and putting a file in their documents
folder uninvited is not an answer.

Deleting expired internal copies is `autosave::Reaper`'s, and it is the only
thing in Umber that deletes a document. Its containment is structural rather
than a matter of callers being careful — one canonicalised root, every
candidate resolved independently, the candidate's parent required to *equal*
that root, symbolic links refused before they are resolved, and no recursion.
See its own documentation.

## Known gaps

- **Saving blocks the frame.** A large document with a full stack takes a
  noticeable moment, and nothing is drawn during it. Moving the readbacks off
  the drawing thread — or making them asynchronous, as colour pickup already is
  — is the fix, and is not done.
- **No backup of the previous version, and no automatic crash recovery.** The
  rename-into-place above protects against a failed write, not against a
  deliberate save over something you wanted to keep. Autosave writes the same
  archive to an internal folder as well as to the document's own file, so a
  recent state is usually recoverable by hand — but nothing reads those copies
  back on the next start, and there is no "revert to saved".
- **The layer PNGs are written with fast compression.** A working file that will
  be rewritten in a minute is not worth spending seconds on. An "optimise on
  save" option, or a separate archive export, would be the place for the other
  trade.
- **`.ora` is the only extension offered.** Krita's `.kra` and Photoshop's
  `.psd` are read but not written, and there is no reason to write them: both
  applications read ORA.
- **A saved history does not survive a layer being added, deleted or reordered
  after it was written.** The manifest fingerprints the stack by name and order,
  so a document whose layers have moved drops its history on the way in — which
  is the safe direction, and the same limitation the in-memory history already
  has, for the same reason. Structural undo is what fixes both.
