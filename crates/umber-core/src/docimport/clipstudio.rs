//! Clip Studio Paint (`.clip`).
//!
//! A `.clip` is a chunk stream wrapped round a **SQLite database**, and Umber
//! already had both halves of what it takes to read one: [`crate::sqlite`],
//! written so a `.sut` brush could be imported without a C toolchain in the
//! build, and [`crate::csblocks`], which is the block stream a brush material's
//! pixels live in and — it turns out — a document layer's as well.
//!
//! # The container
//!
//! `CSFCHUNK`, then a 24-byte file header, then chunks of
//! `[8-byte tag][u64 be size][payload]`. The tags are `CHNKHead` (forty bytes
//! nothing here reads), one `CHNKExta` per stored bitmap, one `CHNKSQLi`
//! holding the whole database, and an empty `CHNKFoot`. An `Exta` payload is
//! `[u64 be name length][name][u64 be data length][block data]`, and the name
//! is the forty-character `extrnlid…` an `Offscreen` row points at.
//!
//! # Finding a layer's pixels
//!
//! Four tables and three hops, none of which can be short-circuited:
//! `Layer.LayerRenderMipmap` names a `Mipmap`, whose `BaseMipmapInfo` names a
//! `MipmapInfo`, whose `Offscreen` names an `Offscreen` row — and *that* row
//! holds the `Attribute` blob describing the bitmap and a `BlockData` naming
//! the external chunk holding it. A layer's **mask** is the identical chain
//! from `LayerLayerMaskMipmap`. The other `MipmapInfo` rows are the mipmap
//! levels, at 50%, 25% and below, and are deliberately not followed: the base
//! one is the picture.
//!
//! Columns are looked up **by name**, never by position. The `Layer` table's
//! schema is not fixed — the version that wrote the sample files has no
//! `LayerEffectInfo` and no `OutputAttribute`, both of which newer ones do —
//! and `crate::sqlite::Table::column` exists for exactly this.
//!
//! # Where the stack order came from
//!
//! `Canvas.CanvasRootFolder` names a folder layer; its `LayerFirstChildIndex`
//! names its first child and every `LayerNextIndex` walks to the next. **The
//! chain runs bottom to top**, which is Umber's own order and was established
//! from the files rather than assumed: the root chain of a fresh Clip Studio
//! document begins at "Layer 1", the layer the document is created with and the
//! one everything else is added *above*; and inside a folder of three layers
//! made one after another the chain visits them in ascending `MainId`, which is
//! creation order, which in Clip Studio is bottom upwards. A folder is emitted
//! **after** its contents, because that is where Umber's `LayerStack` keeps one.
//!
//! # What does not come across, and why each is named
//!
//! - **A correction layer** — brightness, tone curve, gradient map, level
//!   correction — is an operation on everything below it rather than a picture,
//!   and Umber has no such layer. Its own `Offscreen` exists and holds nothing
//!   but a stated fill, so importing it would put a flat white sheet over the
//!   drawing. Refused and named.
//! - **A layer that is not made of pixels** — text, vector, a frame border, a
//!   3D object — arrives as the pixels Clip Studio rendered for it and is named
//!   as rasterised, because it cannot be edited as what it was any more. That
//!   is what Clip Studio's own PSD export does with them.
//! - **A folder's opacity, blend mode and mask.** Umber's folders are
//!   pass-through and carry none of the three, and unlike ORA and Krita there
//!   is nothing to fold an opacity into: the contents are already built by the
//!   time the folder is reached. All three are reported.
//! - **A mask Clip Studio has switched off** is not applied — it bounds nothing
//!   there either — and is named, as `MaskUnsupported` rather than
//!   `MaskIgnored`, because the picture is right without it.
//! - **A layer whose bitmap is filled rather than empty** is refused. The
//!   `InitColor` section states what an absent block holds and a mask's is
//!   readable — one channel, and `255` is the "reveal everything" a mask starts
//!   as — but a *colour* fill is four more values whose meaning has never been
//!   checked against a file that paints with one. See [`crate::csblocks::Fill`].
//! - **A bitmap Clip Studio packed some other way.** One alpha plane followed
//!   by four interleaved bytes is colour, by one is greyscale, by none is a
//!   mask. A 1-bit or 16-bit shape is refused rather than sliced by a byte
//!   count that does not describe it.
//! - **Animation, rulers, tones and everything else hanging off the document**
//!   is not read at all. A `.clip` that is a comic page opens as its layers.
//!
//! An **alpha lock** is deliberately *not* reported, which is the rule
//! `ImportedDocument::dpi` already follows: it changes no pixel, the picture is
//! identical either way, and a line on every layer of every import is the noise
//! that stops the list being read. A full lock does come across.
//!
//! # What has and has not been checked against a real file
//!
//! The container, the schema, the four-table chain, the tree and its direction,
//! the `Attribute` layout including its section lengths, the blend-mode
//! numbering and the `InitColor` shapes were all read out of real `.clip` files.
//! **The pixel bytes were not**, because every layer of every sample obtainable
//! is empty — so the `[alpha][BGRX]` reading rests on two independent sources:
//! `csmaterial`'s measurement of the same block format in real Clip Studio
//! *materials*, where the five-channel shape it refuses is exactly this one, and
//! `clip_to_psd`, whose output people use. **The placement of a bitmap smaller
//! than the canvas is the one thing here taken on somebody else's word alone**
//! — which three columns are summed, and in what order — and is where to look
//! first if an imported layer lands in the wrong place. The *arithmetic* around
//! that placement is exercised: a bitmap smaller than the canvas, at an offset
//! split across all three column pairs, with its block padding kept off the
//! picture.

use std::collections::HashMap;

use glam::UVec2;

use super::blend::{self, Fidelity};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, SourceFormat, StackSize,
    check_bounds, srgb,
};
use crate::color::Color;
use crate::csblocks::{self, BLOCK, Bitmap, Fill, Packing};
use crate::document::Background;
use crate::layer::LayerStack;
use crate::sqlite::{Database, Table, Value};

const FORMAT: SourceFormat = SourceFormat::ClipStudio;

/// `CSFCHUNK`, then the file's own length and the offset the chunks start at.
const FILE_MAGIC: &[u8; 8] = b"CSFCHUNK";
const FILE_HEADER: usize = 24;

/// Clip Studio states an opacity out of this rather than out of 255.
const OPACITY_FULL: f32 = 256.0;

/// `LayerFolder`'s bit 0. Bit 4 is "collapsed", which Umber does not record —
/// see the folders rules in `CLAUDE.md`: a fold that survived a save would be a
/// state somebody had to undo before they could see their own painting.
const FOLDER: i64 = 1;

/// `LayerType`'s bits. `PIXEL` says the layer is made of pixels; `CORRECTION`
/// says it is an operation on the layers below.
const LAYER_IS_PIXEL: i64 = 1;
const LAYER_IS_CORRECTION: i64 = 4096;

/// `LayerVisibility`'s bit 0 is the layer's own eye and bit 1 is its **mask's**.
/// Bit 2 is the ruler's, which Umber has nothing to do with.
const VISIBLE: i64 = 1;
const MASK_VISIBLE: i64 = 2;

/// `LayerLock`'s bit 0 — the whole layer. Bit 4 is the alpha lock, which is a
/// painting aid rather than a property of the picture; see the module docs.
const LOCK_ALL: i64 = 1;

pub fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let container = split(bytes)?;
    let db = Database::open(container.database).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: e.to_string(),
    })?;

    let canvas = canvas(&db)?;
    let mut warnings = Vec::new();

    let layers = Tables::read(&db)?;
    let all = layers.tree(canvas.root, &mut warnings)?;

    // **The Paper layer is the document's background, not a layer of it.** It
    // carries a flat colour and no bitmap, so it used to fall through the
    // "the file does not hold its pixels" path and be dropped — every Clip
    // Studio document opening on transparency where the artist had white paper
    // under their drawing, with a warning that read like a damaged file. Umber
    // already has the concept and `openraster` already does exactly this with
    // its own `umber-background` layer, so this is that route, not a new one.
    //
    // Taken out here rather than in `build`, because it must not reach
    // `check_bounds` either: paper holds no buffer to be charged for and
    // becomes no entry to be counted.
    let (paper, nodes): (Vec<Node>, Vec<Node>) = all
        .into_iter()
        .partition(|n| layers.rows.get(&n.id).is_some_and(LayerRow::paper));
    // `tree` is bottom first, so the first is the lowest — the one actually
    // behind the picture if a file somehow carries more than one.
    let background =
        paper
            .first()
            .and_then(|n| layers.rows.get(&n.id))
            .map_or(Background::Transparent, |row| {
                // A hidden paper is a document the artist was working on
                // transparency, which is what `Background::Transparent` means.
                if row.visible {
                    Background::opaque(row.colour())
                } else {
                    Background::Transparent
                }
            });

    // Folders are entries and hold no pixels, so they count towards the stack's
    // size and not towards its bytes. `.clip` is where that matters most: a
    // Clip Studio document is usually filed into groups, and charging each one
    // a canvas is what made a 15000×5000 file refuse itself.
    let stack = StackSize::of(nodes.iter().map(|n| n.folder));
    check_bounds(FORMAT, canvas.size.x, canvas.size.y, stack)?;

    let mut out = Vec::with_capacity(nodes.len());
    for node in &nodes {
        let Some(row) = layers.rows.get(&node.id) else {
            continue;
        };
        // A refusal has already put its own sentence in `warnings`; a folder
        // whose contents were all refused is kept, empty, because it is where
        // the artist put things and an empty row says so.
        if let Some(layer) = build(row, node, &canvas, &layers, &container, &mut warnings) {
            out.push(layer);
        }
    }

    if out.iter().all(|l| l.folder) {
        return Err(ImportError::Empty { format: FORMAT });
    }

    Ok(ImportedDocument {
        format: FORMAT,
        size: canvas.size,
        layers: out,
        active: None,
        background,
        dpi: canvas.dpi,
        history: None,
        warnings,
    })
}

// ---------------------------------------------------------------------------
// The container
// ---------------------------------------------------------------------------

struct Container<'a> {
    /// Every `CHNKExta`, by the `extrnlid…` name it carries.
    external: HashMap<&'a [u8], &'a [u8]>,
    database: &'a [u8],
}

/// Walk the chunk stream.
///
/// Every length here is the file's own and every step is checked against the
/// slice, so a truncated or hostile stream ends the walk rather than reading
/// past it. The loop advances by at least the sixteen bytes of a chunk header
/// each time round, so it terminates whatever the sizes say.
fn split(bytes: &[u8]) -> Result<Container<'_>, ImportError> {
    let malformed = |detail: &str| ImportError::Malformed {
        format: FORMAT,
        detail: detail.to_string(),
    };
    if !bytes.starts_with(FILE_MAGIC) {
        return Err(malformed("it does not begin with CSFCHUNK"));
    }

    let mut external = HashMap::new();
    let mut database = None;
    let mut at = FILE_HEADER;
    while at + 16 <= bytes.len() {
        let tag = &bytes[at..at + 8];
        let size = be64(bytes, at + 8).ok_or_else(|| malformed("a chunk has no length"))?;
        let body = at + 16;
        let end = body
            .checked_add(size)
            .filter(|e| *e <= bytes.len())
            .ok_or_else(|| malformed("a chunk runs past the end of the file"))?;
        match tag {
            b"CHNKExta" => {
                if let Some((id, data)) = external_chunk(&bytes[body..end]) {
                    external.insert(id, data);
                }
            }
            b"CHNKSQLi" => database = Some(&bytes[body..end]),
            b"CHNKFoot" => break,
            _ => {}
        }
        at = end;
    }

    let database = database.ok_or_else(|| malformed("it holds no database"))?;
    Ok(Container { external, database })
}

/// `[u64 be name length][name][u64 be data length][data]`.
///
/// A chunk that does not parse is passed over rather than failing the file: it
/// costs whichever layer names it, and that layer says so.
fn external_chunk(body: &[u8]) -> Option<(&[u8], &[u8])> {
    let name_len = be64(body, 0)?;
    // The name is a fixed forty characters in every file seen, and a bound is
    // needed whatever it is.
    if name_len == 0 || name_len > 256 {
        return None;
    }
    let id = body.get(8..8usize.checked_add(name_len)?)?;
    let at = 8 + name_len;
    let data_len = be64(body, at)?;
    let start = at.checked_add(8)?;
    let data = body.get(start..start.checked_add(data_len)?)?;
    Some((id, data))
}

fn be64(bytes: &[u8], at: usize) -> Option<usize> {
    let raw = bytes
        .get(at..at.checked_add(8)?)
        .map(|b| u64::from_be_bytes(b.try_into().expect("eight bytes")))?;
    usize::try_from(raw).ok()
}

// ---------------------------------------------------------------------------
// The database
// ---------------------------------------------------------------------------

struct Canvas {
    size: UVec2,
    dpi: Option<f32>,
    root: i64,
}

/// What `Canvas.CanvasUnit` measures `CanvasWidth` and `CanvasHeight` in.
///
/// **Only the two values that were seen in real files are here, and the rest
/// are refused rather than guessed.** Clip Studio's New Document dialog also
/// offers millimetres, inches and points, so this enum is certainly incomplete;
/// what is not available is any evidence of which number means which. Reading
/// the format's own dialog order and assuming it matches the stored codes is
/// exactly the guess that keeps the MediaBang reader unwritten — a wrong unit
/// is a canvas silently out by a factor of ten or twenty-five, which is worse
/// than a refusal that names the code and can be reported.
///
/// The evidence, from 33 documents:
/// * `0` is pixels in 32 of them, at ordinary figures (5000, 3000, 1920).
/// * `1` is centimetres in one, and it is **cross-checked** rather than
///   inferred: 21×29.7 at 600 dpi comes to 4961×7016, which is precisely the
///   canvas of another file in the same folder that stored the same A4 page in
///   pixels. Two independent documents agreeing on the arithmetic is what makes
///   this a reading rather than a plausible story.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CanvasUnit {
    Pixels,
    Centimetres,
}

impl CanvasUnit {
    fn read(code: i64) -> Option<Self> {
        match code {
            0 => Some(Self::Pixels),
            1 => Some(Self::Centimetres),
            _ => None,
        }
    }

    /// How many of this unit make an inch, which is what turns a physical
    /// measurement into pixels once the resolution is known.
    fn per_inch(self) -> f64 {
        match self {
            // Never asked: the pixel arm does not go through the conversion at
            // all, because a resolution is not needed to size a canvas already
            // given in pixels and a file may legitimately state none.
            Self::Pixels => 1.0,
            Self::Centimetres => 2.54,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pixels => "pixels",
            Self::Centimetres => "centimetres",
        }
    }
}

fn canvas(db: &Database<'_>) -> Result<Canvas, ImportError> {
    let malformed = |detail: String| ImportError::Malformed {
        format: FORMAT,
        detail,
    };
    let table = table(db, "Canvas")?.ok_or_else(|| malformed("it has no Canvas table".into()))?;
    let rows = db
        .rows(&table)
        .map_err(|e| malformed(format!("its Canvas table could not be read ({e})")))?;
    // The first canvas, which is the only one a `.clip` has; a multi-page
    // Clip Studio file is a `.cmc` and is not what this reads.
    let row = rows
        .first()
        .ok_or_else(|| malformed("its Canvas table is empty".into()))?;
    let get = |name: &str| table.column(name).map(|i| row.get(i));

    let width = get("CanvasWidth").and_then(Value::as_f64).unwrap_or(0.0);
    let height = get("CanvasHeight").and_then(Value::as_f64).unwrap_or(0.0);
    let dpi = get("CanvasResolution")
        .and_then(Value::as_f64)
        .map(|v| v as f32)
        .filter(|v| *v > 0.0);

    // **`CanvasWidth` is not always pixels, and taking it for pixels is silent.**
    // `CanvasUnit` says which unit it is in, and a document authored in
    // centimetres arrived as a canvas the size of its own measurement: an A4
    // page at 600 dpi is 4961×7016 and opened as 21×29, whereupon every layer
    // missed it and the file was refused as holding no layers. Nothing about
    // that reads as a unit problem from the outside.
    let unit = get("CanvasUnit").and_then(Value::as_i64).unwrap_or(0);
    let (width, height) = match CanvasUnit::read(unit) {
        Some(CanvasUnit::Pixels) => (width, height),
        Some(unit) => {
            // Physical units need the resolution to become pixels, so a file
            // stating one without the other cannot be sized at all.
            let dpi = f64::from(dpi.ok_or_else(|| {
                malformed(format!(
                    "its canvas is measured in {} and it states no resolution",
                    unit.label()
                ))
            })?);
            (
                width * dpi / unit.per_inch(),
                height * dpi / unit.per_inch(),
            )
        }
        // **Refused rather than read as pixels**, which is the reader-wide rule
        // that subtly wrong output is worse than a refusal: falling back would
        // reproduce exactly the 21×29 canvas above, and the sentence names the
        // code so a file carrying a unit nobody here has seen can be reported.
        None => {
            return Err(ImportError::Unsupported {
                format: FORMAT,
                detail: format!("a canvas measured in unit {unit}, which Umber cannot convert"),
            });
        }
    };

    // Rounded rather than truncated, and refused rather than clamped: a
    // negative or absurd figure out of somebody else's file must not become a
    // canvas by way of an `as` cast.
    let (width, height) = (width.round(), height.round());
    if !(1.0..=f64::from(u32::MAX)).contains(&width)
        || !(1.0..=f64::from(u32::MAX)).contains(&height)
    {
        return Err(malformed(format!("its canvas is {width}×{height}")));
    }

    Ok(Canvas {
        size: UVec2::new(width as u32, height as u32),
        dpi,
        root: get("CanvasRootFolder")
            .and_then(Value::as_i64)
            .ok_or_else(|| malformed("it names no root folder".into()))?,
    })
}

fn table(db: &Database<'_>, name: &str) -> Result<Option<Table>, ImportError> {
    db.table(name).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: e.to_string(),
    })
}

/// One `Layer` row, in the fields this reader uses.
struct LayerRow {
    name: String,
    kind: i64,
    folder: i64,
    visible: bool,
    /// Whether the layer's mask is switched on. A mask that is off bounds
    /// nothing in Clip Studio either.
    mask_visible: bool,
    locked: bool,
    clipped: bool,
    opacity: f32,
    composite: i64,
    next: i64,
    first_child: i64,
    render_mipmap: i64,
    mask_mipmap: i64,
    offset: (i64, i64),
    render_offset: (i64, i64),
    mask_offset: (i64, i64),
    mask_offscreen_offset: (i64, i64),
    /// `SpecialRenderType`, whose 20 is the **Paper** layer — the flat sheet
    /// Clip Studio puts under every new document. See [`LayerRow::paper`].
    special_render: i64,
    /// `DrawColorMainRed`/`Green`/`Blue`, each `0..=u32::MAX` rather than a
    /// byte. Only meaningful on a layer that draws a flat colour.
    draw_colour: (i64, i64, i64),
}

/// `SpecialRenderType`'s value for the Paper layer.
const PAPER_RENDER: i64 = 20;

/// `LayerType`'s value for the Paper layer.
///
/// Accepted **beside** [`PAPER_RENDER`] rather than instead of it: the two
/// agreed on every one of the 33 documents this was written against, and
/// neither ever fired on anything else, so accepting either costs nothing and
/// covers a file whose schema is missing one of the columns — `int` answers 0
/// for a column that is not there, which would otherwise read as "not paper"
/// and silently drop the sheet again.
const PAPER_KIND: i64 = 1584;

impl LayerRow {
    /// Whether this is the Paper layer: a flat sheet under the whole canvas,
    /// holding a colour and no bitmap.
    ///
    /// **`DrawColorEnable` is deliberately not the test**, though it is set on
    /// every paper and reads like the obvious one. Two ordinary raster layers
    /// among the 33 documents carry it too, so keying on it would turn somebody's
    /// drawing into the document background and delete it from the stack.
    fn paper(&self) -> bool {
        self.special_render == PAPER_RENDER || self.kind == PAPER_KIND
    }

    /// The flat colour this layer draws, as straight sRGB.
    ///
    /// Clip Studio stores each channel over the whole of `u32`, so `0xFFFFFFFF`
    /// is full. Dividing by that rather than shifting keeps the ends exact —
    /// white comes back 255 and black 0.
    ///
    /// **Taking the low byte instead would be indistinguishable, and the test
    /// cannot separate the two.** Every value seen in a real file is a byte
    /// spread by `0x01010101`, whose low byte *is* that byte, so the two
    /// readings agree on everything reachable; a mutation to `v & 0xFF` passes
    /// the suite, which was checked rather than assumed. This is the principled
    /// half of the pair — it is right whether or not Clip Studio ever stores a
    /// value finer than a byte, where the mask is right only by that accident —
    /// so it is what is written, and the equivalence is recorded here rather
    /// than a test being contrived over data no file produces. All 33 sample
    /// documents are white, so nothing here is exercised by a real colour at
    /// all; the fixture is what drives it.
    fn colour(&self) -> Color {
        let channel = |v: i64| v.clamp(0, i64::from(u32::MAX)) as f64 / f64::from(u32::MAX);
        Color::from_srgb_u8(
            (channel(self.draw_colour.0) * 255.0).round() as u8,
            (channel(self.draw_colour.1) * 255.0).round() as u8,
            (channel(self.draw_colour.2) * 255.0).round() as u8,
            255,
        )
    }
}

/// Everything the database says, resolved into lookups.
struct Tables {
    rows: HashMap<i64, LayerRow>,
    /// `Mipmap.MainId` -> `BaseMipmapInfo`.
    mipmaps: HashMap<i64, i64>,
    /// `MipmapInfo.MainId` -> `Offscreen`.
    infos: HashMap<i64, i64>,
    /// `Offscreen.MainId` -> (`Attribute`, `BlockData`).
    offscreens: HashMap<i64, Offscreen>,
}

impl Tables {
    fn read(db: &Database<'_>) -> Result<Self, ImportError> {
        let malformed = |detail: String| ImportError::Malformed {
            format: FORMAT,
            detail,
        };
        let layers =
            table(db, "Layer")?.ok_or_else(|| malformed("it has no Layer table".into()))?;
        let layer_rows = db
            .rows(&layers)
            .map_err(|e| malformed(format!("its Layer table could not be read ({e})")))?;

        let mut rows = HashMap::new();
        for row in &layer_rows {
            let cell = |name: &str| layers.column(name).map(|i| row.get(i));
            let int = |name: &str| cell(name).and_then(Value::as_i64).unwrap_or(0);
            let Some(id) = cell("MainId").and_then(Value::as_i64) else {
                continue;
            };
            rows.insert(
                id,
                LayerRow {
                    name: cell("LayerName")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    kind: int("LayerType"),
                    folder: int("LayerFolder"),
                    visible: int("LayerVisibility") & VISIBLE != 0,
                    mask_visible: int("LayerVisibility") & MASK_VISIBLE != 0,
                    locked: int("LayerLock") & LOCK_ALL != 0,
                    clipped: int("LayerClip") != 0,
                    opacity: (int("LayerOpacity") as f32 / OPACITY_FULL).clamp(0.0, 1.0),
                    composite: int("LayerComposite"),
                    next: int("LayerNextIndex"),
                    first_child: int("LayerFirstChildIndex"),
                    render_mipmap: int("LayerRenderMipmap"),
                    mask_mipmap: int("LayerLayerMaskMipmap"),
                    offset: (int("LayerOffsetX"), int("LayerOffsetY")),
                    render_offset: (
                        int("LayerRenderOffscrOffsetX"),
                        int("LayerRenderOffscrOffsetY"),
                    ),
                    mask_offset: (int("LayerMaskOffsetX"), int("LayerMaskOffsetY")),
                    mask_offscreen_offset: (
                        int("LayerMaskOffscrOffsetX"),
                        int("LayerMaskOffscrOffsetY"),
                    ),
                    special_render: int("SpecialRenderType"),
                    draw_colour: (
                        int("DrawColorMainRed"),
                        int("DrawColorMainGreen"),
                        int("DrawColorMainBlue"),
                    ),
                },
            );
        }

        Ok(Self {
            rows,
            mipmaps: pairs(db, "Mipmap", "MainId", "BaseMipmapInfo")?,
            infos: pairs(db, "MipmapInfo", "MainId", "Offscreen")?,
            offscreens: offscreens(db)?,
        })
    }

    /// The stack, bottom first, with each folder after its own contents.
    fn tree(&self, root: i64, warnings: &mut Vec<ImportWarning>) -> Result<Vec<Node>, ImportError> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(root);
        let start = self.rows.get(&root).map_or(0, |r| r.first_child);
        self.chain(start, 0, &mut seen, &mut out, warnings)?;
        Ok(out)
    }

    /// One `LayerNextIndex` chain, and everything under each folder on it.
    ///
    /// `seen` bounds the walk absolutely: a file whose chain loops back on
    /// itself, or whose folder is its own child, stops rather than running
    /// until the stack does. So does the entry count, which is checked as the
    /// list grows rather than after it — a hostile file naming a million layers
    /// must not build a million entries to be told there are too many.
    ///
    /// **The nesting needs a bound of its own, and the entry count is not it.**
    /// This descends into a folder *before* pushing anything, so a hundred
    /// thousand folders nested one inside the next is a hundred thousand stack
    /// frames with `out` still empty — `seen` stops it repeating a row and does
    /// nothing about a chain of distinct ones. Every level of nesting is a
    /// folder and every folder is an entry, so a document nested deeper than
    /// [`LayerStack::MAX`] has more entries than the stack holds however it is
    /// counted, and saying so is the honest refusal rather than a guard figure
    /// invented for the recursion.
    fn chain(
        &self,
        first: i64,
        depth: usize,
        seen: &mut std::collections::HashSet<i64>,
        out: &mut Vec<Node>,
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), ImportError> {
        if depth >= LayerStack::MAX {
            return Err(ImportError::TooManyLayers {
                found: depth + 1,
                max: LayerStack::MAX,
            });
        }
        let mut id = first;
        while id != 0 {
            if !seen.insert(id) {
                break;
            }
            let Some(row) = self.rows.get(&id) else {
                break;
            };
            if out.len() >= LayerStack::MAX {
                return Err(ImportError::TooManyLayers {
                    found: out.len() + 1,
                    max: LayerStack::MAX,
                });
            }
            let folder = row.folder & FOLDER != 0;
            if folder {
                // Nested deeper than Umber can hold. The depth is capped, which
                // merges this folder into the one outside it; said out loud,
                // because the grouping is the only thing a folder *is*.
                // `>=`, not `>`: a folder *at* the cap has its children capped
                // to the same depth, so they stop being its contents and the
                // grouping is gone. `openraster` and `krita` have the same
                // comparison and the same off-by-one.
                if depth >= LayerStack::MAX_DEPTH as usize {
                    warnings.push(ImportWarning::GroupFlattened {
                        group: row.name.clone(),
                    });
                }
                // Contents first, then the folder — which is where a
                // `LayerStack` keeps one, above its own subtree.
                self.chain(row.first_child, depth + 1, seen, out, warnings)?;
            }
            out.push(Node {
                id,
                depth: depth.min(LayerStack::MAX_DEPTH as usize) as u8,
                folder,
            });
            id = row.next;
        }
        Ok(())
    }

    /// The `Offscreen` a mipmap chain ends at.
    fn offscreen(&self, mipmap: i64) -> Option<&Offscreen> {
        let info = self.mipmaps.get(&mipmap)?;
        let offscreen = self.infos.get(info)?;
        self.offscreens.get(offscreen)
    }
}

/// An `Offscreen` row: the `Attribute` blob and the external chunk it names.
type Offscreen = (Vec<u8>, Vec<u8>);

struct Node {
    id: i64,
    depth: u8,
    folder: bool,
}

/// A whole table read as a `MainId -> other column` map.
fn pairs(
    db: &Database<'_>,
    name: &str,
    key: &str,
    value: &str,
) -> Result<HashMap<i64, i64>, ImportError> {
    let mut out = HashMap::new();
    let Some(table) = table(db, name)? else {
        return Ok(out);
    };
    let rows = db.rows(&table).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: format!("its {name} table could not be read ({e})"),
    })?;
    let (Some(key), Some(value)) = (table.column(key), table.column(value)) else {
        return Ok(out);
    };
    for row in &rows {
        if let (Some(k), Some(v)) = (row.get(key).as_i64(), row.get(value).as_i64()) {
            out.insert(k, v);
        }
    }
    Ok(out)
}

fn offscreens(db: &Database<'_>) -> Result<HashMap<i64, Offscreen>, ImportError> {
    let mut out = HashMap::new();
    let Some(table) = table(db, "Offscreen")? else {
        return Ok(out);
    };
    let rows = db.rows(&table).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: format!("its Offscreen table could not be read ({e})"),
    })?;
    let (Some(id), Some(attribute), Some(blocks)) = (
        table.column("MainId"),
        table.column("Attribute"),
        table.column("BlockData"),
    ) else {
        return Ok(out);
    };
    for row in &rows {
        let Some(key) = row.get(id).as_i64() else {
            continue;
        };
        // `BlockData` is an `extrnlid…` name. It is a blob in every file seen
        // and text is accepted too, because the column is declared with no
        // affinity and SQLite would store either.
        let (Some(attribute), Some(name)) = (blob(row.get(attribute)), blob(row.get(blocks)))
        else {
            continue;
        };
        out.insert(key, (attribute, name));
    }
    Ok(out)
}

fn blob(value: &Value) -> Option<Vec<u8>> {
    match value {
        Value::Blob(b) => Some(b.clone()),
        Value::Text(t) => Some(t.as_bytes().to_vec()),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

/// Turn one node of the tree into a layer, or refuse it with a reason.
fn build(
    row: &LayerRow,
    node: &Node,
    canvas: &Canvas,
    tables: &Tables,
    container: &Container<'_>,
    warnings: &mut Vec<ImportWarning>,
) -> Option<ImportedLayer> {
    let name = clean_name(&row.name, node.folder);
    if node.folder {
        // **A folder carries less than a layer does, and each thing it drops is
        // named.** Umber's folders are pass-through and hold no opacity and no
        // blend mode of their own, so a Clip Studio folder at half opacity or
        // set to Multiply cannot arrive as it was — and unlike ORA and Krita
        // there is nothing to fold it into, because the contents were built
        // before this entry and a fold would have to reach back through the
        // whole subtree. So it is reported rather than applied, which is the
        // honest half of what those two readers do.
        if row.opacity < 1.0 {
            warnings.push(ImportWarning::GroupOpacityFolded {
                group: name.clone(),
            });
        }
        // **A folder's mode is lost whether or not Umber has it**, which is not
        // the test a layer's takes. Every Umber folder is pass-through, so a
        // Clip Studio folder set to Multiply loses the Multiply even though a
        // *layer* set to Multiply keeps it — asking `blend::nearest` here would
        // report the modes Umber lacks and stay silent about the four it has.
        // Composite 0 and 30 are the two that arrive intact: Clip Studio's
        // "Through" is pass-through, and Normal over an ordinary group is the
        // same picture.
        if !matches!(row.composite, 0 | 30) {
            warnings.push(ImportWarning::BlendDropped {
                layer: name.clone(),
                source: composite_name(row.composite).to_string(),
            });
        }
        // A folder holds no slot, so it can hold no mask either, and every
        // layer inside it now covers more than it did — which is exactly what
        // `MaskIgnored` says. Krita's reader says the same about the same case.
        if row.mask_mipmap != 0 {
            warnings.push(ImportWarning::MaskIgnored {
                layer: name.clone(),
            });
        }
        let mut folder = ImportedLayer::folder(name, node.depth, row.visible);
        folder.locked = row.locked;
        folder.clipped = row.clipped;
        return Some(folder);
    }

    // A correction layer changes what is under it and holds no picture of its
    // own. Importing its `Offscreen` would put a flat sheet over the drawing.
    if row.kind & LAYER_IS_CORRECTION != 0 {
        warnings.push(ImportWarning::LayerSkipped {
            layer: name,
            reason: "it is a correction layer, which changes the layers below it rather than \
                     holding pixels of its own"
                .to_string(),
        });
        return None;
    }

    let Some((attribute, chunk)) = tables.offscreen(row.render_mipmap) else {
        warnings.push(ImportWarning::LayerSkipped {
            layer: name,
            reason: "the file does not hold its pixels".to_string(),
        });
        return None;
    };
    let placed = match colour(
        attribute,
        chunk,
        container,
        (
            row.offset.0 + row.render_offset.0,
            row.offset.1 + row.render_offset.1,
        ),
        canvas.size,
    ) {
        Ok(pixels) => pixels,
        Err(reason) => {
            warnings.push(ImportWarning::LayerSkipped {
                layer: name,
                reason,
            });
            return None;
        }
    };

    // Not made of pixels in the file, and made of pixels here. Clip Studio's
    // own PSD export does the same; what is lost is that it can no longer be
    // edited as text, as a curve or as a tone.
    // Named in words rather than by `LayerType`'s bits. The number is out of a
    // private schema and there is nothing a painter can do with it; what they
    // can act on is that the thing they typed is now paint, so they can go back
    // and export it separately. Which *kind* it was cannot be told apart
    // reliably here, so the sentence does not pretend to.
    if row.kind & LAYER_IS_PIXEL == 0 {
        warnings.push(ImportWarning::LayerRasterised {
            layer: name.clone(),
            what: "a text, vector or similar layer that Clip Studio draws for you".to_string(),
        });
    }

    let source = composite_name(row.composite);
    let (mode, fidelity) = blend::nearest(source);
    match fidelity {
        Fidelity::Exact => {}
        Fidelity::Approximate => warnings.push(ImportWarning::BlendApproximated {
            layer: name.clone(),
            source: source.to_string(),
            used: mode.label(),
        }),
        Fidelity::Dropped => warnings.push(ImportWarning::BlendDropped {
            layer: name.clone(),
            source: source.to_string(),
        }),
    }

    let mut layer = ImportedLayer::new(name.clone(), mode, placed);
    layer.visible = row.visible;
    layer.opacity = row.opacity;
    layer.locked = row.locked;
    layer.clipped = row.clipped;
    layer.depth = node.depth;

    // **A mask Clip Studio has switched off bounds nothing there either**, so
    // applying it would hide pixels the artist can see. The eye is
    // `LayerVisibility`'s second bit, beside the layer's own; `LayerMasking`'s
    // first bit says the same thing and is deliberately not also required,
    // because two readings that have to agree is one more way to drop a mask
    // that was live. It is `MaskUnsupported` rather than `MaskIgnored` for the
    // reason Krita's disabled mask is: the picture is right without it and only
    // the mask itself is lost, where `MaskIgnored` would claim the layer covers
    // more than it did.
    if row.mask_mipmap != 0 && !row.mask_visible {
        warnings.push(ImportWarning::MaskUnsupported {
            layer: name,
            what: "a layer mask that was switched off".to_string(),
        });
    } else if row.mask_mipmap != 0 {
        match tables.offscreen(row.mask_mipmap) {
            Some((attribute, chunk)) => match coverage(
                attribute,
                chunk,
                container,
                (
                    row.offset.0 + row.mask_offset.0 + row.mask_offscreen_offset.0,
                    row.offset.1 + row.mask_offset.1 + row.mask_offscreen_offset.1,
                ),
                canvas.size,
            ) {
                Ok(mask) => layer.mask = Some(srgb::encode_coverage_buffer(&mask)),
                Err(_) => warnings.push(ImportWarning::MaskIgnored { layer: name }),
            },
            None => warnings.push(ImportWarning::MaskIgnored { layer: name }),
        }
    }
    Some(layer)
}

/// A folder Clip Studio left unnamed — the root's own name is empty — still
/// needs something in the panel.
fn clean_name(raw: &str, folder: bool) -> String {
    let trimmed = raw.trim();
    if !trimmed.is_empty() {
        return trimmed.to_string();
    }
    if folder { "Folder" } else { "Layer" }.to_string()
}

// ---------------------------------------------------------------------------
// Pixels
// ---------------------------------------------------------------------------

/// What an `Offscreen` names: the shape of its bitmap, how its bytes are
/// packed, and the blob its blocks are in.
///
/// The blob rather than the blocks, because [`csblocks::for_each_block`] hands
/// them over one at a time — a whole grid of decompressed blocks is 1.3 GB on
/// the largest canvas an import will accept, and every one of them can be a
/// hundred-byte zlib stream.
fn read_bitmap<'a>(
    attribute: &[u8],
    chunk: &[u8],
    container: &'a Container<'a>,
    canvas: UVec2,
) -> Result<(Bitmap, Packing, &'a [u8]), String> {
    // **Bounded by the canvas rather than by `MAX_DIMENSION`.** A layer may
    // legitimately hang off the page, so the bound has slack; what it must not
    // be is the global ceiling, because nothing else ties a bitmap to the
    // document it is in. A 1×1 canvas with sixty-four layers passes
    // `check_bounds` at 256 bytes, and each of those layers could still declare
    // 16384² — a 64×64 grid, 4096 blocks, 1.3 GB of inflate each, every byte of
    // it thrown away by the blit. Worse, nothing stops all of them naming the
    // *same* external chunk, so one small blob would be inflated a hundred and
    // twenty-eight times.
    let bound = canvas
        .x
        .max(canvas.y)
        .saturating_mul(2)
        .max(BLOCK as u32 * 2)
        .min(ImportedDocument::MAX_DIMENSION);
    let bitmap = csblocks::parse_attribute(attribute, bound)
        .ok_or_else(|| "Umber could not read the shape of its bitmap".to_string())?;
    let packing = bitmap
        .packing
        .ok_or_else(|| "its bitmap does not say how its channels are packed".to_string())?;
    let data = container
        .external
        .get(chunk)
        .ok_or_else(|| "the file does not hold its pixels".to_string())?;
    Ok((bitmap, packing, data))
}

/// A layer's colour, canvas-sized, straight-alpha sRGB, ready for
/// [`srgb::encode_buffer`] — which is applied here, so the answer is already in
/// the form a layer texture holds.
fn colour(
    attribute: &[u8],
    chunk: &[u8],
    container: &Container<'_>,
    origin: (i64, i64),
    canvas: UVec2,
) -> Result<Vec<u8>, String> {
    let (bitmap, packing, data) = read_bitmap(attribute, chunk, container, canvas)?;
    if packing.first != 1 || !matches!(packing.second, 1 | 4) {
        return Err(format!(
            "its pixels are packed {} and {} channels at a time, which Umber cannot read",
            packing.first, packing.second
        ));
    }
    // **A stated fill is refused and an unreadable one is not**, and the two
    // must not be run together. `Stated` means the file says an absent block
    // carries a colour, and a colour fill is four values whose meaning nothing
    // here has checked against a file that paints with one. `Unknown` means the
    // `InitColor` section could not be located at all — an older `Attribute`
    // layout, say — and refusing every layer of such a document over a section
    // that is *usually* "nothing" would cost the artist their picture to protect
    // a case that has never been seen. It is read as empty, which is what every
    // other reader of this format does.
    if let Fill::Stated(_) = bitmap.fill {
        return Err(
            "it is filled with a colour Clip Studio states in a form Umber cannot read".to_string(),
        );
    }
    if !overlaps(&bitmap, origin, canvas) {
        return Err("it lies entirely outside the canvas".to_string());
    }

    let mut out = vec![0u8; canvas.x as usize * canvas.y as usize * 4];
    csblocks::for_each_block(
        data,
        packing,
        bitmap.columns * bitmap.rows,
        |index, block| {
            let (column, row) = (index % bitmap.columns, index / bitmap.columns);
            for y in 0..within(row, bitmap.height) {
                let Some(dst_y) = canvas_at(origin.1, row, y, canvas.y) else {
                    continue;
                };
                for x in 0..within(column, bitmap.width) {
                    let Some(dst_x) = canvas_at(origin.0, column, x, canvas.x) else {
                        continue;
                    };
                    let i = y * BLOCK + x;
                    // **The alpha plane comes first, then the colour interleaved.**
                    // Four bytes at a time it is B, G, R and one nothing reads; one
                    // byte at a time it is grey.
                    let alpha = block[i];
                    let at = csblocks::PLANE + i * packing.second;
                    let (r, g, b) = if packing.second == 4 {
                        (block[at + 2], block[at + 1], block[at])
                    } else {
                        let v = block[at];
                        (v, v, v)
                    };
                    let px = (dst_y * canvas.x as usize + dst_x) * 4;
                    out[px..px + 4].copy_from_slice(&[r, g, b, alpha]);
                }
            }
        },
    )
    .ok_or_else(|| "its pixels could not be read".to_string())?;
    srgb::encode_buffer(&mut out);
    Ok(out)
}

/// A mask's coverage, canvas-sized, as the **linear** multiplier every source
/// format states one in — [`srgb::encode_coverage_buffer`] is what turns it
/// into the bytes a mask slice holds, and it is the caller's to apply.
fn coverage(
    attribute: &[u8],
    chunk: &[u8],
    container: &Container<'_>,
    origin: (i64, i64),
    canvas: UVec2,
) -> Result<Vec<u8>, String> {
    let (bitmap, packing, data) = read_bitmap(attribute, chunk, container, canvas)?;
    if packing.first != 1 || packing.second != 0 {
        return Err("its mask is not a single channel".to_string());
    }
    // **What a block Clip Studio did not store holds is read, not assumed.** A
    // mask starts revealing everything, so its `InitColor` states all-ones and
    // taking an absent block for zero would blank the layer. A mask whose fill
    // cannot be read at all is refused rather than guessed at, for the same
    // reason.
    let default = match bitmap.fill {
        Fill::Stated(v) => v,
        Fill::Empty => 0,
        Fill::Unknown => return Err("Umber could not read what its mask hides".to_string()),
    };

    let mut out = vec![default; canvas.x as usize * canvas.y as usize];
    csblocks::for_each_block(
        data,
        packing,
        bitmap.columns * bitmap.rows,
        |index, block| {
            let (column, row) = (index % bitmap.columns, index / bitmap.columns);
            for y in 0..within(row, bitmap.height) {
                let Some(dst_y) = canvas_at(origin.1, row, y, canvas.y) else {
                    continue;
                };
                for x in 0..within(column, bitmap.width) {
                    let Some(dst_x) = canvas_at(origin.0, column, x, canvas.x) else {
                        continue;
                    };
                    out[dst_y * canvas.x as usize + dst_x] = block[y * BLOCK + x];
                }
            }
        },
    )
    .ok_or_else(|| "its mask could not be read".to_string())?;
    Ok(out)
}

/// How much of one block's row or column is inside the **bitmap**.
///
/// A bitmap is padded out to whole blocks, so the last column and the last row
/// hold texels that are not the artist's — and a reader that clipped only
/// against the *canvas* would copy that padding over the picture wherever the
/// bitmap is smaller than the canvas. On a mask that is the difference between
/// the `InitColor` fill the file states and whatever Clip Studio left in the
/// corner of the block.
fn within(block: usize, extent: u32) -> usize {
    (extent as usize).saturating_sub(block * BLOCK).min(BLOCK)
}

/// Where one texel of one block lands on the canvas, or `None` for one that
/// falls outside it.
///
/// `i64` throughout: an origin out of somebody else's file can be negative, and
/// a block's own coordinate added to it must not wrap.
fn canvas_at(origin: i64, block: usize, within: usize, extent: u32) -> Option<usize> {
    let at = origin
        .checked_add((block * BLOCK) as i64)?
        .checked_add(within as i64)?;
    (at >= 0 && at < i64::from(extent)).then_some(at as usize)
}

/// Whether a bitmap placed at `origin` reaches the canvas at all.
///
/// A layer that misses it entirely is a placement this reader has got wrong far
/// more often than it is a layer somebody dragged off the page, so it is
/// refused with a reason rather than imported blank.
fn overlaps(bitmap: &Bitmap, origin: (i64, i64), canvas: UVec2) -> bool {
    let right = origin.0.saturating_add(i64::from(bitmap.width));
    let bottom = origin.1.saturating_add(i64::from(bitmap.height));
    origin.0 < i64::from(canvas.x) && origin.1 < i64::from(canvas.y) && right > 0 && bottom > 0
}

/// `LayerComposite` as a name [`blend`] knows.
///
/// The numbering was read straight off a Clip Studio file whose twenty-eight
/// layers are named after the modes they carry, so it is a table taken from the
/// thing itself rather than from a note. A number this does not know falls
/// through to `"unknown"`, which `blend::nearest` reports as dropped — the
/// right direction, because the user is told rather than the mode being quietly
/// mapped to the wrong one.
fn composite_name(composite: i64) -> &'static str {
    match composite {
        0 => "normal",
        1 => "darken",
        2 => "multiply",
        3 => "color-burn",
        4 => "linear-burn",
        5 => "subtract",
        6 => "darker-color",
        7 => "lighten",
        8 => "screen",
        9 => "color-dodge",
        10 => "glow-dodge",
        // Clip Studio's Add and Add (Glow). Named the way `blend` spells the
        // first of them — "linear-dodge" is the same operation and is what
        // Photoshop's reader already reports it as, so one document does not
        // describe its own blend modes two ways depending on which application
        // wrote it.
        11 => "linear-dodge",
        12 => "add-glow",
        13 => "lighter-color",
        14 => "overlay",
        15 => "soft-light",
        16 => "hard-light",
        17 => "vivid-light",
        18 => "linear-light",
        19 => "pin-light",
        20 => "hard-mix",
        21 => "difference",
        22 => "exclusion",
        23 => "hue",
        24 => "saturation",
        25 => "color",
        26 => "luminosity",
        // A folder's own pass-through. Umber's folders are all pass-through,
        // so there is nothing to report.
        30 => "src-over",
        36 => "divide",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{self, ClipLayer};
    use super::*;
    use crate::layer::BlendMode;

    /// **The Paper layer is the document's background, not one of its layers.**
    ///
    /// It holds a colour and no bitmap, so it used to fall through the "the file
    /// does not hold its pixels" path: all 33 real documents this was written
    /// against lost their paper and opened on transparency, each with a warning
    /// that read like a damaged file.
    ///
    /// The colour is driven at something that is **not** white deliberately.
    /// Every real sample is white, so a reader that ignored the columns and
    /// hard-coded a sheet of white would have passed against every one of them
    /// — and a channel read as a byte rather than as a fraction of `u32` is
    /// wrong by a factor of sixteen million, which white also hides.
    #[test]
    fn a_paper_layer_becomes_the_documents_background() {
        let bytes = fixtures::clip(
            8,
            8,
            &[
                ClipLayer::paper([32, 96, 200]),
                ClipLayer::flat("Ink", 8, 8, [255, 0, 0, 255]),
            ],
        );
        let doc = read(&bytes).expect("a document");

        let colour = doc
            .background
            .colour()
            .expect("the paper should have become an opaque background");
        assert_eq!(colour.to_srgb_u8(), [32, 96, 200, 255]);

        // And it is gone from the stack rather than sitting in it twice.
        let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["Ink"]);
        assert!(
            doc.warnings.is_empty(),
            "the paper is understood, so nothing is lost: {:?}",
            doc.warnings
        );
    }

    /// A hidden paper is somebody working on transparency, and the eye is the
    /// only thing in the file that says so.
    #[test]
    fn a_hidden_paper_layer_leaves_the_document_transparent() {
        let bytes = fixtures::clip(
            8,
            8,
            &[
                ClipLayer::paper([255, 255, 255]).hidden(),
                ClipLayer::flat("Ink", 8, 8, [255, 0, 0, 255]),
            ],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.background, Background::Transparent);
        assert_eq!(doc.layers.len(), 1);
    }

    /// A document that is nothing but paper has nothing to paint on, and the
    /// paper is not a layer that changes that.
    ///
    /// The case that made this worth pinning: `Empty` is raised from
    /// `out.iter().all(|l| l.folder)`, which a stack emptied by the paper
    /// partition satisfies vacuously — so the arm has to be reached with the
    /// paper *removed*, not merely skipped.
    #[test]
    fn a_document_of_nothing_but_paper_is_still_empty() {
        let bytes = fixtures::clip(8, 8, &[ClipLayer::paper([255, 255, 255])]);
        assert!(matches!(read(&bytes), Err(ImportError::Empty { .. })));
    }

    /// **A canvas measured in centimetres is not a canvas of that many pixels.**
    ///
    /// `Study skeleton.clip` is A4 at 600 dpi and opened as a 21×29 canvas,
    /// whereupon every layer missed it entirely and the document was refused as
    /// holding no layers. Nothing in that chain reads as a unit problem.
    ///
    /// The figures are the real file's, and the expected answer is **not**
    /// computed by repeating the conversion here — 4961×7016 is the canvas of a
    /// second real document in the same folder that stored the same A4 page in
    /// pixels. That is what makes this a reading of the format rather than the
    /// test agreeing with the code.
    #[test]
    fn a_canvas_measured_in_centimetres_becomes_its_real_pixel_size() {
        let bytes = fixtures::clip_sized(
            fixtures::CanvasSize::measured(21.0, 29.7, 1, Some(600.0)),
            &[ClipLayer::flat("Ink", 1, 1, [255, 0, 0, 255]).placed((1, 1), (0, 0))],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.size, UVec2::new(4961, 7016));
        assert_eq!(doc.dpi, Some(600.0));
    }

    /// Pixels stay pixels, and the resolution does not touch them.
    ///
    /// The direction that would break every ordinary document if the conversion
    /// were applied unconditionally: at 350 dpi a 300×300 canvas would come out
    /// 41338 square and be refused.
    #[test]
    fn a_canvas_measured_in_pixels_is_not_scaled_by_its_resolution() {
        let bytes = fixtures::clip_sized(
            fixtures::CanvasSize::pixels(300, 300),
            &[ClipLayer::flat("Ink", 300, 300, [255, 0, 0, 255])],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.size, UVec2::new(300, 300));
    }

    /// A unit this build has never seen is refused, never read as pixels.
    ///
    /// Clip Studio also offers millimetres, inches and points and nobody here
    /// has a file carrying one, so the codes are unknown. Falling back to pixels
    /// would reproduce the 21×29 canvas exactly; guessing at the order would be
    /// a canvas silently out by a factor of ten or twenty-five. Both are worse
    /// than a sentence naming the code, which is something somebody can report.
    #[test]
    fn a_canvas_in_a_unit_this_build_cannot_convert_is_refused() {
        for unit in [2, 3, 4, 99] {
            let bytes = fixtures::clip_sized(
                fixtures::CanvasSize::measured(210.0, 297.0, unit, Some(600.0)),
                &[ClipLayer::flat("Ink", 1, 1, [255, 0, 0, 255]).placed((1, 1), (0, 0))],
            );
            let err = read(&bytes).expect_err("an unknown unit must not be read as pixels");
            assert!(
                matches!(err, ImportError::Unsupported { .. }),
                "unit {unit}: {err:?}"
            );
            assert!(err.to_string().contains(&unit.to_string()));
        }
    }

    /// A physical measurement with no resolution cannot become pixels at all.
    #[test]
    fn a_measured_canvas_with_no_resolution_is_refused() {
        let bytes = fixtures::clip_sized(
            fixtures::CanvasSize::measured(21.0, 29.7, 1, None),
            &[ClipLayer::flat("Ink", 1, 1, [255, 0, 0, 255]).placed((1, 1), (0, 0))],
        );
        assert!(matches!(read(&bytes), Err(ImportError::Malformed { .. })));
    }

    /// **A document filed into folders is not charged for its filing**, which
    /// is the bug an artist met: a 15000×5000 `.clip` refused with "the canvas
    /// is larger than Umber can open", a canvas well inside `MAX_DIMENSION`.
    ///
    /// This is here rather than beside `check_bounds` because that is where the
    /// hole was. `folders_are_not_charged_for_pixels_they_do_not_hold` drives
    /// the *function* and passes whatever the reader hands it, so putting the
    /// entry count in both slots left all 1,061 tests green — the "a guard on a
    /// model is not a guard on the call site" failure, demonstrated by mutation
    /// rather than argued. `StackSize` makes the mistake hard to write; this
    /// makes it fail.
    ///
    /// The canvas has to be genuinely large, because the bound being tested is
    /// a byte total: at 10000² a stack of 64 is 25.6 GB and refused, while the
    /// one layer actually holding pixels is 400 MB and fine. The fixture stays
    /// small — `placed` gives that layer a 1×1 bitmap, and a folder carries no
    /// pixels at all — so what the test costs is the one canvas-sized buffer
    /// the reader legitimately produces.
    #[test]
    fn a_document_filed_into_folders_is_not_charged_for_the_folders() {
        let mut layers =
            vec![ClipLayer::flat("Ink", 1, 1, [10, 120, 240, 255]).placed((1, 1), (0, 0))];
        // 63 folders and one layer is the full `LayerStack::MAX`, so this is
        // also the worst a legal stack can be filed.
        for _ in 0..63 {
            layers.push(ClipLayer::folder("Group", Vec::new()));
        }
        let bytes = fixtures::clip(10000, 10000, &layers);

        let doc = read(&bytes).expect("a 10000² document with 63 folders on it should open");
        assert_eq!(doc.size, UVec2::new(10000, 10000));
        assert_eq!(doc.layers.iter().filter(|l| !l.folder).count(), 1);
        assert_eq!(doc.layers.iter().filter(|l| l.folder).count(), 63);
    }

    /// **Bottom first, and the folder above its own contents.**
    ///
    /// Both halves are the thing a reader of this format is most likely to get
    /// backwards, and both were established from real files rather than
    /// assumed — see the module docs. A document that arrived inverted would
    /// still open, still look like a picture, and be wrong.
    #[test]
    fn the_stack_arrives_bottom_first_with_folders_above_their_contents() {
        let bytes = fixtures::clip(
            300,
            300,
            &[
                ClipLayer::flat("Paper", 300, 300, [255, 255, 255, 255]),
                ClipLayer::folder(
                    "Ink",
                    vec![
                        ClipLayer::flat("Under", 300, 300, [255, 0, 0, 255]),
                        ClipLayer::flat("Over", 300, 300, [0, 0, 255, 255]),
                    ],
                ),
            ],
        );

        let doc = read(&bytes).expect("a document");
        let shape: Vec<(&str, u8, bool)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), l.depth, l.folder))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Paper", 0, false),
                ("Under", 1, false),
                ("Over", 1, false),
                ("Ink", 0, true),
            ]
        );
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.size, UVec2::new(300, 300));
        assert_eq!(doc.dpi, Some(350.0));
    }

    /// The pixels come back as the colour that went in, and through the
    /// premultiply every other importer uses.
    ///
    /// The canvas is deliberately not a multiple of 256, so the block grid has
    /// a partial column and a partial row — the case an off-by-one in the blit
    /// survives, and what this actually pins.
    ///
    /// It does **not** pin the channel order, and the colour's three differing
    /// channels buy nothing: the fixture writes `B G R` because the reader
    /// reads `B G R`, so a file that were really `R G B` would pass this
    /// identically. Nothing in this repository can settle that; the module docs
    /// say where the reading came from instead.
    #[test]
    fn a_layers_pixels_arrive_in_the_right_channels_and_the_right_places() {
        let (w, h) = (300u32, 260u32);
        let bytes = fixtures::clip(w, h, &[ClipLayer::flat("Ink", w, h, [10, 120, 240, 255])]);
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.layers.len(), 1);
        let pixels = &doc.layers[0].pixels;
        assert_eq!(pixels.len(), (w * h * 4) as usize);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            assert_eq!(px, [10, 120, 240, 255], "pixel {i}");
        }
    }

    /// Half-transparent white has to arrive premultiplied **in linear space**,
    /// which is the classic import bug and is invisible on anything opaque.
    #[test]
    fn transparency_is_premultiplied_like_every_other_import() {
        let bytes = fixtures::clip(
            300,
            300,
            &[ClipLayer::flat("Soft", 300, 300, [255, 255, 255, 128])],
        );
        let doc = read(&bytes).expect("a document");
        assert!((i32::from(doc.layers[0].pixels[0]) - 188).abs() <= 1);
        assert_eq!(doc.layers[0].pixels[3], 128);
    }

    /// A block Clip Studio did not store is transparent on a layer — the state
    /// of every corner nobody painted on. The fixture omits an all-zero block,
    /// which is what a real writer does.
    #[test]
    fn an_unstored_block_is_transparent() {
        let bytes = fixtures::clip(
            300,
            300,
            &[ClipLayer::flat("Blank", 300, 300, [0, 0, 0, 0])],
        );
        let doc = read(&bytes).expect("a document");
        assert!(doc.layers[0].pixels.iter().all(|b| *b == 0));
    }

    /// Every flag on the row travels, and an opacity is out of **256**.
    #[test]
    fn the_flags_and_the_opacity_come_across() {
        let bytes = fixtures::clip(
            300,
            300,
            &[
                ClipLayer::flat("Base", 300, 300, [1, 2, 3, 255]),
                ClipLayer::flat("Faint", 300, 300, [1, 2, 3, 255])
                    .opacity(128)
                    .hidden()
                    .locked()
                    .clipped(),
            ],
        );
        let doc = read(&bytes).expect("a document");
        let faint = &doc.layers[1];
        assert!((faint.opacity - 0.5).abs() < 1e-6, "{}", faint.opacity);
        assert!(!faint.visible);
        assert!(faint.locked);
        assert!(faint.clipped);
        assert!(doc.layers[0].visible && !doc.layers[0].locked);
    }

    /// The blend numbering is a table taken off a file whose layers are named
    /// after the modes they carry, and each answer is reported at its own
    /// fidelity.
    #[test]
    fn blend_modes_are_mapped_and_the_losses_are_named() {
        let cases: [(i64, BlendMode, bool); 5] = [
            (0, BlendMode::Normal, false),
            (2, BlendMode::Multiply, false),
            (8, BlendMode::Screen, false),
            // Darken has no formula of Umber's; Multiply is the same family.
            (1, BlendMode::Multiply, true),
            // Difference moves the picture somewhere Umber cannot follow.
            (21, BlendMode::Normal, true),
        ];
        for (composite, expected, reported) in cases {
            let bytes = fixtures::clip(
                300,
                300,
                &[ClipLayer::flat("L", 300, 300, [1, 2, 3, 255]).composite(composite)],
            );
            let doc = read(&bytes).expect("a document");
            assert_eq!(doc.layers[0].blend, expected, "composite {composite}");
            assert_eq!(
                !doc.warnings.is_empty(),
                reported,
                "composite {composite} warnings: {:?}",
                doc.warnings
            );
        }
    }

    /// A mask's byte is a **linear** multiplier and a mask slice holds
    /// sRGB-encoded coverage, so a half is stored as ~188 and never as 128.
    /// Copying the byte across would hide four fifths of a layer somebody hid
    /// by half.
    #[test]
    fn a_mask_arrives_encoded_rather_than_copied_across() {
        let (w, h) = (300u32, 300u32);
        let bytes = fixtures::clip(
            w,
            h,
            &[ClipLayer::flat("Ink", w, h, [0, 0, 0, 255]).mask(vec![128u8; (w * h) as usize])],
        );
        let doc = read(&bytes).expect("a document");
        let mask = doc.layers[0].mask.as_ref().expect("a mask");
        assert_eq!(mask.len(), (w * h * 4) as usize);
        assert!((i32::from(mask[0]) - 188).abs() <= 1, "{}", mask[0]);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    /// **What an unstored mask block holds is read out of the file, not
    /// assumed** — and the test starts from the case where the two readings
    /// disagree, which is the only way to know which one is being used.
    ///
    /// Clip Studio states it in `InitColor`: a mask begins revealing
    /// everything, so all-ones is what a real file carries and taking an
    /// absent block for zero would blank the layer everywhere nobody painted.
    /// The same mask is built twice, once against each fill, and the corner
    /// nobody touched has to follow the file.
    ///
    /// **The canvas is a whole number of blocks, and that is the point of the
    /// test rather than tidiness.** At 300 square every block but the first
    /// overhangs the canvas, so the fixture cannot leave any of them out and
    /// the sampled corner comes from a *stored* block whatever the fill says —
    /// which is how the first draft of this test passed under a reader that
    /// ignored `InitColor` entirely. At 512 the three other blocks are wholly
    /// inside, uniform, and therefore genuinely absent.
    #[test]
    fn what_an_unstored_mask_block_holds_follows_the_file() {
        let (w, h) = (512u32, 512u32);
        let corner = |fill: u8| {
            // Everything the stated fill, except a patch confined to the first
            // block — so the other three are omitted and have to be filled in.
            let mut coverage = vec![fill; (w * h) as usize];
            for y in 0..100usize {
                for x in 0..100usize {
                    coverage[y * w as usize + x] = 20;
                }
            }
            let bytes = fixtures::clip(
                w,
                h,
                &[ClipLayer::flat("Ink", w, h, [0, 0, 0, 255])
                    .mask(coverage)
                    .mask_fill(Some(fill))],
            );
            let doc = read(&bytes).expect("a document");
            let mask = doc.layers[0].mask.clone().expect("a mask");
            mask[(400 * w as usize + 400) * 4]
        };

        assert_eq!(corner(255), 255, "an unstored block of a revealing mask");
        assert_eq!(corner(0), 0, "an unstored block of a hiding mask");
    }

    /// **A bitmap smaller than the canvas lands where the file puts it, and
    /// nothing outside it reaches the canvas.**
    ///
    /// Two rules and each was a defect. The placement is the sum of three
    /// column pairs — `LayerOffset*`, `LayerRenderOffscrOffset*` and, for a
    /// mask, `LayerMaskOffset*` — which no test reached at all while every
    /// fixture bitmap was canvas-sized at the origin; the fixture splits the
    /// offset across them so a reader that read one pair would land at half of
    /// it. And a bitmap is padded out to whole blocks, so the last block's
    /// corner is not the artist's: clipping only against the canvas copies that
    /// padding over the picture, which on a mask is a band of rubbish where the
    /// file states a fill.
    #[test]
    fn a_bitmap_smaller_than_the_canvas_lands_where_the_file_puts_it() {
        let (w, h) = (512u32, 512u32);
        // A 300-square bitmap at (100, 60): two blocks across, so its right and
        // bottom edges fall inside a block and the padding is live.
        let (bw, bh) = (300u32, 300u32);
        let (ox, oy) = (100i64, 60i64);
        let bytes = fixtures::clip(
            w,
            h,
            &[ClipLayer::flat("Ink", bw, bh, [10, 120, 240, 255])
                .placed((bw, bh), (ox, oy))
                .mask(vec![200u8; (bw * bh) as usize])],
        );
        let doc = read(&bytes).expect("a document");
        let layer = &doc.layers[0];
        let mask = layer.mask.as_ref().expect("a mask");
        let at = |x: usize, y: usize| {
            let i = (y * w as usize + x) * 4;
            (&layer.pixels[i..i + 4], mask[i])
        };

        // Inside the placed rectangle.
        assert_eq!(at(100, 60).0, [10, 120, 240, 255], "its top-left corner");
        assert_eq!(at(399, 359).0, [10, 120, 240, 255], "its bottom-right");
        // One pixel outside it on each side: the layer is transparent and the
        // mask holds the stated fill, never the block's padding.
        assert_eq!(at(99, 60).0, [0, 0, 0, 0], "left of the bitmap");
        assert_eq!(at(100, 59).0, [0, 0, 0, 0], "above the bitmap");
        assert_eq!(at(400, 359).0, [0, 0, 0, 0], "right of the bitmap");
        assert_eq!(at(399, 360).0, [0, 0, 0, 0], "below the bitmap");
        assert_eq!(at(400, 359).1, 255, "the mask's fill, not the padding");
        assert_eq!(at(399, 360).1, 255, "the mask's fill, not the padding");
    }

    /// A mask Clip Studio has switched off bounds nothing there either, so
    /// applying it would hide pixels the artist can see. The mask is still in
    /// the file, so this is a loss and is named — as `MaskUnsupported` rather
    /// than `MaskIgnored`, because the picture is right without it.
    #[test]
    fn a_mask_that_is_switched_off_is_not_applied_and_is_named() {
        let (w, h) = (300u32, 300u32);
        let bytes = fixtures::clip(
            w,
            h,
            &[ClipLayer::flat("Ink", w, h, [0, 0, 0, 255])
                .mask(vec![0u8; (w * h) as usize])
                .mask_hidden()],
        );
        let doc = read(&bytes).expect("a document");
        assert!(
            doc.layers[0].mask.is_none(),
            "a mask that hides everything must not be applied when it is off"
        );
        assert!(
            matches!(
                doc.warnings.as_slice(),
                [ImportWarning::MaskUnsupported { layer, .. }] if layer == "Ink"
            ),
            "{:?}",
            doc.warnings
        );
    }

    /// **A folder carries less than a layer does, and every difference is
    /// named.** Umber's folders are pass-through: no opacity, no blend mode,
    /// no mask. A Clip Studio folder has all three, and a half-opaque folder
    /// arriving at full opacity with nothing said is the silent kind of wrong.
    #[test]
    fn a_folders_opacity_blend_and_mask_are_reported_rather_than_dropped() {
        let mut folder = ClipLayer::folder(
            "Ink",
            vec![ClipLayer::flat("Line", 300, 300, [1, 2, 3, 255])],
        );
        folder.opacity = 128;
        folder.composite = 2;
        folder.mask = Some(vec![255u8; 300 * 300]);
        let bytes = fixtures::clip(300, 300, &[folder]);

        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.layers.len(), 2);
        assert!(doc.layers[1].folder);
        for wanted in [
            ImportWarning::GroupOpacityFolded {
                group: "Ink".into(),
            },
            ImportWarning::BlendDropped {
                layer: "Ink".into(),
                source: "multiply".into(),
            },
            ImportWarning::MaskIgnored {
                layer: "Ink".into(),
            },
        ] {
            assert!(doc.warnings.contains(&wanted), "{:?}", doc.warnings);
        }
    }

    /// A correction layer is an operation on what is below it, not a picture.
    /// Importing its `Offscreen` would put a flat sheet over the drawing.
    #[test]
    fn a_correction_layer_is_refused_and_named() {
        let bytes = fixtures::clip(
            300,
            300,
            &[
                ClipLayer::flat("Paint", 300, 300, [1, 2, 3, 255]),
                ClipLayer::flat("Tone curve", 300, 300, [255, 255, 255, 255]).kind(4098),
            ],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Paint");
        assert!(
            matches!(
                doc.warnings.as_slice(),
                [ImportWarning::LayerSkipped { layer, .. }] if layer == "Tone curve"
            ),
            "{:?}",
            doc.warnings
        );
    }

    /// A layer that was not made of pixels arrives as pixels, and is named.
    ///
    /// Deliberately *not* a refusal: the picture is all there and looks right.
    /// What is lost is that a caption cannot be retyped, and an artist who is
    /// told can go back and export it separately.
    #[test]
    fn a_layer_that_was_not_pixels_arrives_as_pixels_and_says_so() {
        let bytes = fixtures::clip(
            300,
            300,
            &[ClipLayer::flat("Title", 300, 300, [9, 9, 9, 255]).kind(2)],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(&doc.layers[0].pixels[0..4], &[9, 9, 9, 255]);
        assert!(
            matches!(
                doc.warnings.as_slice(),
                [ImportWarning::LayerRasterised { layer, .. }] if layer == "Title"
            ),
            "{:?}",
            doc.warnings
        );
    }

    /// A file with nothing but folders in it has nowhere to paint.
    #[test]
    fn a_document_of_nothing_but_folders_is_refused() {
        let bytes = fixtures::clip(300, 300, &[ClipLayer::folder("Empty", Vec::new())]);
        assert!(
            matches!(read(&bytes), Err(ImportError::Empty { .. })),
            "a stack of folders has nowhere to paint"
        );
    }

    /// The container is checked before anything is believed about what is in
    /// it, and a file that is not one says which part it failed.
    #[test]
    fn something_that_is_not_a_clip_is_refused_by_name() {
        let err = read(b"not a clip file at all").unwrap_err();
        assert!(
            matches!(&err, ImportError::Malformed { detail, .. } if detail.contains("CSFCHUNK")),
            "{err:?}"
        );
        // A well-formed stream with no database in it is a different failure
        // and says so rather than being read as an empty document.
        let empty = fixtures::clip_container(&[], b"");
        let err = read(&empty).unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }

    /// **A chain that loops back on itself stops.**
    ///
    /// Driven against `Tables` directly, because the fixture cannot write a
    /// cycle — which is exactly why the guard is worth having: a file a
    /// stranger wrote can, and without the visited set the walk recurses until
    /// the stack runs out and the application dies.
    #[test]
    fn a_layer_chain_that_loops_stops_rather_than_running_for_ever() {
        fn row(next: i64, first_child: i64, folder: bool) -> LayerRow {
            LayerRow {
                name: "L".into(),
                kind: 1,
                folder: i64::from(folder),
                visible: true,
                mask_visible: true,
                locked: false,
                clipped: false,
                opacity: 1.0,
                composite: 0,
                next,
                first_child,
                render_mipmap: 0,
                mask_mipmap: 0,
                offset: (0, 0),
                render_offset: (0, 0),
                mask_offset: (0, 0),
                mask_offscreen_offset: (0, 0),
                special_render: 0,
                draw_colour: (0, 0, 0),
            }
        }

        // 1 is the root; 2 and 3 are siblings pointing at each other, and 3 is
        // a folder whose first child is itself.
        let rows = HashMap::from([
            (1i64, row(0, 2, true)),
            (2, row(3, 0, false)),
            (3, row(2, 3, true)),
        ]);
        let tables = Tables {
            rows,
            mipmaps: HashMap::new(),
            infos: HashMap::new(),
            offscreens: HashMap::new(),
        };
        let mut warnings = Vec::new();
        let nodes = tables.tree(1, &mut warnings).expect("a bounded walk");
        assert!(nodes.len() <= 2, "{} nodes", nodes.len());
    }

    /// **Nesting is bounded, and the entry count is not what bounds it.**
    ///
    /// The walk descends into a folder before pushing anything, so a chain of
    /// folders each inside the last recurses with nothing in the list to count
    /// — `seen` only stops a row repeating. Every level is a folder and every
    /// folder is an entry, so past `LayerStack::MAX` levels the document cannot
    /// be one a stack holds, whichever way it is counted.
    ///
    /// The fixture cannot write this either, which is why it is driven against
    /// `Tables` directly: 200 folders is 200 stack frames on the way down and a
    /// hundred thousand is the application gone.
    #[test]
    fn a_stack_of_folders_nested_deeper_than_the_stack_holds_is_refused_not_recursed() {
        let mut rows = HashMap::new();
        let deep = LayerStack::MAX as i64 * 4;
        for id in 1..=deep {
            let mut r = plain_row();
            r.folder = 1;
            r.first_child = if id < deep { id + 1 } else { 0 };
            rows.insert(id, r);
        }
        let tables = Tables {
            rows,
            mipmaps: HashMap::new(),
            infos: HashMap::new(),
            offscreens: HashMap::new(),
        };
        let mut warnings = Vec::new();
        assert!(matches!(
            tables.tree(1, &mut warnings),
            Err(ImportError::TooManyLayers { .. })
        ));
    }

    /// **An `InitColor` this reader could not locate is not the same as one
    /// that states a fill**, and only the second may cost a layer.
    ///
    /// `Stated` says the file put a colour in every block it did not store,
    /// which is four values nothing here has checked against a file that paints
    /// with one — so the layer is refused rather than opened with a sheet over
    /// it. `Unknown` says the section was not where the header's own lengths
    /// put it, which an older `Attribute` layout would produce; refusing every
    /// layer of such a document would cost the artist the picture to protect a
    /// case never seen.
    ///
    /// The test starts from the case where the two readings disagree, which is
    /// the only way to know which one is in force: a single `!=` against
    /// `Fill::Empty` passes the first half of this and fails the second.
    #[test]
    fn an_unreadable_fill_still_imports_where_a_stated_one_is_refused() {
        let (w, h) = (300u32, 300u32);
        let plain = ClipLayer::flat("Ink", w, h, [7, 8, 9, 255]);

        // A stated colour fill: refused, and named.
        let filled = fixtures::clip(
            w,
            h,
            &[ClipLayer::flat("Ink", w, h, [7, 8, 9, 255]).pixel_fill(255)],
        );
        let err = read(&filled).unwrap_err();
        assert!(matches!(err, ImportError::Empty { .. }), "{err:?}");

        // An `InitColor` that cannot be located: the layer still arrives. The
        // header's fourth length is overwritten so the four sections stop
        // accounting for the blob, which is the one signal `init_fill` has that
        // this is the layout it is reading. `Parameter` is untouched, so the
        // picture is as readable as it ever was.
        let attribute = crate::csblocks::fixture::attribute(
            w,
            h,
            Packing {
                first: 1,
                second: 4,
            },
            None,
        );
        let mut bytes = fixtures::clip(w, h, &[plain]);
        let mut patched = 0;
        for at in 0..bytes.len().saturating_sub(16) {
            if bytes[at..at + 12] == attribute[..12] {
                bytes[at + 12] = bytes[at + 12].wrapping_add(1);
                patched += 1;
            }
        }
        assert!(patched > 0, "the fixture must carry an Attribute header");

        let doc = read(&bytes).expect("a document");
        assert_eq!(&doc.layers[0].pixels[0..4], &[7, 8, 9, 255]);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    /// More layers than a `LayerStack` holds is refused **as the list grows**,
    /// not after it: a file naming a million must not build a million entries
    /// to be told there are too many.
    #[test]
    fn a_file_naming_more_layers_than_the_stack_holds_is_refused_while_walking() {
        let mut rows = HashMap::new();
        let count = LayerStack::MAX as i64 + 40;
        rows.insert(1i64, {
            let mut r = plain_row();
            r.folder = 1;
            r.first_child = 2;
            r
        });
        for id in 2..=count + 1 {
            let mut r = plain_row();
            r.next = if id < count + 1 { id + 1 } else { 0 };
            rows.insert(id, r);
        }
        let tables = Tables {
            rows,
            mipmaps: HashMap::new(),
            infos: HashMap::new(),
            offscreens: HashMap::new(),
        };
        let mut warnings = Vec::new();
        assert!(matches!(
            tables.tree(1, &mut warnings),
            Err(ImportError::TooManyLayers { .. })
        ));
    }

    fn plain_row() -> LayerRow {
        LayerRow {
            name: "L".into(),
            kind: 1,
            folder: 0,
            visible: true,
            mask_visible: true,
            locked: false,
            clipped: false,
            opacity: 1.0,
            composite: 0,
            next: 0,
            first_child: 0,
            render_mipmap: 0,
            mask_mipmap: 0,
            offset: (0, 0),
            render_offset: (0, 0),
            mask_offset: (0, 0),
            mask_offscreen_offset: (0, 0),
            special_render: 0,
            draw_colour: (0, 0, 0),
        }
    }

    /// Every byte of the container comes out of somebody else's file, and a
    /// panic takes the application down with every unsaved document in it.
    #[test]
    fn a_corrupt_clip_is_refused_and_never_panics() {
        let good = fixtures::clip(
            300,
            300,
            &[
                ClipLayer::flat("Paper", 300, 300, [255, 255, 255, 255]),
                ClipLayer::folder(
                    "Ink",
                    vec![
                        ClipLayer::flat("Line", 300, 300, [0, 0, 0, 200])
                            .mask(vec![200u8; 300 * 300]),
                    ],
                ),
            ],
        );
        assert!(read(&good).is_ok());

        let mut cases: Vec<Vec<u8>> = (0..48)
            .map(|i| good[..good.len() * i / 48].to_vec())
            .collect();
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..300 {
            let mut c = good.clone();
            for _ in 0..6 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let at = (seed >> 33) as usize % c.len();
                c[at] ^= ((seed >> 11) & 0xff) as u8;
            }
            cases.push(c);
        }
        for (i, case) in cases.iter().enumerate() {
            let verdict = std::panic::catch_unwind(|| read(case).is_ok());
            assert!(
                verdict.is_ok(),
                "case {i} panicked instead of being refused"
            );
        }
    }

    /// The whole way through: the reader, the bounds check every reader shares,
    /// and the engine state a caller is handed.
    #[test]
    fn a_clip_opens_end_to_end() {
        let bytes = fixtures::clip(
            300,
            300,
            &[ClipLayer::flat("Ink", 300, 300, [4, 5, 6, 255])],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.format, SourceFormat::ClipStudio);
        doc.validate().expect("a document the stack can hold");
        let opened = doc.open();
        assert_eq!(opened.stack.len(), 1);
        assert_eq!(opened.uploads.len(), 1);
    }
}
