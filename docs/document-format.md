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
mergedimage.png             the flattened composite, required by the spec
Thumbnails/thumbnail.png    at most 256 px on its long edge, also required
```

`stack.xml` for a two-layer document:

```xml
<?xml version='1.0' encoding='UTF-8'?>
<image w="2048" h="2048" xres="300" yres="300" version="0.0.3" umber-version="1">
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

## Umber's four extra attributes

Four things Umber knows have nowhere to go in baseline ORA. They are written as
extra attributes, which every XML reader ignores if it does not recognise them,
so the file remains an ordinary `.ora` everywhere else. The `umber-` prefix keeps
them clear of anything the specification may add later.

| Attribute | On | Meaning |
|---|---|---|
| `umber-version` | `<image>` | Revision of *these extensions*, not of ORA. |
| `umber-selected` | one `<layer>` | The layer that was being painted on. |
| `umber-blend` | a `<layer>` | Umber's own mode name, where the SVG one is inexact. |
| `umber-background` | the bottom `<layer>` | That layer *is* the document background, and this is its colour. |

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

`umber-version` is bumped only when a revision stores something an older Umber
would drop without knowing it had. A file whose number is higher than the
running build's is **refused**, before a pixel is decoded, with a message saying
so and pointing at the two ways out: update Umber, or open it in another
OpenRaster application — which still works, because the file is still an ORA.
Additions an older build can safely ignore do not need a bump.

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

## What is not saved

- **Undo history.** It is a list of rectangles keyed by texture slot; in a file
  it would be meaningless. A reopened document starts with an empty history.
- **The camera.** A reopened document is framed to fit, like any other.
- **Layer groups, masks, adjustment layers, per-layer blend options.** Umber has
  none of these yet. When it grows them, `umber-version` is the mechanism.
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

- **File → Save** (`Ctrl+S`) writes to the file the document came from, and asks
  for one the first time.
- **File → Save as…** (`Ctrl+Shift+S`) always asks.
- **File → Open** already read `.ora`, so it reads Umber's own documents with no
  new code at all — which is, in miniature, the whole argument for this choice.
- The tab takes the file's name, and the status bar shows the full path. The
  modified dot clears on a successful save.
- Closing a document with unsaved work offers **Save**, Export PNG, Discard and
  Cancel. Save closes the tab only if a file was actually written: a cancelled
  file dialog is not permission to discard.

Both shortcuts are in the same table as every other one (`shortcuts.rs`) and can
be rebound in the settings dialog.

A save reads every layer off the GPU with `read_layer_rect`, which blocks, and
holds all of them at once — 16 MB per layer at 2048². That is the price of a
format that keeps layers. It is paid on an explicit Save and never on the
drawing path.

## Known gaps

- **Saving blocks the frame.** A large document with a full stack takes a
  noticeable moment, and nothing is drawn during it. Moving the readbacks off
  the drawing thread — or making them asynchronous, as colour pickup already is
  — is the fix, and is not done.
- **No autosave, no recovery file, no backup of the previous version.** The
  rename-into-place above protects against a failed write, not against a
  deliberate save over something you wanted to keep.
- **The layer PNGs are written with fast compression.** A working file that will
  be rewritten in a minute is not worth spending seconds on. An "optimise on
  save" option, or a separate archive export, would be the place for the other
  trade.
- **`.ora` is the only extension offered.** Krita's `.kra` and Photoshop's
  `.psd` are read but not written, and there is no reason to write them: both
  applications read ORA.
