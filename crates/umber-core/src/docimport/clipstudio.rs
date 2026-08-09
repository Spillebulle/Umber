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
//! and is where to look first if an imported layer lands in the wrong place.

use std::collections::HashMap;

use glam::UVec2;

use super::blend::{self, Fidelity};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, SourceFormat, check_bounds, srgb,
};
use crate::csblocks::{self, BLOCK, Bitmap, Blocks, Fill, Packing};
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

/// `LayerVisibility`'s bit 0. The others are the mask's own eye and the ruler's.
const VISIBLE: i64 = 1;

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
    let nodes = layers.tree(canvas.root, &mut warnings)?;
    check_bounds(FORMAT, canvas.size.x, canvas.size.y, nodes.len())?;

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
        background: Background::Transparent,
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
        dpi: get("CanvasResolution")
            .and_then(Value::as_f64)
            .map(|v| v as f32)
            .filter(|v| *v > 0.0),
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
    fn chain(
        &self,
        first: i64,
        depth: usize,
        seen: &mut std::collections::HashSet<i64>,
        out: &mut Vec<Node>,
        warnings: &mut Vec<ImportWarning>,
    ) -> Result<(), ImportError> {
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
                if depth > LayerStack::MAX_DEPTH as usize {
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
        let mut folder = ImportedLayer::folder(name, node.depth, row.visible);
        folder.locked = row.locked;
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
    if row.kind & LAYER_IS_PIXEL == 0 {
        warnings.push(ImportWarning::LayerRasterised {
            layer: name.clone(),
            what: format!("a Clip Studio layer of type {}", row.kind),
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

    if row.mask_mipmap != 0 {
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

/// Read the bitmap an `Offscreen` names, whatever it is going to be used for.
///
/// Returns the blocks in grid order beside what the `Attribute` said, or a
/// sentence saying why not — which becomes the reason a layer or a mask was
/// refused. Nothing here allocates from a figure the file chose alone.
fn read_bitmap(
    attribute: &[u8],
    chunk: &[u8],
    container: &Container<'_>,
) -> Result<(Bitmap, Packing, Blocks), String> {
    let bitmap = csblocks::parse_attribute(attribute, ImportedDocument::MAX_DIMENSION)
        .ok_or_else(|| "Umber could not read the shape of its bitmap".to_string())?;
    let packing = bitmap
        .packing
        .ok_or_else(|| "its bitmap does not say how its channels are packed".to_string())?;
    let data = container
        .external
        .get(chunk)
        .ok_or_else(|| "the file does not hold its pixels".to_string())?;
    let blocks = csblocks::blocks(data, packing)
        .ok_or_else(|| "its pixels could not be decompressed".to_string())?;
    if blocks.len() != bitmap.columns * bitmap.rows {
        return Err("its bitmap holds a different number of blocks than it claims".to_string());
    }
    Ok((bitmap, packing, blocks))
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
    let (bitmap, packing, blocks) = read_bitmap(attribute, chunk, container)?;
    if packing.first != 1 || !matches!(packing.second, 1 | 4) {
        return Err(format!(
            "its pixels are packed {} and {} channels at a time, which Umber cannot read",
            packing.first, packing.second
        ));
    }
    if bitmap.fill != Fill::Empty {
        return Err(
            "it is filled with a colour Clip Studio states in a form Umber cannot read".to_string(),
        );
    }
    if !overlaps(&bitmap, origin, canvas) {
        return Err("it lies entirely outside the canvas".to_string());
    }

    let mut out = vec![0u8; canvas.x as usize * canvas.y as usize * 4];
    for (index, block) in blocks.iter().enumerate() {
        let Some(block) = block else { continue };
        let (column, row) = (index % bitmap.columns, index / bitmap.columns);
        for y in 0..BLOCK {
            let Some(dst_y) = canvas_at(origin.1, row, y, canvas.y) else {
                continue;
            };
            for x in 0..BLOCK {
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
    }
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
    let (bitmap, packing, blocks) = read_bitmap(attribute, chunk, container)?;
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
    for (index, block) in blocks.iter().enumerate() {
        let Some(block) = block else { continue };
        let (column, row) = (index % bitmap.columns, index / bitmap.columns);
        for y in 0..BLOCK {
            let Some(dst_y) = canvas_at(origin.1, row, y, canvas.y) else {
                continue;
            };
            for x in 0..BLOCK {
                let Some(dst_x) = canvas_at(origin.0, column, x, canvas.x) else {
                    continue;
                };
                out[dst_y * canvas.x as usize + dst_x] = block[y * BLOCK + x];
            }
        }
    }
    Ok(out)
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
    /// survives. A colour is chosen whose three channels differ, because
    /// `[alpha][B G R X]` and `[alpha][R G B X]` cannot be told apart by grey.
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
    #[test]
    fn what_an_unstored_mask_block_holds_follows_the_file() {
        let (w, h) = (300u32, 300u32);
        let corner = |fill: u8| {
            // Everything the stated fill, except one patch — so every block but
            // the first is omitted by the fixture and has to be filled in.
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
            mask[(290 * w as usize + 290) * 4]
        };

        assert_eq!(corner(255), 255, "an unstored block of a revealing mask");
        assert_eq!(corner(0), 0, "an unstored block of a hiding mask");
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
