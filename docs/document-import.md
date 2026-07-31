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
groups, masks, blend modes Umber lacks — it is imported and the loss is
reported in `ImportedDocument::warnings`, which the UI shows.

## Verdicts

| Format | Verdict | Notes |
|---|---|---|
| `.ora` OpenRaster | **Landed, exact** | Open specification; layers, offsets, opacity, visibility and blend modes all arrive. |
| `.kra` Krita | **Landed, layer-aware** | 8-bit RGBA documents read tile by tile. Anything else falls back to the embedded composite, with a warning. |
| `.psd` Photoshop | **Landed, lossy** | 8-bit RGB only. Groups flatten, masks and clipping are dropped, and every loss is reported. |
| `.png` | **Landed, exact** | A single layer. Also the decoder ORA uses. |
| `.psb` Photoshop large | Declined | Header version 2; the `psd` crate reads version 1 only. |
| `.clip` Clip Studio | Declined | Undocumented proprietary schema inside SQLite. Research project, not a feature. |
| `.procreate` | Declined | Undocumented LZ4 tile layout behind a binary plist. |
| `.mdp` MediBang | Declined | Proprietary, effectively undocumented. |
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

## OpenRaster (`.ora`) — landed, exact

A ZIP of PNGs plus a `stack.xml`, fully specified at
[openraster.org](https://www.openraster.org/baseline/file-layout-spec.html) and
[the layer stack spec](https://www.openraster.org/baseline/layer-stack-spec.html).
It maps onto Umber almost one to one and is the recommended way in from any
program this document declines — Krita, GIMP, MyPaint, Drawpile and Pinta all
export it.

What arrives: canvas size, per-layer name, `src` image, `x`/`y` offset, opacity,
visibility, and `composite-op`.

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
ends with `Background` — so it is reversed too.

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

### Clip Studio Paint (`.clip`)

A `.clip` is a chunked container — `CSFCHUNK`, `CHNKHead`, one or more
`CHNKExta`, `CHNKSQLi`, `CHNKFoot` — whose `CHNKSQLi` chunk is an embedded
SQLite database and whose `CHNKExta` chunks hold zlib-compressed image data.
The community reverse-engineering that exists ([Inochi2D/clip-d's
SPEC.md](https://github.com/Inochi2D/clip-d/blob/main/SPEC.md), `clipthumb`)
covers the container: enough to find the database and extract the embedded
thumbnail, and explicitly not enough to describe the layer bitmap encoding —
the tile/block structure, the mipmap and offscreen tables, and how a layer's
external chunk ids relate to its pixels are all still "don't know yet".

So the choice was between three things:

1. A **thumbnail import**, which would open a `.clip` and show a low-resolution
   preview of it. That is a lie dressed as a feature.
2. A **layer-bitmap import**, which is the research project: reverse-engineering
   an undocumented proprietary tile format with no reference implementation to
   check against, on a format the vendor changes at will.
3. **Decline**, and tell the user that Clip Studio exports PSD.

Option 3. It also avoids `rusqlite`, which is a C build in every CI job and every
future mobile cross-compile, bought for a feature that would not work.

Clip Studio Paint exports layered PSD (File → Export → .psd), which Umber reads.
That is the supported route, and it is one menu item away.

### Procreate (`.procreate`)

A ZIP containing a binary property list and LZ4-compressed tile chunks. The
format is undocumented and versioned with the application; community readers
exist but disagree, and the chunk-to-layer mapping and orientation handling are
the sort of thing that is right for one file and wrong for the next. Procreate
exports PSD and PNG.

### MediBang Paint (`.mdp`)

Proprietary and barely documented — no specification, no maintained reader, no
fixtures to test against. There is nothing here that could be shipped honestly.
MediBang exports PSD and PNG.

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
umber-core` instant and keeps an Android or iOS cross-build plausible.

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

The tests were also mutation-checked: inverting the visibility compensation,
removing either stack reversal, premultiplying in the wrong colour space and
transposing the Krita tile planes each make the suite fail.

## Interface

```rust
pub fn import(path: &Path) -> Result<ImportedDocument, ImportError>;
pub fn supported_extensions() -> &'static [&'static str];  // ["ora", "kra", "psd", "png"]

pub struct ImportedDocument {
    pub format: SourceFormat,
    pub size: UVec2,
    pub layers: Vec<ImportedLayer>,   // bottom first, like LayerStack
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
- Layer groups flatten. Whenever Umber grows real groups, every reader here has
  the tree available and can stop flattening.
- Krita's `.krz` (the same container with different compression) is untested and
  not claimed.
- Photoshop layer masks are detected and reported but not applied. Applying one
  is straightforward — multiply it into alpha — but the mask's own rectangle,
  density and feather all have to be right first.
