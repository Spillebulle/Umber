# Importing documents from other painting applications

Umber can open layered files written by other programs. This document records
which formats were investigated, which of them landed, what each one loses on
the way in, and — for the ones that did not land — the reasoning, so the
question does not have to be re-opened from scratch.

The code is `crates/umber-core/src/docimport/`. It is part of `umber-core`, so
it has no GPU or windowing types and its tests run in under a second without an
adapter.

## The rule

**An import that produces subtly wrong pixels is worse than one that refuses.**

A refusal costs the artist one step: export an ORA or a PNG from the program
that wrote the file. A wrong import costs them an afternoon and their trust —
worse still if what is wrong is a gamma curve or a stacking order, which look
almost right until they are compared side by side.

Everything below follows from that. Where a format could be read only by
guessing, it is not read. Where a document can be read but not represented —
groups, another application's masks, blend modes Umber lacks — it is imported
and the loss is reported in `ImportedDocument::warnings`, which the UI shows.
Clipping is no longer in that list: Umber's own flag means what Photoshop's
does, so a clipped `.psd` layer arrives clipped. Umber's own masks arrive too,
out of its own `.ora`; so does a Krita **transparency** mask, and so does a Clip
Studio layer mask. Krita's other four mask kinds — filter, transform, selection,
colorize — are reported as lost, because Umber has no equivalent for any of
them. A `.psd` mask is reported
as lost for a different reason: the pinned `psd` crate skips the block holding
the mask's own rectangle, keeps the bytes behind a private accessor, and panics
on an RLE mask channel, so reading one means a second parser beside the crate's.

## Verdicts

| Format | Verdict | Notes |
|---|---|---|
| `.ora` OpenRaster | **Landed, exact** | Open specification; layers, offsets, opacity, visibility and blend modes all arrive. Also what Umber *writes* — see `document-format.md`. |
| `.kra` Krita | **Landed, layer-aware** | 8-bit RGBA documents read tile by tile. Anything else falls back to the embedded composite, with a warning. |
| `.psd` Photoshop | **Landed, lossy** | 8-bit RGB only. Groups flatten and masks are dropped; clipping arrives. A file with an RLE-compressed mask channel is refused outright, because the `psd` crate panics on one. Every loss is reported. |
| `.png` | **Landed, exact** | A single layer. Also the decoder ORA uses. |
| `.psb` Photoshop large | Declined | Header version 2; the `psd` crate reads version 1 only. |
| `.clip` Clip Studio | **Landed, layer-aware** | Layers, folders, masks, blend modes, opacity and locks arrive out of the embedded SQLite database. Correction layers are refused; text and vector layers arrive rasterised. Every loss is reported. |
| `.procreate` | Declined | Undocumented LZ4 tile layout behind a binary plist. |
| `.mdp` MediBang | Declined for now | Specified, and there are real samples — but nothing available settles whether the layer list runs top first or bottom first, and two of its three tile codecs are unwritten. |
| `.xcf` GIMP | Declined for now | Documented but large; several precisions and a colour model of its own. GIMP exports ORA. |
| Layered `.tiff` | Declined | Photoshop's layers ride in a private tag as a PSD blob; a plain multi-page TIFF has pages, not layers. |
| `.jpg` | Declined for now | Needs EXIF orientation handling to avoid importing photographs sideways. |

## Colour space — the part that is easy to get wrong

Umber is linear RGBA internally, but its layer textures are `Rgba8UnormSrgb`
and `commit.wgsl` renders **premultiplied linear** colour into them, so
`composite.wgsl` can treat what it samples as premultiplied. The byte in the
texture is therefore

```
stored = srgb_encode( srgb_decode(source) * alpha )
```

Every format here stores sRGB-encoded, *straight*-alpha RGBA8, so every import
runs that transform (`docimport::srgb`). Two things make it worth its own module
and five tests:

- Multiplying the sRGB byte by alpha — the obvious one-liner — is wrong by a
  whole gamma curve on every partly transparent pixel. 50% white must arrive as
  sRGB ~188, not 128.
- The transform is the **identity for opaque pixels**, so a wrong version looks
  perfect on any screenshot without transparency. It shows up only later, as
  haloed or washed-out edges on real artwork.

The conversion is a 64 KiB lookup table over every (value, alpha) pair: exact,
and fast enough that a 4096² import does not stall on `powf`.

Colour *management* is deliberately not attempted. ORA and PNG mean sRGB; Krita
names its profile, and a document in a linear or Rec.2020 profile is imported
with a warning rather than silently converted. Doing better needs an ICC engine
Umber does not have.

## Two things every import decides, and one of them is not a loss

**The background.** Umber documents have one; no other application's ORA, KRA or
PSD states such a thing. So every import here opens on **transparency**, and
that is a fact about those files rather than a default — inventing a colour
would be putting paint in a document that does not contain any. The one
exception is a file Umber itself wrote, where the background rides in as a
tagged bottom layer and is turned back into the document property.

**The resolution.** ORA states it in `xres`, Krita in `x-res`, and both are
read. Photoshop and PNG both *can* carry one and neither is read yet; those
documents open at Umber's default of 72 dpi, and this is deliberately **not**
an `ImportWarning`. Resolution changes no pixel — the picture is identical
either way — and a line on every PSD and PNG would be noise in the one list that
has to stay worth reading. It is visible and editable in the canvas settings
dialog, which is where somebody who cares will look.

## OpenRaster (`.ora`) — landed, exact

A ZIP of PNGs plus a `stack.xml`, fully specified at
[openraster.org](https://www.openraster.org/baseline/file-layout-spec.html) and
[the layer stack spec](https://www.openraster.org/baseline/layer-stack-spec.html).
It maps onto Umber almost one to one and is the recommended way in from any
program this document declines — Krita, GIMP, MyPaint, Drawpile and Pinta all
export it.

What arrives: canvas size, resolution (`xres`), per-layer name, `src` image,
`x`/`y` offset, opacity, visibility, and `composite-op` — plus, for a file Umber
wrote, the document background, which is a tagged bottom layer here and a
document property inside. See `document-format.md`.

What is lost, with a warning each:

- **Nested stacks.** Umber has no groups, so the tree is flattened. A group's
  visibility propagates to its children (exactly right) and its opacity is
  multiplied into theirs (right only where the children do not overlap — hence
  `GroupOpacityFolded`).
- **Blend modes Umber lacks.** See the table below.

Verified against MyPaint's own test files (`smallimage.ora`, `bigimage.ora`,
`fill_outlines.ora`): 16-layer documents, nested hidden groups, fractional
opacities, layers offset within a 3520×2688 canvas, and MyPaint's non-standard
`mypaint:spectral-wgm` composite-op, which is reported as unsupported rather
than mistaken for something else.

Ordering is the trap. **"The first element in a stack is the uppermost"**, and
`LayerStack` is bottom first, so the list is reversed. Getting that backwards
inverts the entire document and is invisible on a symmetrical test image.

## Krita (`.kra`) — landed, layer-aware

Also a ZIP, but Krita stores its own tile format rather than PNGs:
`<document>/layers/layerN` holds a five-line text header followed by tiles —
64×64 by default — each with a `left,top,LZF,size` header of its own and a
payload that begins with a flag byte saying whether the rest is LZF-compressed.
Inside a tile the pixels are **planar and blue first**: all the blue samples,
then green, then red, then alpha.

Sources: [the KDE wiki's tile data
format](https://community.kde.org/Krita/Tile_Data_Format), the [godot-kra-psd-importer
notes](https://github.com/2shady4u/godot-kra-psd-importer/blob/master/docs/KRA_FORMAT.md),
and the `krita` crate's `paint_layer.rs`, which was read as an independent check
on the tile header — in particular that the declared size counts the flag byte.

LZF is implemented here (`docimport::lzf`, about forty lines) rather than taken
as a dependency. The only crate on offer has not been touched since 2015 and
panics on malformed input; LZF is a fixed published format with no upstream to
track, so owning it costs nothing and lets a corrupt tile return an error.

Krita's layer list is uppermost first, like ORA — its own documentation example
ends with `Background` — so it is reversed too. That reversal is also what makes
groups free: a `<layer nodetype="grouplayer">` becomes a **folder**, and
reversing an uppermost-first list puts a group after its own contents, which is
exactly where a `LayerStack` keeps one. A group's *opacity* still folds into its
children, because a folder at 50% over two overlapping children is not two
children at 50% each and Umber's folders carry none; its *eye* does not, because
it lives on the folder and `LayerStack::effective_visible` walks the ancestors.

**Where it refuses:** a document whose colour space is not 8-bit `RGBA`
(`RGBA16`, `RGBAF32`, `GRAYA`, `CMYKA`, `LABA`) has the same tile *layout* with
different bytes in it, so reading it as 8-bit would produce a plausible image
made of the wrong halves of every sample. Those documents import as the
`mergedimage.png` every `.kra` carries, as one flat layer, with a warning saying
so. Vector, filter, clone and file layers are skipped the same way — reported,
never silently missing.

This reader is a convenience rather than a necessity, since Krita exports ORA,
which is why it is happy to decline.

## Photoshop (`.psd`) — landed, lossy

Read through the [`psd` crate](https://crates.io/crates/psd) (0.3.5, MIT/Apache,
one dependency). It handles the parts that are genuinely tedious — PackBits per
scanline, additional-info blocks, Unicode names, section dividers — and returns
a canvas-sized RGBA buffer per layer, which is exactly Umber's layer shape.

It was assessed honestly before being adopted, by running it over its own test
fixtures, which are real Photoshop files. It has three behaviours that would
each silently produce a wrong document, and all three are compensated for in
`docimport::photoshop` with the evidence written at the call site:

- **`layers()` is ordered top first**, though its own doc comment says index 0
  is the bottom layer. In `transparent-top-layer-2x1.psd`, whose top layer the
  fixture README describes as blue, `layers()[0]` is "Blue Layer". Trusting the
  comment inverts every import.
- **`visible()` returns PSD flag bit 1, which is set when a layer is *hidden*.**
  Every layer of every fixture — all ordinary visible layers — reports
  `visible() == false`.
- **`is_clipping_mask()` is really "is a clipping base".** In
  `green-clipping-10x10.psd` it is true for the base and false for the two
  layers clipped to it.

It also panics rather than erroring on real files: ZIP-compressed channel data
is an `unimplemented!()`, the major-section split slices without bounds checks,
and `negative-top-left-layer.psd` — a file the crate ships itself — panics inside
`rgba()`. Parsing therefore runs inside `catch_unwind`, per layer as well as per
file, so a bad layer is reported and skipped and a bad file refuses to open
instead of taking the application with it.

**Only 8-bit RGB is accepted.** The crate reads channel bytes without consulting
the file's depth or colour mode, so a 16-bit or CMYK document either comes back
with no layers at all — the deep-colour layer records live in an `Lr16`/`Lr32`
block it does not read — or with channels reinterpreted as something they are
not. Both are refused with a message naming the reason.

Lost, each with a warning: layer groups (flattened, with visibility propagated
and opacity folded), layer masks, clipping, adjustment and text layers as live
objects (they arrive rasterised if the file carries pixels for them, and are
skipped if it does not), layer effects, and the blend modes below.

### Why not write our own reader, or use `ag-psd`?

Writing one was weighed seriously — the fixture builder in `fixtures.rs` writes
a valid layer record, so the shape of the work is known. The tedious parts are
not the record but everything around it, and a narrower reader would start with
the same three questions this crate already answers correctly (ordering,
flags, channel compression) and no fixtures to answer them against.

`ag-psd` 0.1.0 is a much more capable port of the well-tested TypeScript library
of the same name: 16-bit, PSB, masks, effects. It was rejected for now because
it is a single 0.1.0 release, self-described as "vibe-coded" and not
line-audited, and because it still cannot read `Lr16`/`Lr32` nested layers —
so the 16-bit case, its main advantage, is exactly the one it does not solve.
Worth revisiting when it has a track record.

## Clip Studio Paint (`.clip`) — landed, layer-aware

This was a declined format and is not any more. The reason it was declined —
"an undocumented proprietary schema inside SQLite; a research project, not a
feature" — was right about the schema and wrong about the cost, because by the
time it was re-opened Umber already had both halves of the machinery, each built
for the brush importer:

- `umber-core::sqlite` is a read-only SQLite reader, written so that a `.sut`
  brush could be imported without putting a C toolchain in every CI job and
  every cross-compile. A `.clip` is the same database with a different schema.
- `umber-core::csblocks` is the 256-square zlib block stream a Clip Studio
  *material* stores its pixels in. A document layer's pixels are in the same
  stream, so it moved out of `brushimport::csmaterial` and both read it.

What was actually new is the container, the table walk and the tree.

### The container

`CSFCHUNK`, a 24-byte file header, then chunks of `[8-byte tag][u64 be size]
[payload]`: `CHNKHead` (forty bytes nothing here reads), one `CHNKExta` per
stored bitmap, one `CHNKSQLi` holding the whole database, and an empty
`CHNKFoot`. An `Exta` payload is `[u64 be name length][name][u64 be data
length][block data]` and the name is the forty-character `extrnlid…` an
`Offscreen` row points at.

### Finding a layer's pixels

Four tables and three hops, none of which can be short-circuited:

```
Layer.LayerRenderMipmap -> Mipmap.BaseMipmapInfo -> MipmapInfo.Offscreen -> Offscreen
```

and the `Offscreen` row holds an `Attribute` blob describing the bitmap and a
`BlockData` naming the external chunk. A layer's **mask** is the identical chain
from `LayerLayerMaskMipmap`. The other `MipmapInfo` rows are the 50%, 25% and
smaller mipmap levels and are deliberately not followed.

Columns are looked up **by name**. The `Layer` table's schema is not fixed: the
Clip Studio version that wrote the sample files has no `LayerEffectInfo` and no
`OutputAttribute`, both of which newer versions do.

### The stack order, and how it was settled

`Canvas.CanvasRootFolder` names a folder layer; `LayerFirstChildIndex` names its
first child and `LayerNextIndex` walks to the next. **The chain runs bottom to
top**, which happens to be Umber's own order, and a reader that gets this
backwards still produces a picture — so it was established from files rather
than assumed:

- The root chain of a fresh Clip Studio document begins at "Layer 1", the layer
  a new document is created with and the one everything else is added *above*.
- Inside a folder of layers made one after another, the chain visits them in
  ascending `MainId`, which is creation order, which in Clip Studio is bottom
  upwards.

A folder is emitted **after** its own contents, which is where a `LayerStack`
keeps one.

### What it loses, and what each loss says

| Lost | Reported as | Why |
|---|---|---|
| A correction layer (brightness, tone curve, level correction, gradient map) | `LayerSkipped` | It is an operation on the layers below rather than a picture. Its `Offscreen` exists and holds a stated fill, so importing it would put a flat sheet over the drawing. |
| A layer that was not made of pixels (text, vector, frame border, 3D) | `LayerRasterised` | It arrives as the pixels Clip Studio rendered for it — which is what Clip Studio's own PSD export does — and can no longer be edited as what it was. |
| A bitmap whose absent blocks carry a stated colour fill | `LayerSkipped` | `InitColor` is readable as a *flag* and as one channel, which is all a mask needs; a colour fill is four more values nothing has checked against a file that paints with one. |
| A bitmap packed some other way (1-bit, 16-bit) | `LayerSkipped` | One alpha plane then four interleaved bytes is colour, one is greyscale, none is a mask. Anything else would be sliced by a byte count that does not describe it. |
| A blend mode Umber lacks | `BlendApproximated` / `BlendDropped` | The same table every other reader uses. |
| Animation, rulers, tones, the comic-page furniture | nothing | Not read at all. A `.clip` opens as its layers. |

An **alpha lock** is deliberately not reported, for the reason a resolution is
not: it changes no pixel. A full lock does come across, as does clipping, a
folder's eye, an opacity out of 256, and a layer mask.

### What was and was not checked against a real file

Checked, against five real `.clip` documents: the container, the schema, the
four-table chain, the tree and its direction, the `Attribute` layout including
the section lengths that are the only way to reach `InitColor`, the twenty-eight
blend-mode numbers, and the two `InitColor` shapes — a raster layer states no
fill and a mask states all-ones, which is the "reveal everything" a Clip Studio
mask starts as.

**Not** checked against one, and said out loud rather than implied away:

- **The pixel bytes.** Every layer of every obtainable sample is empty. The
  `[alpha plane][B G R X interleaved]` reading rests on two independent sources
  instead: `csmaterial`'s measurement of the same block format in real Clip
  Studio *materials*, where the five-channel shape it refuses is exactly this
  one, and `clip_to_psd`, whose output people use.
- **The placement of a bitmap smaller than the canvas.** Every sample's bitmap
  is canvas-sized at offset zero, so `LayerOffsetX + LayerRenderOffscrOffsetX`
  is taken on `clip_to_psd`'s word. It is where to look first if an imported
  layer lands in the wrong place, and a bitmap that misses the canvas entirely
  is refused rather than imported blank.
- **The greyscale packing.** One alpha plane and one interleaved byte is read as
  a grey; no sample carries pixels in that shape.

## Blend modes

Umber has five: Normal, Multiply, Screen, Overlay, Add. Photoshop has
twenty-seven and Krita rather more. `docimport::blend` maps them all in one
table and returns a fidelity alongside the mode:

| Source | Umber | Fidelity |
|---|---|---|
| `src-over` / normal | Normal | exact |
| `multiply` | Multiply | exact |
| `screen` | Screen | exact |
| `overlay` | Overlay | exact |
| `plus`, `linear-dodge`, Krita `add` | Add | approximate — identical where both layers are opaque |
| `darken`, `color-burn`, `linear-burn` | Multiply | approximate |
| `lighten`, `color-dodge` | Screen | approximate |
| `hard-light`, `soft-light`, `vivid-light`, `linear-light`, `pin-light` | Overlay | approximate |
| everything else | Normal | dropped |

Difference, exclusion, hue, saturation, colour, luminosity, divide, subtract and
the Porter-Duff alpha operators fall in the last row. Choosing a "close" mode
for them would be a worse lie than Normal, so they are reported as dropped.

## Declined formats

### Procreate (`.procreate`)

A ZIP containing a binary property list and LZ4-compressed tile chunks. The
format is undocumented and versioned with the application; community readers
exist but disagree, and the chunk-to-layer mapping and orientation handling are
the sort of thing that is right for one file and wrong for the next. Procreate
exports PSD and PNG.

### MediBang Paint (`.mdp`) — not landed, and not for want of a specification

This section used to say the format was "proprietary and barely documented — no
specification, no maintained reader, no fixtures to test against". Every clause
of that turned out to be false, and the correct reason for not shipping it is
much narrower. What follows is the research, so the next attempt starts here
rather than at the beginning.

`.mdp` is **MDIPACK**, the format `mdiapp+`, FireAlpaca, MediBang Paint and
LayerPaint HD all write. There is a specification by the nattou.org group who
wrote both the format and most of the applications using it, mirrored as
[`extras/mdp_format_wiki.md`](https://github.com/weeb-poly/krita-plugin-mdp/blob/main/extras/mdp_format_wiki.md)
in `weeb-poly/krita-plugin-mdp`, which is a working Python reader; the same
author's `gimp-file-mdp-plugin` is a second one. That repository also ships
three real `.mdp` sample files, which is more than was available for `.clip`.

Everything below was verified by decoding those three files.

**The container.** `mdipack\0` (8 bytes), then `[u32 le version][u32 le MDI
size][u32 le MDIBIN size]`, then the two sections back to back — the whole file
is exactly `20 + mdi + mdibin` bytes.

**MDI** is UTF-8 XML and is completely legible:

```xml
<Mdiapp width="370" height="320" dpi="72" checkerBG="true"
        bgColorR="255" bgColorG="255" bgColorB="255">
  <Thumb width="256" height="221" bin="thumb" />
  <Layers active="1">
    <Layer ofsx="0" ofsy="0" width="370" height="320" mode="normal" alpha="255"
           visible="false" protectAlpha="false" locked="false" clipping="false"
           masking="false" maskingType="0" id="1" parentId="-1" name="…"
           binType="2" bin="layer0img" type="32bpp" />
  </Layers>
</Mdiapp>
```

`parentId` is the folder tree. `type` is `32bpp`, `8bpp` or `1bpp`; the last two
carry a `color="AARRGGBB"` attribute and their samples are an **alpha channel**
painted with that colour, not a greyscale image.

**MDIBIN** is a run of 132-byte `PAC ` headers, each followed by its stream:
`[4s "PAC "][u32 chunkSize][u32 streamType][u32 streamSize][u32 archiveSize]
[48 reserved][64-byte name]`, little-endian, `streamType` 0 raw and 1 zlib. The
name is what a `<Layer bin="…">` or `<Thumb bin="…">` points at.

**An archive is tiles.** `[u32 tileNum]`, then — only when there is at least one
— `[u32 tileDim]` and one record per tile: `[u32 col][u32 row][u32 ctype]
[u32 size][payload]`, padded to a 4-byte boundary. `tileDim` is 128 in all three
samples. `ctype` is 0 for zlib, 1 for Snappy and 2 for FastLZ; **all three
samples use zlib only**.

**32bpp samples are BGRA with straight alpha.** The BGRA reading is the Krita
plugin's, whose comment says so out loud ("For some reason, this reads in BGRA
data"); straight alpha is visible in the samples, where pixels with `a = 0` keep
their colour.

**What is missing is one bit of information and two codecs.**

1. **The stack direction.** The three samples cannot settle it: one has a single
   layer, and in the other two the visible layers do not overlap, so
   compositing them in either order and comparing against the file's own
   thumbnail gives the same answer to five decimal places. The circumstantial
   evidence points at *first element = bottom* — in the three-layer sample the
   ids ascend with the XML and the first is a white matte, which is a bottom
   layer — but the Krita plugin adds nodes in a way that reads as the opposite,
   and the wiki does not say. **A reader that gets this wrong inverts every
   multi-layer document silently**, so it is not something to ship on a
   three-to-one prior. Settling it takes one `.mdp` with two overlapping visible
   layers and its own embedded thumbnail, which is thirty seconds for anybody
   with MediBang installed.
2. **Snappy**, for `ctype == 1`. About a hundred and twenty lines, or a
   dependency.
3. **FastLZ**, for `ctype == 2`. Level 1 of FastLZ is byte-compatible with LZF,
   which `docimport::lzf` already decodes for Krita; level 2 is not, and which
   level MediBang writes has not been established. A tile in a codec the reader
   does not have is a named loss rather than a wrong picture, so this could ship
   incomplete.

So the honest verdict is **not yet, and cheaply**: perhaps a day's work once
somebody has produced the one file that settles the direction. Until then
MediBang exports PSD and PNG, which Umber reads.

### GIMP (`.xcf`)

The one declined format that is genuinely documented (`docs/xcf.txt` in GIMP's
source). It is declined on size rather than on principle: XCF carries several
precisions (8/16/32-bit integer and float, each in linear or perceptual
encoding), its own tile and RLE scheme, layer masks, channels, paths and parasites,
and the colour-model question alone — which encoding a given file's samples are
in — is exactly the kind of subtlety this module refuses to guess at.

GIMP exports OpenRaster natively, which is exact. If XCF is wanted later, the
work is a week and a pile of fixtures, not an afternoon.

### Layered TIFF

Photoshop writes its layers into a private TIFF tag as a PSD-format blob, so
"layered TIFF support" is either PSD support again or nothing. A plain
multi-page TIFF has pages, which are not layers and have no blend modes or
opacities. Neither is worth a TIFF decoder.

### JPEG

Not layered, so it would be the flat single-layer path — cheap, except that
photographs from phones carry an EXIF orientation tag, and a decoder that
ignores it imports them sideways. That is precisely the silent wrongness this
module exists to avoid, and doing it properly means an EXIF parser. Small,
well-understood, and not done yet.

## Dependencies added

All four are pure Rust with no C build step, which keeps `cargo test -p
umber-core` instant and keeps an Android or iOS cross-build plausible. **Clip
Studio added none**: its SQLite reader and its block decoder were already in the
crate, written for the brush importer, and `flate2` was already a dependency.

| Crate | Why |
|---|---|
| `zip` 8 | ORA and KRA containers. `default-features = false`, `features = ["deflate-flate2"]` — the default pulls bzip2, zstd, lzma, ppmd, AES and zopfli, none of which either format uses. |
| `png` 0.18 | ORA layer images, KRA and ORA composites, and the flat PNG import. Encodes as well as decodes, so it also builds the test fixtures. |
| `quick-xml` 0.41 | `stack.xml` and `maindoc.xml`. |
| `psd` 0.3.5 | Photoshop. Its only dependency is `thiserror`. |

LZF is implemented in the module rather than taken from a crate; see the Krita
section.

## Testing

`cargo test -p umber-core` covers the lot in about a second: colour conversion,
LZF, canvas placement, blend mapping, and a whole-file import for each format.

Fixtures are **generated in memory**, not committed. A repository of binary
sample documents rots — nobody can review a diff to a `.psd`, nobody remembers
which application wrote it, and it is dead weight in every clone forever.
Writing the bytes in Rust means the fixture is readable and the test states out
loud what it believes the format to be.

The honest limitation of that approach is that a generated fixture tests the
reader against *this repository's* understanding of a format, not against
Photoshop. It is mitigated two ways:

- The PSD fixture builder was **calibrated against real Photoshop files** — the
  `psd` crate's own test corpus — before it was written: the flag bit that means
  "hidden", the clipping byte and the bottom-to-top record order were all read
  back out of those files first.
- The ORA and PSD readers were run over real third-party files during
  development (MyPaint's test `.ora`s; the `psd` crate's fixtures, including
  RLE-compressed layers, nested groups, Unicode names, out-of-bounds layers and
  the file that panics). Those files are not in this repository, so those runs
  are not tests — but the behaviours they exposed are, and every compensation
  in `photoshop.rs` cites the file that revealed the need for it.
- The Clip Studio reader was run over five real `.clip` documents the same way,
  and the Clip Studio section above says exactly which of its readings that did
  and did not settle. The one it settled that no fixture could is the **stack
  direction**, which is the reading a `.clip` reader is most likely to get
  backwards and which a self-written fixture would have agreed with either way.

The tests were also mutation-checked: inverting the visibility compensation,
removing either stack reversal, premultiplying in the wrong colour space and
transposing the Krita tile planes each make the suite fail.

## Interface

```rust
pub fn import(path: &Path) -> Result<ImportedDocument, ImportError>;
pub fn read_openraster(bytes: &[u8]) -> Result<ImportedDocument, ImportError>;
pub fn supported_extensions() -> &'static [&'static str];  // ["ora", "kra", "psd", "png"]

pub struct ImportedDocument {
    pub format: SourceFormat,
    pub size: UVec2,
    pub layers: Vec<ImportedLayer>,   // bottom first, like LayerStack
    pub active: Option<usize>,        // only Umber's own files say; None means the top
    pub warnings: Vec<ImportWarning>, // empty means nothing was lost
}

impl ImportedDocument {
    pub fn document(&self) -> Document;
    pub fn into_stack(self) -> (Document, LayerStack, Vec<LayerUpload>);
}
```

`into_stack` builds the engine-side state; each `LayerUpload` carries a slot and
a canvas-sized RGBA8 buffer ready for `queue.write_texture`. `ImportWarning`
implements `Display` in finished prose, so the UI can list warnings without
composing sentences of its own.

Imports are bounded: at most `LayerStack::MAX` layers, 16384 px per edge, 2 GiB
of layer data. `umber-core` cannot see the GPU, so the caller must still check
the adapter's real `max_texture_dimension_2d` before uploading.

## Known gaps

- No import writes into an *existing* document — an import replaces the
  document. Placing a file as a new layer in the current document is a separate
  feature.
- **Photoshop groups still flatten**, and are the last reader that does.
  ORA, `.kra` and `.clip` all produce folders. The `psd` crate exposes
  `groups()`, `group_ids_in_order()` and a `parent_id` on both layers and
  groups, so the tree is reachable; what is not directly given is where a
  group's own divider sits among its siblings, which has to be reconstructed
  from the file positions of its descendants. That is the whole of the work.
- Krita's `.krz` (the same container with different compression) is untested and
  not claimed.
- Photoshop layer masks are detected and reported but not applied, and with the
  pinned `psd` crate they *cannot* be: `read_layer_record` reads the length of
  the layer-mask block and skips it, so the mask's own rectangle — which is
  where its pixels live, not the layer's — never leaves the parser, the channel
  bytes sit behind a private accessor, and an RLE mask channel panics. Applying
  a mask would be a multiply into alpha; getting one to multiply is a second
  parser walking the same bytes, which is the fork this module declines.
  Density and feather would still have to be right afterwards.

  **That verdict was re-opened and is now narrower.** "A second parser walking
  the same bytes" was written about a *whole* PSD reader, and that is the right
  thing to refuse: PackBits per scanline, additional-info blocks, Unicode names,
  section dividers, colour modes and depths. What a mask actually needs is much
  smaller — the layer records section is a length-prefixed sequence, and inside
  each record the layer-mask block is itself length-prefixed and holds a
  rectangle, a default colour and two flag bytes and nothing else. Reading only
  that is perhaps two hundred lines, and it does not have to agree with the
  crate about anything except where each layer record begins.

  It is still not free, and the cost is in three places rather than one. The
  **channel data** would have to be walked as well, because a mask's samples are
  a fifth channel with its own compression, and that is where the crate panics
  today — so the fork would have to read the channels too, at which point it is
  most of a layer reader. The **fifth channel's length** is what the crate gets
  wrong (it assumes every channel is the layer's height), so the two parsers
  would disagree about where a record ends, which is exactly the drift the
  original refusal names. And **an RLE mask channel currently refuses the whole
  file**, which is the loss that costs a user something today and would be fixed
  by the same work.

  So the honest ranking is: the file that is *refused* is a worse failure than
  the mask that is *dropped*, and both are fixed by the same fork. If Photoshop
  masks are wanted, the shape to build is a small parser that walks the layer
  records section for the mask block **and** the channel lengths, keeps the
  crate for the pixels, and compares its own record boundaries against the
  crate's on every layer — refusing the mask, not the document, wherever the two
  disagree. `ag-psd` is the reference implementation to check against; see
  "Why not write our own reader" above for why replacing the crate outright is
  still the wrong trade.
