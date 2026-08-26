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
//! - **A placed image** — an image file imported into the document and left
//!   resizable — is refused and named, and it is **not** a vector layer though
//!   it was reported as one until somebody looked. Clip Studio stores the
//!   picture that was imported in a second mipmap chain named by
//!   `ResizableOriginalMipmap`, plus the placement in a 184-byte
//!   `ResizableImageInfo` blob, and leaves the render chain's external chunks
//!   out of the file entirely — so the *pixels are there* and where they go is
//!   not. See "The pixels a placed image keeps" below.
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
//! # The pixels a placed image keeps, and why they are not taken
//!
//! This is on record because the pixels genuinely are in the file, so the next
//! person to look will ask, and the answer took a day's measurement.
//!
//! An artist's 45 MB document was refused whole as holding no layers. It is an
//! A4 page at 600 dpi carrying a Paper sheet and four placed images and nothing
//! else. Each of the four has a seven-level render chain (100%, 50%, … 1%)
//! whose **every** level names an external chunk the container does not hold,
//! and a `ResizableOriginalMipmap` chain whose base level names one it does:
//! 11,103,575 bytes, a 4961×7016 bitmap, exactly the canvas. 44.4 MB of the
//! 45.4 MB file is those four originals.
//!
//! **They are not placed at the identity, and that is what settles it.**
//! `ResizableImageInfo` is 184 bytes holding, among six fields nothing here can
//! explain, a scale, a centre, a half-extent and a destination quad. On that
//! document the four are scaled by 0.4434 into the four quadrants of the page —
//! a contact sheet. Blitting the originals where the layer's offsets say (all
//! zero) would stack four full-page copies on top of one another: not subtly
//! wrong, but wrong, and wearing the artist's own artwork while it was. A
//! second real document places a 10000×5000 original on a 5000×5000 canvas at
//! 0.8409, running off the page.
//!
//! So taking them needs two things this reader will not invent:
//!
//! 1. **A resampler.** The map is an affine one into a quad, so the source has
//!    to be filtered into canvas space. `umber-core` has none, deliberately —
//!    see the transform rules in `CLAUDE.md`: filtering is the hardware
//!    sampler's, and an importer cannot reach it.
//! 2. **A reading of `ResizableImageInfo` that is not a guess.** The layout
//!    above is inferred from five layers in two documents, every one of them at
//!    zero rotation with a uniform positive scale, and six of its twenty-three
//!    fields are unexplained. A wrong reading puts somebody's picture in the
//!    wrong place at the wrong size and says nothing — the failure that keeps
//!    the MediaBang reader unwritten and makes an unrecognised `CanvasUnit` a
//!    refusal rather than a guess.
//!
//! The one instrument that would make it evidence rather than a story is
//! already in the file: `CanvasPreview` is a flattened PNG of what the document
//! actually looks like, so a candidate placement can be *compared* rather than
//! argued. Whoever builds this should start there — and note that it verifies
//! the rotations the samples happen to contain, which is none of them.
//!
//! **The thumbnails are not a substitute.** `LayerThumbnail` holds one per
//! layer and they are 528 to 3,616 bytes; `CanvasPreview` is flattened, so it
//! has no layers in it at all. Both are the blurry, plausible, subtly wrong
//! output this module refuses everywhere else, and `docimport::preview` already
//! says nothing that decides pixels may read it.
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
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, PixelPiece, SourceFormat,
    StackSize, check_bounds, srgb,
};
use crate::color::Color;
use crate::csblocks::{self, BLOCK, Bitmap, Fill, Packing};
use crate::document::Background;
use crate::geom::PixelRect;
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

pub fn read(bytes: &[u8], progress: super::Progress<'_>) -> Result<ImportedDocument, ImportError> {
    let container = split(bytes)?;
    let db = Database::open(container.database).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: e.to_string(),
    })?;

    let canvas = canvas(&db)?;
    let mut warnings = Vec::new();

    let layers = Tables::read(&db)?;
    let all = layers.tree(canvas.root, LayerStack::MAX, &mut warnings)?;

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
    let mut budget = check_bounds(FORMAT, canvas.size.x, canvas.size.y, stack)?;

    let mut out = Vec::with_capacity(nodes.len());
    let total = nodes.len() as u32;
    for (done, node) in nodes.iter().enumerate() {
        // Before the layer rather than after, so a bar shows the work that is
        // about to take the time rather than the work already finished — the
        // rule `splash.rs` keeps for its own stages.
        progress(done as u32, total);
        let Some(row) = layers.rows.get(&node.id) else {
            continue;
        };
        // A refusal has already put its own sentence in `warnings`; a folder
        // whose contents were all refused is kept, empty, because it is where
        // the artist put things and an empty row says so.
        if let Some(layer) = build(row, node, &canvas, &layers, &container, &mut warnings) {
            // Charged as it lands rather than off the header, so the figure is
            // the paint the file holds and not `canvas x 4 x layers`. See
            // `PieceBudget`.
            budget.charge(&layer)?;
            out.push(layer);
        }
    }

    if out.iter().all(|l| l.folder) {
        // Everything that was dropped said why on the way past, and that list
        // is the only thing an artist can act on — a bare "contains no layers"
        // sent one looking for a corrupt file that was perfectly intact.
        return Err(ImportError::Empty {
            format: FORMAT,
            because: ImportError::reasons_from(&warnings),
        });
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

/// The SQLite database out of the chunk stream, and nothing else.
///
/// [`super::preview`] needs it and must not walk the chunks itself: two copies
/// of a container's framing is the drift `docformat`'s "there must never be a
/// second ORA reader" refuses, applied to the format that arrives inside a
/// wrapper. `split` also collects every external chunk, which a preview does
/// not need — the picture is in the database — but the walk is one pass and
/// splitting it further would be two shapes of one loop.
pub(super) fn database_chunk(bytes: &[u8]) -> Result<&[u8], ImportError> {
    Ok(split(bytes)?.database)
}

// ---------------------------------------------------------------------------
// Residency
// ---------------------------------------------------------------------------

/// How much of each layer this document actually holds.
/// [`super::residency`] is the argument and the caller.
///
/// It lives here rather than beside those types because everything it needs is
/// private to this module — the container, the four-table chain, the tree and
/// its direction, and the bitmap bound. A second walk of any of that is exactly
/// the drift `docformat`'s "there must never be a second ORA reader" refuses,
/// and a survey walking its own chain would be measuring a stack Umber does not
/// read.
///
/// Four deliberate departures from [`read`], each because the question is
/// different:
///
/// - **No `check_bounds`.** The document that provoked the tiling design is one
///   Umber refuses, and refusing to measure it would be answering a question
///   nobody asked.
/// - **[`super::residency::MAX_ENTRIES`] rather than [`LayerStack::MAX`]**, for
///   the same reason, one level up in the walk.
/// - **The Paper layer is left in the stack** and reported like any other. It
///   becomes a `Background` on import and allocates no slice, so it is *not*
///   counted in the occupancy the caller computes — but a survey that dropped
///   it silently would be one that could not say so.
/// - **Nothing canvas-sized is ever allocated**, under either reading. The
///   [`Reading::Contents`] pass hands one 256-square block at a time to
///   `for_each_block`, which is the bound that function exists for; the only
///   per-slice allocation is one bit per canvas tile.
pub(super) fn residency(
    bytes: &[u8],
    reading: super::residency::Reading,
) -> Result<super::residency::DocumentResidency, ImportError> {
    use super::residency::{DocumentResidency, MAX_ENTRIES, Reading, SliceResidency, place};

    let container = split(bytes)?;
    let db = Database::open(container.database).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: e.to_string(),
    })?;
    let canvas = canvas(&db)?;
    let tables = Tables::read(&db)?;
    // Warnings are collected and thrown away: every one of them is about how a
    // layer would *import*, and `skipped` below is this walk's own reporting.
    let mut warnings = Vec::new();
    let nodes = tables.tree(canvas.root, MAX_ENTRIES, &mut warnings)?;

    let mut out = DocumentResidency {
        size: canvas.size,
        reading,
        entries: nodes.len(),
        folders: nodes.iter().filter(|n| n.folder).count(),
        slices: Vec::new(),
        skipped: Vec::new(),
    };

    for node in &nodes {
        let Some(row) = tables.rows.get(&node.id) else {
            continue;
        };
        if node.folder {
            continue;
        }
        let name = clean_name(&row.name, false);
        // A paper sheet and a correction layer both hold an `Offscreen` that is
        // not a picture. Named rather than passed over, so the layer count in
        // the report adds up.
        if row.paper() {
            out.skipped
                .push((name, "the Paper layer, which becomes the background".into()));
            continue;
        }
        if row.kind & LAYER_IS_CORRECTION != 0 {
            out.skipped.push((name, "a correction layer".into()));
            continue;
        }

        let slice = |mipmap: i64, origin: (i64, i64), mask: bool| {
            if mipmap == 0 {
                return None;
            }
            let Some((attribute, chunk)) = tables.offscreen(mipmap) else {
                return Some(Err(row.no_pixels_reason().unwrap_or(NO_PIXELS).to_string()));
            };
            // `read_bitmap`'s own refusals are kept, packing included — the
            // survey has to describe the slices Umber would actually store,
            // not every rectangle the file happens to hold.
            let (bitmap, packing, data) =
                match read_bitmap(attribute, chunk, &container, canvas.size) {
                    Ok(read) => read,
                    // **A vector layer fails here rather than above**, exactly as
                    // it does in `build`: it has the whole mipmap chain and what is
                    // absent is the external chunk, because the strokes were never
                    // rasterised. Naming the cause at only one of the two sites is
                    // the bug that made every real vector layer report a damaged
                    // file, and a survey that miscounts them miscounts what it
                    // could not measure.
                    // Substituted only for the generic absence, exactly as
                    // `build` does — a survey that reported a bounds refusal as
                    // a placed image would miscount what it could not measure
                    // *and* misname it.
                    Err(reason) => {
                        return Some(Err(match row.no_pixels_reason() {
                            Some(named) if reason == NO_PIXELS => named.to_string(),
                            _ => reason,
                        }));
                    }
                };
            let grid = bitmap.columns * bitmap.rows;
            let Some(present) = csblocks::stored_blocks(data, grid) else {
                return Some(Err("its blocks could not be walked".to_string()));
            };

            // Every block's placement, worked out once. `None` is a block
            // nothing can see — off the page, or nothing but the padding past
            // the bitmap's own edge — and it is the same answer for both
            // readings, which is what stops a tile being charged for texels the
            // content scan never looked at.
            let size = UVec2::new(bitmap.width, bitmap.height);
            let placed: Vec<_> = (0..grid)
                .map(|i| {
                    place(
                        (i % bitmap.columns, i / bitmap.columns),
                        size,
                        origin,
                        canvas.size,
                    )
                })
                .collect();

            // **Which blocks hold something, measured against the fill rather
            // than against zero.** An absent block of a raster layer is
            // transparent and an absent block of a *mask* is all-ones, so a
            // mask block of all-ones is exactly as redundant as a layer block
            // of all-zeroes — testing both against zero would call every
            // full-reveal mask tile live and report a mask as dense.
            //
            // The first plane is the one read: alpha for a colour or greyscale
            // bitmap, coverage for a mask. What is under a zero alpha cannot
            // be seen, so it cannot make a tile worth backing.
            let blank_byte = match bitmap.fill {
                Fill::Stated(v) => v,
                // `Unknown` is read as empty everywhere else in this reader.
                Fill::Empty | Fill::Unknown => 0,
            };
            let live = match reading {
                Reading::Presence => None,
                Reading::Contents => {
                    let mut held = vec![false; grid];
                    let walked = csblocks::for_each_block(data, packing, grid, |index, block| {
                        // One block is live at a time and dropped here, which is
                        // the whole of what makes this pass affordable.
                        let Some(placed) = placed.get(index).and_then(Option::as_ref) else {
                            return;
                        };
                        let (xs, ys) = placed.local();
                        held[index] =
                            ys.flat_map(|y| xs.clone().map(move |x| y * BLOCK + x))
                                .any(|i| {
                                    // The first plane only: alpha, or a mask's own
                                    // coverage. Bounded by `block_len`, which
                                    // `decode_block` has already checked against
                                    // the packing.
                                    block[i] != blank_byte
                                });
                    });
                    if walked.is_none() {
                        return Some(Err("its blocks could not be decoded".to_string()));
                    }
                    Some(held)
                }
            };

            let across = (canvas.size.x as usize).div_ceil(BLOCK);
            let down = (canvas.size.y as usize).div_ceil(BLOCK);
            let mut touched = vec![false; across * down];
            let mut lit = vec![false; across * down];
            let mut stored = 0usize;
            let mut live_count = 0usize;
            for (index, held) in present.iter().enumerate() {
                if !held {
                    continue;
                }
                stored += 1;
                let alive = live.as_ref().is_some_and(|live| live[index]);
                live_count += usize::from(alive);
                let Some(placed) = placed.get(index).and_then(Option::as_ref) else {
                    continue;
                };
                let (xs, ys) = placed.tiles();
                for ty in ys {
                    for tx in xs.clone() {
                        touched[ty * across + tx] = true;
                        lit[ty * across + tx] |= alive;
                    }
                }
            }

            Some(Ok(SliceResidency {
                layer: clean_name(&row.name, false),
                mask,
                bitmap: UVec2::new(bitmap.width, bitmap.height),
                grid: (bitmap.columns, bitmap.rows),
                stored,
                covered: touched.iter().filter(|hit| **hit).count(),
                live: live.as_ref().map(|_| live_count),
                live_covered: live
                    .as_ref()
                    .map(|_| lit.iter().filter(|hit| **hit).count()),
                fill: match bitmap.fill {
                    Fill::Empty => "empty",
                    Fill::Stated(_) => "stated",
                    Fill::Unknown => "unknown",
                },
            }))
        };

        // The same two origins `build` computes, from the same three column
        // pairs — the placement is the one thing in this reader taken on
        // somebody else's word, so a second statement of it would be a second
        // thing to be wrong.
        let render = slice(
            row.render_mipmap,
            (
                row.offset.0 + row.render_offset.0,
                row.offset.1 + row.render_offset.1,
            ),
            false,
        );
        // A mask Clip Studio has switched off bounds nothing there and is not
        // imported here, so it would allocate no slice — measuring it would put
        // a slice in the sample that Umber never stores. Named rather than
        // passed over, exactly as `build` names it.
        let mask = if row.mask_mipmap != 0 && !row.mask_visible {
            Some(Err("a layer mask that was switched off".to_string()))
        } else {
            slice(
                row.mask_mipmap,
                (
                    row.offset.0 + row.mask_offset.0 + row.mask_offscreen_offset.0,
                    row.offset.1 + row.mask_offset.1 + row.mask_offscreen_offset.1,
                ),
                true,
            )
        };
        // **A layer naming no render mipmap is reported and a layer with no
        // mask is not**, which is the one asymmetry here: most layers have no
        // mask and saying so five hundred times would bury the list, while a
        // layer with no *picture* is a slice missing from the sample and one
        // real document in the folder is five of them.
        match render {
            Some(Ok(slice)) => out.slices.push(slice),
            Some(Err(reason)) => out.skipped.push((name.clone(), reason)),
            None => out
                .skipped
                .push((name.clone(), "it names no rendered bitmap".into())),
        }
        match mask {
            Some(Ok(slice)) => out.slices.push(slice),
            Some(Err(reason)) => out.skipped.push((format!("{name} (mask)"), reason)),
            None => {}
        }
    }

    Ok(out)
}

pub(super) fn table(db: &Database<'_>, name: &str) -> Result<Option<Table>, ImportError> {
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
    /// `ResizableOriginalMipmap`: the chain holding the **source** picture of a
    /// layer that is a placed image rather than paint.
    ///
    /// Non-zero only on a resizable-image layer, and it is the one column that
    /// tells such a layer apart from a vector one — see
    /// [`LayerRow::no_pixels_reason`], which read every one of them as a vector
    /// layer until this was added. Nothing follows the chain: what it holds is
    /// the picture *before* placement, and where the placement lives is
    /// `ResizableImageInfo`, which is not read. See the module docs.
    resizable_original: i64,
    /// `DrawColorMainRed`/`Green`/`Blue`, each `0..=u32::MAX` rather than a
    /// byte. Only meaningful on a layer that draws a flat colour.
    draw_colour: (i64, i64, i64),
}

/// How much larger than the canvas a layer's own bitmap may be, by area.
///
/// Measured rather than picked: over 5,438 bitmaps in 33 real documents the
/// worst is 15.93×, so this is a little over twice what any of them needs. See
/// [`read_bitmap`] for why the quantity is area and not a side.
const BITMAP_AREA_SLACK: u64 = 32;

/// The longest a single edge of a layer's bitmap may be.
///
/// **Not [`ImportedDocument::MAX_DIMENSION`]**, which is the bound on a
/// *canvas* and is the wrong instrument here: a layer's bitmap is intermediate
/// data that the blit clips into the canvas, so there is no reason it cannot be
/// wider than the largest document Umber opens. One real 15000×5000 file stores
/// a layer 19712 px across — 1.2× that ceiling and only 2.1× the canvas by
/// area — and it was refused for its width alone.
///
/// What remains is a bound on *shape* rather than on cost, since
/// [`BITMAP_AREA_SLACK`] is what bounds the work: it stops a bitmap one pixel
/// tall and a billion wide, whose area could pass while its block grid is
/// millions of columns to walk. Four times the largest canvas, which is three
/// times the worst edge measured in any real document.
const BITMAP_MAX_SIDE: u32 = 4 * ImportedDocument::MAX_DIMENSION;

/// The smallest area bound, whatever the canvas is.
///
/// A very small canvas would otherwise be held to a very small bitmap, which is
/// exactly backwards — the worst real ratio in the sample is a 1200×480 banner,
/// because the smaller the page the further a layer reaches past it in relative
/// terms. 1024² is one megapixel, about 5 MB to inflate, which is the ceiling
/// the pathological 1×1 canvas is held to.
const BITMAP_AREA_FLOOR: u64 = 1024 * 1024;

/// What is said where the absence of a layer's pixels has no cause this reader
/// can name.
///
/// One statement of it, because [`read_bitmap`] produces the same sentence and
/// the two must not drift into being different wordings of one condition.
const NO_PIXELS: &str = "the file does not hold its pixels";

/// `LayerType`'s value for a **vector** layer.
///
/// Zero, which is also what a folder carries — so this alone never decides
/// anything: `build` answers the folder question first, off `LayerFolder`, and
/// only a non-folder reaches [`LayerRow::no_pixels_reason`].
const VECTOR_KIND: i64 = 0;

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
    /// Why this layer has no pixels in the file, phrased for the artist.
    ///
    /// **"The file does not hold its pixels" reads as damage, and for a vector
    /// layer it is a lie about the cause.** Clip Studio stores a vector layer
    /// as *strokes* — that is what it is for — and rasterises them on demand,
    /// so there is genuinely no bitmap at any level of the mipmap chain. The
    /// document is perfectly intact; Umber simply has no vector renderer. An
    /// artist told the first sentence goes looking for a corrupt file, and an
    /// artist told the second uses Layer → Rasterize and re-saves.
    ///
    /// Measured across 33 real documents: 28 of 542 painted layers are vector.
    /// The only raster Clip Studio keeps for one is a small thumbnail, which is
    /// no use as a layer — substituting it would be the blurry, plausible,
    /// subtly wrong output this module refuses everywhere else.
    ///
    /// `LayerType` is the reading, and 0 is what a vector layer carries;
    /// `VectorNormalStrokeIndex` is set beside it on one that has been drawn
    /// on, but not on an empty one, so the type is the reliable half.
    ///
    /// **`LayerType == 0` is not enough on its own, and reading it as though it
    /// were called a real document's every layer a vector layer.** A *placed
    /// image* — an image file imported into a document and left resizable
    /// rather than rasterised — carries `LayerType` 0 as well, has the same
    /// empty render chain, and is a different thing with a different cause:
    /// Clip Studio keeps the picture that was imported plus the size and
    /// position it was given, and redraws it on demand. `ResizableOriginalMipmap`
    /// is what tells the two apart, so it is asked **first**.
    ///
    /// Measured over the same 33 documents: 5 of the 28 layers this used to
    /// call vector are placed images, and 4 of those 5 are every painted layer
    /// of one document — which is exactly the file that was refused whole as
    /// holding no layers.
    ///
    /// **The remedy is the same sentence and the cause is not**, which is the
    /// point: Layer → Rasterize fixes both, and an artist told their imported
    /// photograph is a vector layer has been told something false about their
    /// own document.
    /// **`None` is "this reader cannot name a cause", and it is an `Option` so
    /// that the four sites which ask cannot disagree about which layers have
    /// one.** Two of those sites reach this through a *generic* failure —
    /// `read_bitmap` saying the chunk is absent — and each used to decide for
    /// itself, by restating `kind == VECTOR_KIND`, whether to override that
    /// sentence. That is two copies of the list of causes, and adding the
    /// placed-image cause to one and not the other would have left every
    /// placed image in a real document still reporting a damaged file, which is
    /// precisely the bug the vector arm was added to fix, one revision later.
    /// The fallback now lives at the call site and the *list* lives here once.
    ///
    /// **What is guarded and what is structural.** `build`'s two drop sites are
    /// both driven —
    /// `a_layer_that_names_no_rendered_bitmap_still_names_its_own_cause`
    /// reaches the first and the placed-image and vector tests reach the
    /// second — and both were demonstrated by mutation. `residency`'s two are
    /// **not**: they are the survey path, they build no document, and a wrong
    /// sentence there is a wrong row in a report rather than a wrong thing said
    /// to an artist. So the `Option` makes all four agree by construction and
    /// tests hold two of them; do not read the first sentence as claiming more.
    fn no_pixels_reason(&self) -> Option<&'static str> {
        if self.resizable_original != 0 {
            Some(
                "it is an image placed into the document rather than painted, so \
                 Clip Studio keeps the picture that was imported and redraws it at \
                 the size and position you gave it instead of saving it as pixels; \
                 select it in Clip Studio, use Layer then Rasterize, and save again \
                 to bring it across",
            )
        } else if self.kind == VECTOR_KIND {
            Some(
                "it is a vector layer, which Clip Studio stores as strokes rather \
                 than pixels; rasterise it in Clip Studio and save again to bring \
                 it across",
            )
        } else {
            None
        }
    }

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
                    resizable_original: int("ResizableOriginalMipmap"),
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
    ///
    /// `limit` bounds both the entries and the nesting, and it is a parameter
    /// rather than [`LayerStack::MAX`] for one caller: [`residency`] measures
    /// documents Umber **refuses**, which is exactly the set of documents worth
    /// a figure, so it would otherwise be blind to the one file the tiling
    /// design was written about. Every other caller passes the stack's own cap
    /// and behaves exactly as before.
    fn tree(
        &self,
        root: i64,
        limit: usize,
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<Vec<Node>, ImportError> {
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(root);
        let start = self.rows.get(&root).map_or(0, |r| r.first_child);
        self.chain(start, 0, limit, &mut seen, &mut out, warnings)?;
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
        limit: usize,
        seen: &mut std::collections::HashSet<i64>,
        out: &mut Vec<Node>,
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), ImportError> {
        if depth >= limit {
            return Err(ImportError::TooManyLayers {
                found: depth + 1,
                max: limit,
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
            if out.len() >= limit {
                return Err(ImportError::TooManyLayers {
                    found: out.len() + 1,
                    max: limit,
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
                self.chain(row.first_child, depth + 1, limit, seen, out, warnings)?;
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
            reason: row.no_pixels_reason().unwrap_or(NO_PIXELS).to_string(),
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
            // **A vector layer fails here rather than above**, and that is the
            // case this whole distinction exists for. It *has* a mipmap chain
            // and an `Offscreen` row — Clip Studio writes the bookkeeping for
            // one — and what is missing is the external chunk the row points
            // at, because the strokes were never rasterised into a block. So
            // the honest sentence has to be reached from both sites, and
            // guarding only the earlier one left every real vector layer still
            // reporting a file with something missing. A **placed image** is
            // the same shape for a different reason and arrives here too.
            warnings.push(ImportWarning::LayerSkipped {
                layer: name,
                // **Only the generic absence is replaced**, which is narrower
                // than substituting on the row's kind alone. `read_bitmap` also
                // refuses a bitmap whose shape will not read or whose packing
                // it does not know, and those are statements about *this*
                // bitmap that the layer's kind does not override — telling
                // somebody whose placed image met a bounds refusal to go and
                // rasterise it is a remedy for a cause they did not meet. No
                // layer in the 33 real documents reaches here with anything but
                // `NO_PIXELS`, so nothing observable moved; what changed is
                // that the claim is checked against the failure rather than
                // assumed from the row.
                reason: match row.no_pixels_reason() {
                    Some(named) if reason == NO_PIXELS => named.to_string(),
                    _ => reason,
                },
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
                // One canvas piece: a mask's empty value is white and the
                // upload's clear leaves transparent black, so a mask may not go
                // sparse yet. `PixelPiece`'s rule 3.
                Ok(mask) => {
                    layer.mask = Some(vec![PixelPiece::whole(
                        canvas.size,
                        srgb::mask_buffer(&mask),
                    )])
                }
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
    // **Bounded by *area* against the canvas, not by each side.** A layer may
    // legitimately hang off the page, and the bound must still not be the
    // global ceiling: nothing else ties a bitmap to the document it is in, so a
    // 1×1 canvas with sixty-four layers passes `check_bounds` at 256 bytes
    // while each of those layers declares 16384² — a 64×64 grid, 4096 blocks,
    // 1.3 GB of inflate each, every byte of it thrown away by the blit. Worse,
    // nothing stops all of them naming the *same* external chunk, so one small
    // blob would be inflated a hundred and twenty-eight times.
    //
    // **It was twice the longer canvas edge, and that refused real documents.**
    // Five layers of one 3000² file came back as "Umber could not read the
    // shape of its bitmap" because Clip Studio had stored them 8448×11264 —
    // 3.75 times the edge. Measured over 5,438 bitmaps in 33 real documents,
    // eight exceed a 2× *side* bound and the worst is 3.75×.
    //
    // Area is the right quantity because area is what costs: the work is
    // `columns × rows` inflates, so a tall thin bitmap is cheap however far it
    // hangs off the page, and a side bound charges it as though it were square.
    // The worst real ratio is **15.93×**, and it is on the *smallest* canvas in
    // the folder — a 1200×480 banner holding a 2560×3584 layer — which is the
    // shape of the whole problem: the smaller the page, the further a layer
    // reaches beyond it in relative terms. So the cap is 32×, a little over
    // twice the worst seen, with a floor so that a very small canvas is not
    // squeezed by its own smallness.
    //
    // What the pathological case now costs: a 1×1 canvas is held to the floor,
    // 1024², which is 5 MB of inflate a layer rather than 1.3 GB.
    let canvas_area = u64::from(canvas.x) * u64::from(canvas.y);
    let area_bound = canvas_area
        .saturating_mul(BITMAP_AREA_SLACK)
        .max(BITMAP_AREA_FLOOR);
    let bitmap = csblocks::parse_attribute(attribute, BITMAP_MAX_SIDE)
        .filter(|b| u64::from(b.width) * u64::from(b.height) <= area_bound)
        .ok_or_else(|| "Umber could not read the shape of its bitmap".to_string())?;
    let packing = bitmap
        .packing
        .ok_or_else(|| "its bitmap does not say how its channels are packed".to_string())?;
    let data = container
        .external
        .get(chunk)
        .ok_or_else(|| NO_PIXELS.to_string())?;
    Ok((bitmap, packing, data))
}

/// A layer's colour, **one [`PixelPiece`] per stored block**, already in the
/// form a layer texture holds.
///
/// This is where the piece contract pays. A `.clip` states which of its
/// 256-squares it stores and Clip Studio stores only the ones the artist
/// touched: measured over 33 real documents, 13.5% of a dense store holds
/// paint, and the 20000×5000 document this was written for holds 6.5%. The
/// dense form of this function built a canvas-sized buffer per layer and
/// dropped the whole file's blocks into it, so a 124 MB file became 21.6 GB of
/// host memory and was then refused for being too big.
///
/// **The signal is block *presence*, not an emptiness scan.**
/// [`csblocks::for_each_block`] hands over the blocks the container says are
/// there, and a stored block that the artist later erased is carried anyway.
/// Measured, presence over-reports by 1.13× across the corpus and 1.58× on the
/// worst document — 13% of a small number, against a full extra pass over every
/// byte. `docs/perf/roadmap.md` §3.3 is where that trade is settled.
///
/// **A block absent from the file contributes nothing, and that is exactly the
/// old behaviour.** The dense buffer started as zeroes and `Fill::Stated` is
/// refused above, so an absent block was already four zeroes — and
/// `srgb::encode_pixel` of a transparent pixel is four zeroes too, since
/// `TABLE`'s alpha-0 row is all zeroes. So the canvas this leaves untouched is
/// byte for byte the canvas the dense version wrote.
fn colour(
    attribute: &[u8],
    chunk: &[u8],
    container: &Container<'_>,
    origin: (i64, i64),
    canvas: UVec2,
) -> Result<Vec<PixelPiece>, String> {
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

    let mut pieces = Vec::new();
    csblocks::for_each_block(
        data,
        packing,
        bitmap.columns * bitmap.rows,
        |index, block| {
            let (column, row) = (index % bitmap.columns, index / bitmap.columns);
            let Some(placed) = block_at(origin, (column, row), &bitmap, canvas) else {
                return;
            };
            let mut bytes = Vec::with_capacity(placed.rect.area() as usize * 4);
            for y in placed.ys.clone() {
                for x in placed.xs.clone() {
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
                    bytes.extend_from_slice(&srgb::encode_pixel([r, g, b, alpha]));
                }
            }
            pieces.push(PixelPiece::new(placed.rect, bytes));
        },
    )
    .ok_or_else(|| "its pixels could not be read".to_string())?;
    Ok(pieces)
}

/// Where one 256-square block lands, and which of its own texels go there.
///
/// The block's rectangle clipped twice — against the **bitmap**, because a
/// bitmap is padded out to whole blocks and the padding is not the artist's,
/// and against the **canvas**, because a layer may hang off the page. Getting
/// either wrong is a row of somebody else's texels along the edge of the
/// picture, which is why [`within`] and [`canvas_at`] exist and why this is
/// their rectangle form rather than a third reading.
struct BlockAt {
    rect: PixelRect,
    /// Within-block column indices that land on the canvas.
    xs: std::ops::Range<usize>,
    /// Within-block row indices that land on the canvas.
    ys: std::ops::Range<usize>,
}

fn block_at(
    origin: (i64, i64),
    block: (usize, usize),
    bitmap: &Bitmap,
    canvas: UVec2,
) -> Option<BlockAt> {
    let (x, xs) = block_span(origin.0, block.0, within(block.0, bitmap.width), canvas.x)?;
    let (y, ys) = block_span(origin.1, block.1, within(block.1, bitmap.height), canvas.y)?;
    Some(BlockAt {
        rect: PixelRect {
            x,
            y,
            width: (xs.end - xs.start) as u32,
            height: (ys.end - ys.start) as u32,
        },
        xs,
        ys,
    })
}

/// One axis of [`block_at`]: the canvas coordinate the run starts at, and the
/// within-block indices it covers.
///
/// `extent` is how much of this block is inside the bitmap — [`within`]'s
/// answer. Saturating rather than checked because an origin out of somebody
/// else's file can be anything at all, and the two clamps below already refuse
/// everything that does not land.
fn block_span(
    origin: i64,
    block: usize,
    extent: usize,
    canvas_extent: u32,
) -> Option<(u32, std::ops::Range<usize>)> {
    let base = origin.checked_add((block * BLOCK) as i64)?;
    let extent = extent as i64;
    let from = base.saturating_neg().clamp(0, extent);
    let to = i64::from(canvas_extent)
        .saturating_sub(base)
        .clamp(0, extent);
    if to <= from {
        return None;
    }
    // `base + from` is at least 0 by the clamp above and below `canvas_extent`
    // because `from < to <= canvas_extent - base`.
    Some(((base + from) as u32, from as usize..to as usize))
}

/// A mask's coverage, canvas-sized, as the **linear** multiplier every source
/// format states one in — which is what a mask slice holds, so
/// [`srgb::mask_buffer`] only widens it to four channels and the caller is what
/// applies that.
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

    /// `read` with no bar attached, which is what every test here wants.
    ///
    /// Shadows the module's own inside this scope, so the progress callback is
    /// stated once rather than at each of the several dozen call sites — none
    /// of which is about progress.
    fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
        super::read(bytes, &|_, _| {})
    }
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

    /// **A layer whose bitmap hangs well off the page still arrives.**
    ///
    /// The bound used to be twice the longer canvas edge, and five layers of
    /// one real 3000² document were refused because Clip Studio had stored
    /// them 8448×11264 — 3.75 times the edge, and a perfectly ordinary raster
    /// layer. The artist saw "Umber could not read the shape of its bitmap",
    /// which reads as a damaged file, and lost the layers.
    ///
    /// Driven at the real figures rather than at a round number: 8448×11264 on
    /// 3000², which is 10.57× by area and the second worst of 5,438 bitmaps
    /// measured across 33 documents.
    #[test]
    fn a_layer_reaching_far_past_the_canvas_still_arrives() {
        // `placed` is what puts a bitmap bigger than the canvas in the file,
        // which is exactly what a layer dragged off the page produces.
        //
        // **Sized past the floor as well as past the ratio**, or this measures
        // the wrong bound. The first version was 845×1126 on 300², which is
        // 10.57× by area — the real document's own figure — and was admitted by
        // `BITMAP_AREA_FLOOR` whatever the slack was: cutting the slack to a
        // quarter left it green. 1600×2134 on 600² is 9.5× *and* three times
        // the floor, so only the ratio can let it through.
        let bytes = fixtures::clip(
            600,
            600,
            &[ClipLayer::flat("Wide", 1600, 2134, [10, 20, 30, 255])
                .placed((1600, 2134), (-100, -100))],
        );
        let doc = read(&bytes).expect("a document");
        assert_eq!(doc.layers.len(), 1, "warnings: {:?}", doc.warnings);
        assert!(
            doc.warnings.is_empty(),
            "a layer hanging off the page is not a loss: {:?}",
            doc.warnings
        );
    }

    /// The bound is still a bound: a bitmap far past what any real document
    /// carries is refused rather than inflated.
    ///
    /// The case it exists for is a *tiny* canvas — `check_bounds` lets a 1×1
    /// document through at 256 bytes, and without this each of its layers could
    /// declare 16384² and cost 1.3 GB of inflate apiece.
    #[test]
    fn a_bitmap_far_larger_than_the_canvas_is_still_refused() {
        // 4096² against the 1024² floor: sixteen times the area a very small
        // canvas is allowed, so the floor rather than the ratio is what refuses
        // it.
        let bytes = fixtures::clip(
            8,
            8,
            &[ClipLayer::flat("Huge", 4096, 4096, [1, 2, 3, 255]).placed((4096, 4096), (0, 0))],
        );
        let doc = read(&bytes);
        // Refused as a document with nothing in it, or opened having said what
        // it lost — either is a refusal somebody can see. What must not happen
        // is the bitmap arriving quietly.
        if let Ok(opened) = doc {
            assert!(
                opened.layers.is_empty() || !opened.warnings.is_empty(),
                "an absurd bitmap must not arrive silently"
            );
        }
    }

    /// **A vector layer is named as one, not reported as a file with no
    /// pixels.**
    ///
    /// Clip Studio stores it as strokes and rasterises on demand, so there is
    /// no bitmap at any mipmap level — the document is intact and Umber simply
    /// has no vector renderer. The old wording sent an artist looking for a
    /// corrupt file; the new one sends them to Layer → Rasterize, which is the
    /// thing that actually works.
    ///
    /// The fixture is a layer with `LayerType` 0 and no stored pixels, which is
    /// what a real one looks like — 28 of the 542 painted layers across the 33
    /// documents this was measured against.
    #[test]
    fn a_vector_layer_is_named_rather_than_reported_as_missing_pixels() {
        let bytes = fixtures::clip(
            64,
            64,
            &[
                ClipLayer::flat("Ink", 64, 64, [255, 0, 0, 255]),
                ClipLayer::vector("Lines", 64, 64),
            ],
        );
        let doc = read(&bytes).expect("a document");

        let said: Vec<String> = doc.warnings.iter().map(|w| w.to_string()).collect();
        let about = said
            .iter()
            .find(|s| s.contains("Lines"))
            .unwrap_or_else(|| panic!("nothing said about the vector layer: {said:?}"));
        assert!(about.contains("vector layer"), "{about}");
        assert!(
            about.contains("rasterise") || about.contains("Rasterize"),
            "the sentence has to say what to do about it: {about}"
        );
        assert!(
            !about.contains("does not hold its pixels"),
            "that wording reads as a damaged file: {about}"
        );

        // The rest of the document is untouched by it.
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Ink");
    }

    /// **A placed image is not a vector layer**, and calling it one told an
    /// artist something false about their own document.
    ///
    /// Both carry `LayerType` 0 and neither has a rendered bitmap, so the type
    /// alone cannot tell them apart — and the reader read the type alone. Over
    /// the 33 real documents, 5 of the 28 layers it called vector are placed
    /// images, and 4 of those 5 are every painted layer of one file.
    ///
    /// The distinguishing evidence is `ResizableOriginalMipmap`, and the
    /// fixture writes the real shape: a full render chain with its chunk
    /// withheld, beside a resizable-original chain whose pixels are present.
    #[test]
    fn a_placed_image_is_named_as_one_rather_than_as_a_vector_layer() {
        let bytes = fixtures::clip(
            64,
            64,
            &[
                ClipLayer::flat("Ink", 64, 64, [255, 0, 0, 255]),
                ClipLayer::placed_image("Photo", 64, 64),
            ],
        );
        let doc = read(&bytes).expect("a document");

        let said: Vec<String> = doc.warnings.iter().map(|w| w.to_string()).collect();
        let about = said
            .iter()
            .find(|s| s.contains("Photo"))
            .unwrap_or_else(|| panic!("nothing said about the placed image: {said:?}"));
        assert!(
            about.contains("placed into the document"),
            "it has to name the cause it actually met: {about}"
        );
        assert!(
            !about.contains("vector layer"),
            "this is the false diagnosis the column exists to prevent: {about}"
        );
        assert!(
            about.contains("Rasterize"),
            "the sentence has to say what to do about it: {about}"
        );
        assert!(
            !about.contains("does not hold its pixels"),
            "that wording reads as a damaged file: {about}"
        );

        // And nothing about the rest of the document moved.
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Ink");
    }

    /// **A document whose every layer was refused says why, and does not read
    /// as a damaged file.**
    ///
    /// This is the artist's own document: an A4 page holding four images
    /// placed on it and nothing else. Every layer is dropped for a reason the
    /// reader knows and states, and the reasons live on
    /// [`ImportedDocument::warnings`] — which never reaches the caller, because
    /// there is no document to carry them. What the artist saw was "The Clip
    /// Studio Paint file contains no layers", of a file that is perfectly
    /// intact.
    ///
    /// The Paper sheet is in the fixture deliberately: it is taken out before
    /// the count, so a document of paper-plus-refusals is exactly the shape
    /// that reaches this refusal in the real file.
    #[test]
    fn a_document_of_nothing_but_refused_layers_says_why_it_was_refused() {
        let bytes = fixtures::clip(
            64,
            64,
            &[
                ClipLayer::paper([255, 255, 255]),
                ClipLayer::placed_image("Illustration", 64, 64),
                ClipLayer::placed_image("Illustration 2", 64, 64),
                ClipLayer::vector("Lines", 64, 64),
            ],
        );
        let err = read(&bytes).expect_err("nothing paintable in it");
        let said = err.to_string();

        assert!(
            !said.contains("contains no layers"),
            "that is the sentence that reads as a corrupt file: {said}"
        );
        assert!(
            said.contains("Umber read this Clip Studio Paint file"),
            "saying the file was read is what stops it reading as damage: {said}"
        );
        // **And it must not go further than that.** Some reasons a layer is
        // refused do mean the file may be damaged — "Umber could not read the
        // shape of its bitmap" is one, and `openraster`'s "it names no image
        // file" is another whose own comment calls the file malformed — and
        // this heading is shared by every one of them, so a claim that the
        // document is intact would be false in exactly the case an artist most
        // needs the truth.
        assert!(
            !said.contains("not damaged") && !said.contains("intact"),
            "the heading may not promise something its reasons can contradict: {said}"
        );
        // Both causes, counted, and each said once rather than once per layer.
        // The count trails the reason so that a plural tally does not sit in
        // front of a clause written with a singular subject.
        assert!(
            said.contains("It is an image placed into the document"),
            "the reason stands as its own sentence: {said}"
        );
        assert!(
            said.contains("(2 layers.)"),
            "the two placed images are one sentence with a count: {said}"
        );
        assert!(
            said.contains("It is a vector layer"),
            "and the vector layer keeps its own cause: {said}"
        );
        assert!(
            said.contains("(one layer.)"),
            "one is spelled out, as it is everywhere else: {said}"
        );
        assert!(
            !said.contains("layers: it is") && !said.contains("layer: it is"),
            "a plural tally must not head a singular clause: {said}"
        );
        assert!(
            said.contains("Rasterize"),
            "every cause here has a remedy and it has to survive: {said}"
        );
        // The Paper sheet became the background, so it is not a refused layer
        // and must not be named as one.
        assert!(
            !said.to_lowercase().contains("paper"),
            "the background is not a layer that was refused: {said}"
        );
    }

    /// **The reader's *first* drop site names a cause too, and nothing reached
    /// it.**
    ///
    /// A layer whose bitmap is missing can fail in two places: naming no render
    /// mipmap at all, or naming one whose external chunk is absent. Every
    /// fixture in this file took the second route, because they all write a
    /// chain and withhold only the chunk — which is right, and left the first
    /// site covered by nothing. Demonstrated by mutation: replacing that site's
    /// whole sentence with a marker string left all 1,120 tests green.
    ///
    /// Both kinds are driven, because the two arms of `no_pixels_reason` are
    /// what the site has to route between and a fixture carrying one of them
    /// would test the arm it happened to pick.
    #[test]
    fn a_layer_that_names_no_rendered_bitmap_still_names_its_own_cause() {
        // `kind` 0 with no resizable original is a vector layer; a placed image
        // cannot reach this site, since it is `flat`-derived and so always has
        // a chain.
        let bytes = fixtures::clip(
            8,
            8,
            &[
                ClipLayer::flat("Ink", 8, 8, [255, 0, 0, 255]),
                ClipLayer::no_render_bitmap("Lines", VECTOR_KIND),
                // `kind` 1 is an ordinary raster layer, which has no cause this
                // reader can name — so it must fall back rather than borrow
                // somebody else's sentence.
                ClipLayer::no_render_bitmap("Blank", LAYER_IS_PIXEL),
            ],
        );
        let doc = read(&bytes).expect("a document");
        let said: Vec<String> = doc.warnings.iter().map(ToString::to_string).collect();

        let lines = said
            .iter()
            .find(|s| s.contains("Lines"))
            .unwrap_or_else(|| panic!("nothing said about it: {said:?}"));
        assert!(
            lines.contains("vector layer"),
            "the first site has to name the cause the second one does: {lines}"
        );
        let blank = said
            .iter()
            .find(|s| s.contains("Blank"))
            .unwrap_or_else(|| panic!("nothing said about it: {said:?}"));
        assert!(
            blank.contains(NO_PIXELS),
            "and fall back where there is no cause to name: {blank}"
        );
    }

    /// A refusal with nothing to explain reads exactly as it always did.
    ///
    /// The `because` list is an addition, not a replacement: a document that is
    /// genuinely empty has no reasons to give, and inventing a heading over an
    /// empty list would be a message about nothing.
    #[test]
    fn a_document_refused_with_no_reasons_keeps_the_plain_sentence() {
        let err = ImportError::Empty {
            format: FORMAT,
            because: Vec::new(),
        };
        assert_eq!(
            err.to_string(),
            "The Clip Studio Paint file contains no layers."
        );
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
        let pixels = doc.layers[0].dense(UVec2::new(w, h));
        assert_eq!(pixels.len(), (w * h * 4) as usize);
        for (i, px) in pixels.as_chunks::<4>().0.iter().enumerate() {
            assert_eq!(*px, [10, 120, 240, 255], "pixel {i}");
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
        let pixels = doc.layers[0].dense(UVec2::new(300, 300));
        assert!((i32::from(pixels[0]) - 188).abs() <= 1);
        assert_eq!(pixels[3], 128);
    }

    /// **The reader yields the blocks the file holds and nothing else**, which
    /// is the whole of Stage 2 on the format that motivated it.
    ///
    /// Three claims, and each would be a separate way to get this wrong:
    ///
    /// - the *picture* is unchanged, which is the one that must never move: the
    ///   assembled canvas is exactly what a dense reader would have produced,
    ///   built here from the fixture's own colour rather than from the reader's
    ///   output, so it agrees with nothing but the file;
    /// - the pieces obey rules 1 and 2 — inside the canvas, not overlapping;
    /// - and it is genuinely **sparse**. That last is the one a version that
    ///   quietly went back to one canvas piece would fail, and it is stated as a
    ///   fraction of the canvas rather than as a piece count, because the piece
    ///   count is a property of the block grid and the fraction is the thing
    ///   anybody cares about.
    #[test]
    fn a_layer_yields_only_the_blocks_the_file_stores() {
        // A 300-square bitmap in the corner of a 1024-square canvas: 2×2 blocks
        // of the nine a dense layer would occupy, and 8.6% of the page.
        let canvas = UVec2::new(1024, 1024);
        let bytes = fixtures::clip(
            canvas.x,
            canvas.y,
            &[ClipLayer::flat("Ink", 300, 300, [10, 120, 240, 255]).placed((300, 300), (0, 0))],
        );
        let doc = read(&bytes).expect("a document");
        let layer = &doc.layers[0];

        crate::docimport::check_piece_rules(&layer.pixels, canvas);

        // What a dense reader would have written, built from the fixture rather
        // than from the reader.
        let mut expected = vec![0u8; (canvas.x * canvas.y * 4) as usize];
        for y in 0..300usize {
            for x in 0..300usize {
                let px = (y * canvas.x as usize + x) * 4;
                expected[px..px + 4].copy_from_slice(&[10, 120, 240, 255]);
            }
        }
        assert_eq!(layer.dense(canvas), expected, "the picture moved");

        // And it did not cost a canvas to say so. Four 256-squares clipped to
        // the bitmap is 300×300 pixels; a dense reader spent 1024×1024.
        assert_eq!(layer.pixel_bytes(), 300 * 300 * 4);
        assert!(
            layer.pixel_bytes() * 10 < u64::from(canvas.x) * u64::from(canvas.y) * 4,
            "a layer covering 8.6% of the page must not be charged the page: {} bytes",
            layer.pixel_bytes()
        );
    }

    /// A bitmap's origin is a number out of somebody else's file, and the
    /// arithmetic that places its blocks must not panic on any of them.
    ///
    /// `-i64::MIN` panics in a debug build and `canvas - origin` overflows for
    /// a very negative one, which is why [`block_span`] is checked and
    /// saturating throughout. No real `.clip` reaches these values, which is
    /// exactly why they need a test rather than a reader.
    #[test]
    fn a_block_placed_absurdly_far_off_the_page_lands_nowhere() {
        for origin in [i64::MIN, i64::MAX, -256, 300, i64::MIN + 1] {
            assert!(
                block_span(origin, 0, BLOCK, 256).is_none(),
                "a block at {origin} does not reach a 256-wide canvas"
            );
        }
        // And one that does land, so the sweep is not passing by refusing
        // everything: half a block hanging off the left edge.
        let (at, within) = block_span(-128, 0, BLOCK, 256).expect("half a block lands");
        assert_eq!((at, within), (0, 128..256));
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
        // Under the piece contract the reader also has the option of saying
        // nothing at all about an unstored block, and that has to mean the same
        // thing: `dense` is what holds the two readings together.
        assert!(
            doc.layers[0]
                .dense(UVec2::new(300, 300))
                .iter()
                .all(|b| *b == 0)
        );
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
        let cases: [(i64, BlendMode, bool); 6] = [
            (0, BlendMode::Normal, false),
            (2, BlendMode::Multiply, false),
            (8, BlendMode::Screen, false),
            // Darken and Difference are Umber's own now, so neither is a loss
            // and neither may be reported. They were Multiply-with-a-warning
            // and Normal-with-a-warning, which is what this used to pin.
            (1, BlendMode::Darken, false),
            (21, BlendMode::Difference, false),
            // Hard Mix has no formula here and is still named as a loss, so
            // this case keeps a mapping that *is* approximate under test.
            (20, BlendMode::VividLight, true),
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

    /// A mask's byte is a **linear** multiplier and a mask slice now holds one,
    /// so it is copied across unchanged.
    ///
    /// This guard used to assert the opposite — that a half arrived as ~188,
    /// because a mask slice was read through the layer array's sRGB view. That
    /// encode was inherited rather than chosen and it was not injective: 73 of
    /// its 256 states collided. **The direction is the whole of what this
    /// catches**: put the encode back and every Clip Studio mask arrives
    /// revealing more than its author set.
    ///
    /// Driven at three values rather than one, and none of them a fixed point of
    /// the transfer function — 0 and 255 survive either rule, which is why a
    /// fixture reaching for the ends can see nothing here at all.
    #[test]
    fn a_masks_coverage_arrives_as_the_multiplier_it_was() {
        let (w, h) = (300u32, 300u32);
        for c in [1u8, 128, 254] {
            let bytes = fixtures::clip(
                w,
                h,
                &[ClipLayer::flat("Ink", w, h, [0, 0, 0, 255]).mask(vec![c; (w * h) as usize])],
            );
            let doc = read(&bytes).expect("a document");
            let mask = doc.layers[0].dense_mask(UVec2::new(w, h)).expect("a mask");
            assert_eq!(mask.len(), (w * h * 4) as usize);
            assert_eq!(mask[0], c, "coverage {c} did not arrive as itself");
            assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        }
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
            let mask = doc.layers[0].dense_mask(UVec2::new(w, h)).expect("a mask");
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
        let pixels = layer.dense(UVec2::new(w, h));
        let mask = layer.dense_mask(UVec2::new(w, h)).expect("a mask");
        let at = |x: usize, y: usize| {
            let i = (y * w as usize + x) * 4;
            (&pixels[i..i + 4], mask[i])
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
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(300, 300))[0..4],
            &[9, 9, 9, 255]
        );
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
                resizable_original: 0,
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
        let nodes = tables
            .tree(1, LayerStack::MAX, &mut warnings)
            .expect("a bounded walk");
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
            tables.tree(1, LayerStack::MAX, &mut warnings),
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
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(300, 300))[0..4],
            &[7, 8, 9, 255]
        );
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
            tables.tree(1, LayerStack::MAX, &mut warnings),
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
            resizable_original: 0,
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

    /// **A stored block is not a block that holds anything, and the two
    /// readings have to say so.**
    ///
    /// Clip Studio writes a block where the artist *touched* the canvas, not
    /// where paint survived, so `Reading::Presence` is an upper bound on what a
    /// tiled store would keep. If that bound is loose the whole tiling argument
    /// moves with it, which is why both figures exist and why this drives them
    /// against a case where they **disagree** — a test that only saw a painted
    /// layer would pass under either rule.
    ///
    /// The fixture is a 300-square transparent layer. Its one block wholly
    /// inside the bitmap is elided as uniform, exactly as Clip Studio elides
    /// one; the other three overhang the bitmap's edge, so they are *stored*,
    /// padded with the fixture's deliberate `0x5a`, and hold not one visible
    /// texel. Presence charges three tiles for them and contents charges none.
    #[test]
    fn a_block_the_file_stores_may_still_hold_nothing_and_the_two_readings_differ() {
        use super::super::residency::Reading;

        let bytes = fixtures::clip(
            300,
            300,
            &[
                ClipLayer::flat("Blank", 300, 300, [0, 0, 0, 0]),
                ClipLayer::flat("Ink", 300, 300, [9, 9, 9, 255]),
            ],
        );

        let cheap = residency(&bytes, Reading::Presence).expect("a survey");
        assert_eq!(cheap.canvas_tiles(), 4);
        let blank = cheap.slices.iter().find(|s| s.layer == "Blank").unwrap();
        assert_eq!((blank.stored, blank.covered), (3, 3));
        // Not "the same as stored": absent, so nobody can quote a figure that
        // was never measured.
        assert_eq!((blank.live, blank.live_covered), (None, None));

        let full = residency(&bytes, Reading::Contents).expect("a survey");
        let blank = full.slices.iter().find(|s| s.layer == "Blank").unwrap();
        assert_eq!((blank.stored, blank.covered), (3, 3));
        assert_eq!(
            (blank.live, blank.live_covered),
            (Some(0), Some(0)),
            "the padding past a bitmap's edge is nobody's picture"
        );

        // The painted layer is the control: every block holds paint, so the two
        // readings agree and the difference above is about content rather than
        // about the scan being broken.
        let ink = full.slices.iter().find(|s| s.layer == "Ink").unwrap();
        assert_eq!((ink.stored, ink.covered), (4, 4));
        assert_eq!((ink.live, ink.live_covered), (Some(4), Some(4)));

        // And it composes: half the document's slices are empty.
        assert_eq!(full.occupancy(), Some(7.0 / 8.0));
        assert_eq!(full.live_occupancy(), Some(0.5));
    }

    /// **Blank is measured against the fill, and a mask's fill is all-ones.**
    ///
    /// A Clip Studio mask begins revealing everything, so a mask block of 255
    /// is exactly as redundant as a layer block of 0 — a tiled store answers it
    /// with a default and backs no tile. Testing both against zero is the
    /// plausible simplification and it reports every full-reveal mask tile as
    /// live, which on a document of masked layers is a residency figure well
    /// over the truth.
    ///
    /// This is the case that makes the fill rule non-vacuous, and it was
    /// demonstrated by mutation rather than argued: the layer in the test above
    /// has a fill of zero, so a change to a bare `!= 0` walks straight through
    /// it and fails only here.
    #[test]
    fn a_mask_is_blank_where_it_reveals_everything_rather_than_where_it_is_zero() {
        use super::super::residency::Reading;

        let bytes = fixtures::clip(
            300,
            300,
            &[
                ClipLayer::flat("Ink", 300, 300, [9, 9, 9, 255]).mask(vec![255u8; 300 * 300]),
                ClipLayer::flat("Hidden", 300, 300, [9, 9, 9, 255]).mask(vec![0u8; 300 * 300]),
            ],
        );
        let full = residency(&bytes, Reading::Contents).expect("a survey");

        let masks: Vec<_> = full.slices.iter().filter(|s| s.mask).collect();
        assert_eq!(masks.len(), 2, "both masks were measured");
        for mask in &masks {
            assert_eq!(mask.fill, "stated", "a Clip Studio mask states its fill");
            assert!(mask.stored > 0, "the overhanging blocks are stored");
        }
        // The revealing mask is the fill everywhere, so nothing needs backing.
        let reveal = masks.iter().find(|s| s.layer == "Ink").unwrap();
        assert_eq!(reveal.live, Some(0));
        // The concealing one differs from the fill everywhere, so all of it
        // does — and it is the half of the pair a bare `!= 0` gets *wrong the
        // other way*, which is what makes the pair rather than one case.
        let conceal = masks.iter().find(|s| s.layer == "Hidden").unwrap();
        assert_eq!(conceal.live, Some(conceal.stored));
    }
}
