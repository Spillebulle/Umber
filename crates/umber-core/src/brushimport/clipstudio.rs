//! Clip Studio Paint sub-tools: `.sut` and `.sutg`.
//!
//! # What the file actually is
//!
//! A `.sut` is an **ordinary SQLite database**, and so is a `.sutg`: the same
//! four tables either way, which is why one reader serves both.
//!
//! | Table | Holds |
//! |---|---|
//! | `Manager` | the file's version, and the uuid of the root node |
//! | `Node` | one row per sub-tool: its name, its uuid, and the links |
//! | `Variant` | one row per settings block, keyed by `VariantID` |
//! | `MaterialFile` | the bitmap materials, as a blob per material |
//!
//! The nodes are a **linked list**, not a table order: `Manager.RootUuid` names
//! the first, `NodeFirstChildUuid` descends and `NodeNextUuid` chains. A `.sut`
//! is exactly the degenerate case — one node, no children — which is what makes
//! the group format free once the single one works. `Node.NodeVariantID` points
//! at the settings; the *other* variant each node names (`NodeInitVariantID`)
//! is the "reset to default" copy and holds almost nothing, so it must not be
//! read by mistake.
//!
//! **The schema is not fixed and columns must be looked up by name.** Sample
//! files here declare 187 columns in `Variant` for a lone brush and 214 for a
//! group that happens to contain a fill tool, interleaved rather than appended.
//! An importer that counted columns would read a brush's rotation out of its
//! neighbour's fill tolerance.
//!
//! # The bitmap materials
//!
//! `MaterialFile.FileData` is a **USTAR tar archive**, and the members that
//! matter are:
//!
//! ```text
//! catalog.zip               a C2F chunk stream, compressed
//! info.zip                  the same
//! data/material_0.layer     the material's full-resolution pixels
//! thumbnail/thumbnail.png   an ordinary PNG, longest side up to 300
//! icedata/layerData.xml     what kind of material it is
//! ```
//!
//! Despite the names, nothing in there is a zip. `catalog.zip` and
//! `material_0.layer` are Clip Studio's own container — magic
//! `\x89C2F\r\n\x1a\n`, then chunks of `[u32 le size][4-byte tag][payload]
//! [u32 checksum]` tagged `HEAD`, `dATA` and `TAIL` — and a `dATA` payload
//! begins with a two-byte flag that is `1` when what follows is compressed by
//! a codec that is Clip Studio's and is documented nowhere.
//!
//! **So the tip comes from `thumbnail.png`.** That is a real PNG of the real
//! material and needs no guesswork, at the cost of a longest side of 300 —
//! which is a limit only for a material that was bigger, and is well inside
//! what [`TipMask`] stamps. Reversing the codec to gain resolution on a stamp
//! that is then scaled to the dab anyway is not a trade worth the risk of
//! getting somebody's brush subtly wrong, which is the thing `CLAUDE.md` says
//! an import must never do. It is named as a loss all the same.
//!
//! A material reference — `TextureImage`, `BrushPatternImageArray` — is a small
//! blob whose first field is the `OriginalPath` of the row in `MaterialFile`
//! that holds it, so a tip resolves by exact string match and never by
//! searching.
//!
//! # What is dropped
//!
//! The list is in [`Dropped`]. The ones worth stating here are the shape of the
//! whole thing rather than a detail:
//!
//! - **Clip Studio's per-setting "effect sources" are a table Umber does not
//!   have.** Every one of them carries a bitmask of which inputs drive it, a
//!   minimum per input, and a response curve. Pen pressure and the per-dab
//!   random draw are read; pen tilt, pen bearing and stroke speed are named and
//!   dropped, because which bit is which cannot be told apart from the files to
//!   hand and a modulation wired to the wrong input is worse than none.
//! - **The paper texture becomes one of Umber's three.** Umber's grain is a
//!   closed set (see [`crate::GrainPattern`]), so the strength and the tile
//!   size carry across and the picture does not.
//! - **Dual brushes, watercolour edges, the taper, colour jitter and the vector
//!   settings have no engine behind them at all** and are named.
//!
//! Nothing here is refused for being approximate: this is a user's own import,
//! which `CLAUDE.md` holds to a different standard than the shipped library —
//! a usable approximation that says what it lost beats a rejection.

use crate::brush::{Brush, GrainPattern};
use crate::curve::ResponseCurve;
use crate::preset::PresetError;
use crate::sqlite::{Database, Row, Table, Value};
use crate::tip::{TipMask, stroke_coverage};

/// Every loss this importer can report, in one place.
///
/// Constants rather than literals at the call site because several are pushed
/// from more than one place, and because a list of what an importer knows it
/// cannot do is worth being able to read in one screen.
pub mod dropped {
    /// The tip is the material's thumbnail, not its full-resolution pixels.
    pub const THUMBNAIL_TIP: &str = "bitmap tips at their full resolution";
    pub const SEVERAL_TIPS: &str = "brushes that cycle through several tip images";
    pub const PAPER_TEXTURE: &str = "the paper texture's own picture";
    pub const OTHER_INPUTS: &str = "settings driven by pen tilt, pen bearing or stroke speed";
    pub const MIXING: &str = "the detail of Clip Studio's underlying-colour mixing";
    pub const DUAL_BRUSH: &str = "dual brushes";
    pub const WATER_EDGE: &str = "watercolour edges";
    pub const TAPER: &str = "stroke taper (in and out)";
    pub const COLOUR_JITTER: &str = "per-dab hue, saturation and brightness shifts";
    pub const RIBBON: &str = "ribbon and continuous-image strokes";
    pub const BLEND_MODE: &str = "a blending mode set on the brush itself";
    pub const SPRAY_SHAPE: &str = "the spray's particle count and bias";
    pub const NOT_A_BRUSH: &str = "sub-tools that are not brushes";
    pub const VECTOR: &str = "the vector-layer settings";
    /// The material was there and its thumbnail could not be turned into a
    /// mask — a picture with no dark pixels in it paints nothing.
    pub const UNUSABLE_TIP: &str = "a bitmap tip that would have painted nothing";
}

/// One sub-tool, converted.
#[derive(Debug)]
pub struct SubTool {
    pub name: String,
    pub brush: Brush,
    pub tip: Option<TipMask>,
    /// What this particular sub-tool lost.
    pub dropped: Vec<&'static str>,
}

/// Everything readable in one file.
#[derive(Debug)]
pub struct SutFile {
    pub tools: Vec<SubTool>,
    /// Losses that belong to the file rather than to one brush — a fill tool
    /// that was skipped is not any brush's problem.
    pub dropped: Vec<&'static str>,
}

/// Read a `.sut` or a `.sutg`.
pub fn from_sut(bytes: &[u8]) -> Result<SutFile, PresetError> {
    let db = Database::open(bytes).map_err(|e| malformed(e.to_string()))?;

    let (nodes, node_table) = read_table(&db, "Node")?;
    let (variants, variant_table) = read_table(&db, "Variant")?;
    // Absent in a file whose brushes are all procedural, which is legal and
    // simply means no brush in it has a bitmap tip.
    let materials = match db
        .table("MaterialFile")
        .map_err(|e| malformed(e.to_string()))?
    {
        Some(table) => {
            let rows = db.rows(&table).map_err(|e| malformed(e.to_string()))?;
            Materials::new(&table, rows)
        }
        None => Materials::default(),
    };

    let mut file = SutFile {
        tools: Vec::new(),
        dropped: Vec::new(),
    };

    for index in node_order(&db, &node_table, &nodes) {
        let node = &nodes[index];
        let Some(settings) = variant_for(&node_table, node, &variant_table, &variants) else {
            continue;
        };
        // A sub-tool with no brush size is not a brush: the fill, selection and
        // shape tools share these tables and leave every brush column null.
        // Skipping them by the data rather than by a tool-type number means a
        // tool this build has never heard of is skipped for the right reason.
        if settings.real("BrushSize").is_none() {
            push_once(&mut file.dropped, dropped::NOT_A_BRUSH);
            continue;
        }

        let name = node_name(&node_table, node);
        let (brush, tip, dropped) = convert(&settings, &materials);
        file.tools.push(SubTool {
            name,
            brush,
            tip,
            dropped,
        });
    }

    if file.tools.is_empty() {
        return Err(malformed(
            "it holds no brushes — every sub-tool in it is a fill, selection or shape tool"
                .to_string(),
        ));
    }
    Ok(file)
}

/// Everything reading this file will throw away, named, without keeping the
/// brushes.
///
/// Best-effort like every other importer's: an unreadable file answers with
/// nothing here and fails properly in [`from_sut`], so a file is never reported
/// on twice.
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    let Ok(file) = from_sut(bytes) else {
        return Vec::new();
    };
    let mut out = file.dropped;
    for tool in file.tools {
        for loss in tool.dropped {
            push_once(&mut out, loss);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Walking the file
// ---------------------------------------------------------------------------

fn read_table(db: &Database, name: &str) -> Result<(Vec<Row>, Table), PresetError> {
    let table = db
        .table(name)
        .map_err(|e| malformed(e.to_string()))?
        .ok_or_else(|| malformed(format!("it has no `{name}` table")))?;
    let rows = db.rows(&table).map_err(|e| malformed(e.to_string()))?;
    Ok((rows, table))
}

/// The nodes in the order the tool palette shows them.
///
/// Follows `Manager.RootUuid` down `NodeFirstChildUuid` and along
/// `NodeNextUuid`. Falls back to the table's own order whenever the chain does
/// not reach everything — a hand-edited or half-written file must still give up
/// its brushes, and the rows are in insertion order anyway, which is usually
/// the same answer.
fn node_order(db: &Database, table: &Table, nodes: &[Row]) -> Vec<usize> {
    let Some(uuid_column) = table.column("NodeUuid") else {
        return (0..nodes.len()).collect();
    };

    let by_uuid = |uuid: &[u8]| -> Option<usize> {
        nodes
            .iter()
            .position(|row| row.get(uuid_column).as_blob() == Some(uuid))
    };

    let root = db
        .table("Manager")
        .ok()
        .flatten()
        .and_then(|manager| {
            let column = manager.column("RootUuid")?;
            let rows = db.rows(&manager).ok()?;
            rows.first()
                .and_then(|row| row.get(column).as_blob().map(<[u8]>::to_vec))
        })
        .and_then(|uuid| by_uuid(&uuid));

    let mut out = Vec::new();
    let mut seen = vec![false; nodes.len()];
    let mut stack = root.into_iter().collect::<Vec<_>>();
    while let Some(index) = stack.pop() {
        // A file that names a node twice would otherwise be an endless walk,
        // and this is somebody else's file.
        if std::mem::replace(&mut seen[index], true) {
            continue;
        }
        out.push(index);
        let node = &nodes[index];
        // Pushed in reverse, so the child comes off the stack before the
        // sibling and the group's own order is preserved.
        for column in ["NodeNextUuid", "NodeFirstChildUuid"] {
            if let Some(next) = table
                .column(column)
                .and_then(|c| node.get(c).as_blob())
                .filter(|uuid| uuid.len() > 1)
                .and_then(by_uuid)
            {
                stack.push(next);
            }
        }
    }

    // Anything the chain missed, in table order, so nothing is lost to a link
    // this reader did not understand.
    out.extend(
        seen.iter()
            .enumerate()
            .filter(|(_, visited)| !**visited)
            .map(|(index, _)| index),
    );
    out
}

fn node_name(table: &Table, node: &Row) -> String {
    table
        .column("NodeName")
        .and_then(|c| node.get(c).as_str())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The settings block a node points at.
fn variant_for<'a>(
    node_table: &Table,
    node: &Row,
    variant_table: &'a Table,
    variants: &'a [Row],
) -> Option<Settings<'a>> {
    let wanted = node_table
        .column("NodeVariantID")
        .and_then(|c| node.get(c).as_i64())?;
    let id = variant_table.column("VariantID")?;
    let row = variants
        .iter()
        .find(|row| row.get(id).as_i64() == Some(wanted))?;
    Some(Settings {
        table: variant_table,
        row,
    })
}

/// A row of `Variant`, read by column name.
struct Settings<'a> {
    table: &'a Table,
    row: &'a Row,
}

impl Settings<'_> {
    fn value(&self, name: &str) -> &Value {
        match self.table.column(name) {
            Some(index) => self.row.get(index),
            None => &Value::Null,
        }
    }

    fn int(&self, name: &str) -> Option<i64> {
        self.value(name).as_i64()
    }

    fn real(&self, name: &str) -> Option<f64> {
        self.value(name).as_f64()
    }

    fn blob(&self, name: &str) -> Option<&[u8]> {
        self.value(name).as_blob()
    }

    /// True when a flag column is present and not zero. A column this build has
    /// never heard of reads as off, which is the right answer for a feature
    /// that did not exist when the file was written.
    fn flag(&self, name: &str) -> bool {
        self.int(name).is_some_and(|v| v != 0)
    }

    /// A `0..100` column as a fraction.
    fn percent(&self, name: &str) -> Option<f32> {
        self.int(name).map(|v| v as f32 / 100.0)
    }

    fn effector(&self, name: &str) -> Option<Effector> {
        Effector::parse(self.blob(name)?)
    }
}

// ---------------------------------------------------------------------------
// Effect sources
// ---------------------------------------------------------------------------

/// Which input drives a setting, bit by bit.
///
/// Only these two are acted on. **Bit 4 is pen pressure**: it is the bit set on
/// the size effector of every pressure-sensitive brush in the sample files and
/// on nothing else, which is as firm as this gets without Clip Studio's source.
/// **Bit 7 is the per-dab random draw**: it is the only bit ever set on the hue,
/// saturation and brightness effectors — which is what colour jitter is — and
/// it is set exactly on the brushes whose rotation randomness is not left at its
/// default.
///
/// Bits 5, 6 and 8 are pen tilt, pen bearing and stroke speed in some order that
/// the files to hand cannot settle. They are reported through
/// [`dropped::OTHER_INPUTS`] rather than guessed at: Umber has an input for
/// speed, and wiring tilt into it would make a brush behave wrongly in a way
/// that looks deliberate.
const PRESSURE: u32 = 1 << 4;
const RANDOM: u32 = 1 << 7;
/// Everything else a setting can be driven by.
const OTHER: u32 = (1 << 5) | (1 << 6) | (1 << 8);

/// One `*Effector` blob.
///
/// Big-endian throughout, unlike the container it sits in:
///
/// ```text
/// u32  44          length of this record
/// u32               which inputs this setting supports
/// u32               which are switched on
/// i32 x 8           the minimum, per input, as a percentage; [5..8] unread
/// -- and, when the blob is longer than 44 bytes --
/// u32  12          length of the curve header
/// u32               how many control points
/// u32  16          bytes per point
/// (f64, f64) x n    the points, x then y, both 0..1
/// ```
#[derive(Clone, Debug)]
struct Effector {
    enabled: u32,
    /// Minimum for each of the five inputs, in the order of bits 4 to 8.
    minimums: [i32; 5],
    points: Vec<(f64, f64)>,
}

impl Effector {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 44 {
            return None;
        }
        let word = |at: usize| -> u32 {
            u32::from_be_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
        };
        let enabled = word(8);
        let mut minimums = [0i32; 5];
        for (i, slot) in minimums.iter_mut().enumerate() {
            *slot = word(12 + i * 4) as i32;
        }

        let mut points = Vec::new();
        if bytes.len() >= 56 {
            let count = word(48) as usize;
            for i in 0..count {
                let at = 56 + i * 16;
                let Some(pair) = bytes.get(at..at + 16) else {
                    break;
                };
                let x = f64::from_be_bytes(pair[..8].try_into().expect("eight bytes"));
                let y = f64::from_be_bytes(pair[8..].try_into().expect("eight bytes"));
                if x.is_finite() && y.is_finite() {
                    points.push((x, y));
                }
            }
        }

        Some(Self {
            enabled,
            minimums,
            points,
        })
    }

    fn drives(&self, input: u32) -> bool {
        self.enabled & input != 0
    }

    /// This setting's floor for one input, as a fraction. Clip Studio states it
    /// as a percentage of the setting's own value.
    fn minimum(&self, input: u32) -> f32 {
        let index = input.trailing_zeros().saturating_sub(4) as usize;
        let raw = self.minimums.get(index).copied().unwrap_or(0);
        (raw as f32 / 100.0).clamp(0.0, 1.0)
    }

    /// The control points as one of Umber's fixed-sample response curves.
    ///
    /// Piecewise linear between the points and held flat outside them, which is
    /// what the curve editor draws. Clip Studio's own interpolation is smoother
    /// than that between widely spaced points; five evenly spaced samples is
    /// the resolution [`ResponseCurve`] has, so the difference is below what it
    /// could record anyway.
    fn response(&self) -> ResponseCurve {
        if self.points.len() < 2 {
            return ResponseCurve::LINEAR;
        }
        let mut curve = ResponseCurve::LINEAR;
        for i in 0..ResponseCurve::N {
            let x = f64::from(ResponseCurve::x_of(i));
            curve.set(i, self.at(x) as f32);
        }
        curve
    }

    fn at(&self, x: f64) -> f64 {
        let first = self.points[0];
        if x <= first.0 {
            return first.1;
        }
        for pair in self.points.windows(2) {
            let (ax, ay) = pair[0];
            let (bx, by) = pair[1];
            if x <= bx {
                let span = bx - ax;
                if span <= f64::EPSILON {
                    return by;
                }
                return ay + (by - ay) * (x - ax) / span;
            }
        }
        self.points.last().expect("at least two points").1
    }
}

/// Whether any effect source on this brush is something Umber will not
/// reproduce.
///
/// Every `*Effector` column is swept rather than the handful this importer
/// reads, because the question is what the *brush* does and not what this
/// function happens to look at — a setting Umber has no field for at all is
/// still a setting whose behaviour will not arrive.
fn reads_other_inputs(settings: &Settings) -> bool {
    let effectors = settings
        .table
        .columns()
        .iter()
        .filter(|name| name.ends_with("Effector"))
        // The dual brush's own are not consulted: the whole dual brush is
        // dropped and already says so.
        .filter(|name| !name.starts_with("Dual"))
        .filter_map(|name| settings.effector(name))
        .any(|effector| effector.enabled & OTHER != 0);

    // Rotation states its sources as a bare integer of the same bits rather
    // than as a record, so it is not in the sweep above and has to be asked
    // separately — which matters, because a chisel that turns with the stroke
    // is exactly the brush this would otherwise stay quiet about.
    let rotation = settings.int("BrushRotationEffector").unwrap_or(0) as u32;
    effectors || rotation & OTHER != 0
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

/// The bitmap materials in a file, indexed by the path a reference names.
#[derive(Default)]
struct Materials {
    by_path: Vec<(String, Vec<u8>)>,
}

impl Materials {
    fn new(table: &Table, rows: Vec<Row>) -> Self {
        let (Some(path), Some(data)) = (table.column("OriginalPath"), table.column("FileData"))
        else {
            return Self::default();
        };
        let mut by_path = Vec::new();
        for row in rows {
            let (Some(key), Some(bytes)) = (row.get(path).as_str(), row.get(data).as_blob()) else {
                continue;
            };
            if key.is_empty() || by_path.iter().any(|(seen, _)| seen == key) {
                continue;
            }
            by_path.push((key.to_string(), bytes.to_vec()));
        }
        Self { by_path }
    }

    /// The tar archive a reference blob points at.
    fn resolve(&self, reference: &[u8]) -> Option<&[u8]> {
        let path = reference_path(reference)?;
        self.by_path
            .iter()
            .find(|(key, _)| *key == path)
            .map(|(_, bytes)| bytes.as_slice())
    }
}

/// The `MaterialFile.OriginalPath` a reference blob names.
///
/// The blob's first field is that path, at a fixed offset, with its length in
/// bytes just before it and the text in UTF-16 little-endian:
///
/// ```text
/// u32 be   8      record size
/// u32 be          how many materials the reference holds
/// u32 be          bytes that follow
/// u32 be          bytes of path
/// utf-16le        the path
/// ```
fn reference_path(blob: &[u8]) -> Option<String> {
    if blob.len() < 16 {
        return None;
    }
    let len = u32::from_be_bytes(blob[12..16].try_into().expect("four bytes")) as usize;
    let text = blob.get(16..16 + len)?;
    let units: Vec<u16> = text
        .chunks_exact(2)
        .map(|p| u16::from_le_bytes([p[0], p[1]]))
        .collect();
    let path = String::from_utf16(&units).ok()?;
    // A path this reader recognises always names a member inside the archive,
    // which is what distinguishes it from any other string that might be first.
    path.contains(":data:").then_some(path)
}

/// How many materials a reference names. One for almost every brush; more is a
/// tip that changes as the stroke goes on, which Umber cannot bind.
fn reference_count(blob: &[u8]) -> u32 {
    if blob.len() < 8 {
        return 0;
    }
    u32::from_be_bytes(blob[4..8].try_into().expect("four bytes"))
}

/// One member out of a USTAR archive.
///
/// Written out rather than taken from a crate for the same reason
/// [`crate::sqlite`] is: it is fifty lines, it is the only tar Umber will ever
/// read, and a dependency here would be one more thing between a brush file and
/// a cross-build.
fn tar_member(archive: &[u8], wanted: &str) -> Option<Vec<u8>> {
    let mut at = 0usize;
    while at + 512 <= archive.len() {
        let header = &archive[at..at + 512];
        // Two zero blocks end the archive, and one is enough to stop on.
        if header.iter().all(|b| *b == 0) {
            return None;
        }
        let name = {
            let end = header[..100].iter().position(|b| *b == 0).unwrap_or(100);
            std::str::from_utf8(&header[..end]).ok()?
        };
        // Octal, null- or space-terminated, as tar has always written it.
        let size = {
            let field = &header[124..136];
            let end = field
                .iter()
                .position(|b| *b == 0 || *b == b' ')
                .unwrap_or(field.len());
            let digits = std::str::from_utf8(&field[..end]).ok()?.trim();
            usize::from_str_radix(digits, 8).ok()?
        };

        let body = at + 512;
        if name == wanted {
            return archive.get(body..body + size).map(<[u8]>::to_vec);
        }
        // Every member is padded up to the next 512-byte block.
        at = body.checked_add(size.next_multiple_of(512))?;
    }
    None
}

/// Turn a material's thumbnail into a coverage mask.
///
/// Coverage is `alpha * (1 - luminance)`, which is what "how much ink is here"
/// means for a picture laid over white — and it has to be both terms, because
/// the two kinds of material in the sample files disagree about which one
/// carries the shape. A brush tip is black on transparent, so its alpha is the
/// mark and its colour is constant; a paper texture is fully opaque grey, so
/// its luminance is the mark and its alpha is constant. Either alone turns the
/// other kind into a blank rectangle.
fn mask_from_thumbnail(png_bytes: &[u8]) -> Option<TipMask> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    // Expands a palette or a low bit depth, and 16-bit down to 8, so every
    // shape below sees the same three cases.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;

    let texels = (info.width as usize).checked_mul(info.height as usize)?;
    let ink = |r: u8, g: u8, b: u8, a: u8| -> u8 {
        // Rec. 601 luma over the stored sRGB values, which is the space the
        // picture was authored in and the one a painter judges it in.
        let luma = 0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b);
        let coverage = f32::from(a) / 255.0 * (1.0 - luma / 255.0);
        (coverage * 255.0).round().clamp(0.0, 255.0) as u8
    };

    let coverage: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buffer[..texels * 4]
            .chunks_exact(4)
            .map(|p| ink(p[0], p[1], p[2], p[3]))
            .collect(),
        png::ColorType::Rgb => buffer[..texels * 3]
            .chunks_exact(3)
            .map(|p| ink(p[0], p[1], p[2], 255))
            .collect(),
        png::ColorType::GrayscaleAlpha => buffer[..texels * 2]
            .chunks_exact(2)
            .map(|p| ink(p[0], p[0], p[0], p[1]))
            .collect(),
        png::ColorType::Grayscale => buffer[..texels]
            .iter()
            .map(|v| ink(*v, *v, *v, 255))
            .collect(),
        png::ColorType::Indexed => return None,
    };

    TipMask::new(info.width, info.height, coverage).ok()
}

/// The mask a material reference resolves to, if there is one.
fn tip_for(reference: &[u8], materials: &Materials) -> Option<TipMask> {
    let archive = materials.resolve(reference)?;
    let png_bytes = tar_member(archive, "thumbnail/thumbnail.png")?;
    mask_from_thumbnail(&png_bytes)
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/// Clip Studio's interval is a percentage of the brush size, the same unit
/// GIMP's `.gbr` and `.vbr` state theirs in.
const INTERVAL_PER_CENT: f32 = 100.0;

/// Umber's grain tile is stated in document pixels and Clip Studio's texture
/// scale is a percentage of the material's own size, which is not recorded
/// anywhere the thumbnail can be trusted for. A hundred per cent is therefore
/// read as Umber's own default tile, so the *relative* coarseness of two
/// brushes out of one file is right even where neither matches Clip Studio's
/// absolute size.
const GRAIN_TILE_AT_FULL_SCALE: f32 = 256.0;

/// Dabs a second for a brush set to keep spraying while the pen is held still.
///
/// Clip Studio states that as a flag and takes the rate from its own timer,
/// which is not in the file. Zero — the alternative — is an airbrush that stops
/// the moment the hand does, which is the one thing an airbrush must not do.
const HELD_SPRAY_RATE: f32 = 30.0;

fn convert(
    settings: &Settings,
    materials: &Materials,
) -> (Brush, Option<TipMask>, Vec<&'static str>) {
    let mut dropped = Vec::new();
    let default = Brush::default();

    // ---- the dab -------------------------------------------------------
    // Thickness is how flat the dab is, as a percentage: a hundred is round and
    // thirty is three and a bit times as long as it is wide. `Brush::size`
    // describes the long axis either way, so this narrows the dab rather than
    // growing it — which is what makes a flat brush cover the ground the round
    // one it came from covered.
    let thickness = settings.percent("BrushThickness").unwrap_or(1.0);

    let mut brush = Brush {
        size: settings
            .real("BrushSize")
            .map_or(default.size, |v| v as f32)
            .clamp(Brush::MIN_SIZE, Brush::MAX_SIZE),
        hardness: settings
            .percent("BrushHardness")
            .unwrap_or(default.hardness)
            .clamp(0.0, 1.0),
        opacity: settings
            .percent("Opacity")
            .unwrap_or(default.opacity)
            .clamp(0.0, 1.0),
        // Clip Studio calls stabilisation "flicker reduction", which is a
        // literal translation of the same Japanese label a tablet driver uses
        // for hand shake.
        stabilization: settings
            .percent("FlickerReduction")
            .unwrap_or(default.stabilization)
            .clamp(0.0, 0.95),
        dab_ratio: if thickness > 0.01 {
            (1.0 / thickness).clamp(1.0, 20.0)
        } else {
            1.0
        },
        dab_angle: settings.real("BrushRotation").unwrap_or(0.0) as f32
            // The flattening can be stated on the other axis, which is a
            // quarter turn and nothing else.
            + if settings.flag("BrushVerticalThicknes") {
                90.0
            } else {
                0.0
            },
        ..default
    };

    // ---- spacing -------------------------------------------------------
    // Clip Studio picks the spacing itself unless the control is set to
    // "fixed", and the number left in the file is then whatever it last was —
    // 0.1 in one sample file, which as a spacing would be ten thousand dabs to
    // the diameter, and 10.0 in twelve others, which would be a dotted line.
    //
    // Deliberately **not** reported as a loss. Umber picks a spacing too, so an
    // automatic one arrives as an automatic one; and every brush in both sample
    // files is set that way, so a note about it would appear on every import
    // ever made and train the reader to skip the list that carries the losses
    // that do matter.
    if settings.int("BrushAutoIntervalType") == Some(0)
        && let Some(interval) = settings.real("BrushInterval")
    {
        brush.spacing = (interval as f32 / INTERVAL_PER_CENT).clamp(0.01, 4.0);
    }

    // ---- pressure ------------------------------------------------------
    let size_effector = settings.effector("BrushSizeEffector");
    brush.pressure_size = size_effector.as_ref().is_some_and(|e| e.drives(PRESSURE));
    if let Some(effector) = size_effector.as_ref().filter(|e| e.drives(PRESSURE)) {
        brush.min_size_ratio = effector.minimum(PRESSURE);
        brush.size_curve = effector.response();
    }
    // The same effector's random input is a per-dab size draw, which is
    // Umber's radius jitter. Stated as a floor rather than a spread, so a
    // minimum of forty per cent is a dab that may be anywhere from that to
    // full — half a factor of two and a half, in the log space the jitter is
    // measured in.
    if let Some(effector) = size_effector.as_ref().filter(|e| e.drives(RANDOM)) {
        brush.radius_jitter = spread_from_floor(effector.minimum(RANDOM));
    }

    if let Some(effector) = settings.effector("BrushOpacityEffector") {
        brush.pressure_opacity = effector.drives(PRESSURE);
        if brush.pressure_opacity {
            brush.opacity_curve = effector.response();
        }
    }

    // Rotation has its own encoding: a plain integer of the same bits rather
    // than the record every other setting uses, with the amount in a column
    // beside it.
    let rotation_inputs = settings.int("BrushRotationEffector").unwrap_or(0) as u32;
    if rotation_inputs & RANDOM != 0 {
        // Clip Studio's amount is a percentage of a full turn.
        let amount = settings.percent("BrushRotationRandomScale").unwrap_or(0.0);
        brush.dab_angle_jitter = (amount * 360.0).clamp(0.0, 360.0);
    }

    // ---- scatter -------------------------------------------------------
    if settings.flag("BrushUseSpray") {
        // Umber measures scatter in dab radii; Clip Studio states the spray's
        // own diameter in the same unit the brush size is in.
        let spray = settings.real("BrushSpraySize").unwrap_or(0.0) as f32;
        if spray > 0.0 && brush.size > 0.0 {
            brush.scatter = (spray / brush.size).clamp(0.0, 8.0);
        }
        push_once(&mut dropped, dropped::SPRAY_SHAPE);
    }

    // ---- colour pickup -------------------------------------------------
    if settings.flag("BrushUseWaterColor") {
        // Clip Studio splits mixing into how much paint the brush carries and
        // how dense that paint is; Umber has one number for how much of a dab
        // comes off the canvas instead of the palette. A brush carrying no
        // paint is a pure blender however its density reads, and one carrying
        // a full load still smears by its density — so the stronger of the two
        // readings is what survives. Named as an approximation either way.
        let carried = settings.percent("BrushMixAlpha").unwrap_or(1.0);
        let density = settings.percent("BrushMixColor").unwrap_or(0.0);
        brush.smudge = (1.0 - carried).max(density).clamp(0.0, 1.0);
        // "Colour stretch" is how far picked-up paint is carried along, which
        // is exactly what smudge length is.
        brush.smudge_length = settings
            .percent("BrushMixColorExtension")
            .unwrap_or(default.smudge_length)
            .clamp(0.0, 0.99);
        push_once(&mut dropped, dropped::MIXING);
    }

    // ---- grain ---------------------------------------------------------
    if settings.blob("TextureImage").is_some() {
        brush.grain = settings
            .percent("TextureDensity")
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let scale = settings.real("TextureScale2").unwrap_or(100.0) as f32 / 100.0;
        brush.grain_scale = (GRAIN_TILE_AT_FULL_SCALE * scale)
            .clamp(Brush::MIN_GRAIN_SCALE, Brush::MAX_GRAIN_SCALE);
        // Umber's papers are a closed set, so the strength and the tile size
        // come across and the picture does not.
        brush.grain_pattern = GrainPattern::Tooth;
        if brush.has_grain() {
            push_once(&mut dropped, dropped::PAPER_TEXTURE);
        }
    }

    // ---- the tip -------------------------------------------------------
    let mut tip = None;
    if settings.flag("BrushUsePatternImage")
        && let Some(reference) = settings.blob("BrushPatternImageArray")
    {
        if reference_count(reference) > 1 {
            push_once(&mut dropped, dropped::SEVERAL_TIPS);
        }
        match tip_for(reference, materials) {
            Some(mask) => {
                push_once(&mut dropped, dropped::THUMBNAIL_TIP);
                // A stamp is the overlap of many faint impressions, and Clip
                // Studio composites every dab as GIMP and Krita do. Measured
                // rather than assumed, by the same function that decides it for
                // the shipped library.
                let measured = stroke_coverage(&mask, brush.spacing);
                if measured.is_usable() {
                    brush.build_up = measured.needs_build_up();
                    tip = Some(mask);
                } else {
                    // A mask with nothing dark in it would be a brush that
                    // paints nothing at all, which is worse than a round one.
                    push_once(&mut dropped, dropped::UNUSABLE_TIP);
                }
            }
            // The material was named and is not in the file — Clip Studio
            // leaves an installed one out and expects to find it locally.
            None => push_once(&mut dropped, dropped::UNUSABLE_TIP),
        }
    }

    // ---- what is left over ---------------------------------------------
    if settings.flag("BrushContinuousPlot") {
        brush.dabs_per_second = HELD_SPRAY_RATE;
    }
    if reads_other_inputs(settings) {
        push_once(&mut dropped, dropped::OTHER_INPUTS);
    }
    if settings.flag("UseDualBrush") {
        push_once(&mut dropped, dropped::DUAL_BRUSH);
    }
    if settings.flag("BrushUseWaterEdge") {
        push_once(&mut dropped, dropped::WATER_EDGE);
    }
    if settings.flag("BrushUseIn") || settings.flag("BrushUseOut") {
        push_once(&mut dropped, dropped::TAPER);
    }
    if settings.flag("BrushRibbon") {
        push_once(&mut dropped, dropped::RIBBON);
    }
    if settings.flag("CompositeMode") {
        push_once(&mut dropped, dropped::BLEND_MODE);
    }
    if settings.flag("BrushUseVectorEraser") || settings.flag("BrushUseVectorMagnet") {
        push_once(&mut dropped, dropped::VECTOR);
    }
    if settings.flag("BrushChangeStrokeColor")
        || settings.flag("BrushHueChange")
        || settings.flag("BrushSaturationChange")
        || settings.flag("BrushValueChange")
    {
        push_once(&mut dropped, dropped::COLOUR_JITTER);
    }

    (brush, tip, dropped)
}

/// A jitter spread, given the floor a random draw is allowed to fall to.
///
/// Umber's radius jitter is a standard deviation in log space, so a floor of
/// `f` — a dab that may be anywhere between `f` and full size — is a spread of
/// about `-ln(f) / 4`, four standard deviations covering the range. A floor of
/// one is no variation at all, which is the exact identity.
fn spread_from_floor(floor: f32) -> f32 {
    if floor >= 0.999 {
        return 0.0;
    }
    (-floor.max(0.001).ln() / 4.0).clamp(0.0, 1.5)
}

fn push_once(list: &mut Vec<&'static str>, loss: &'static str) {
    if !list.contains(&loss) {
        list.push(loss);
    }
}

fn malformed(message: String) -> PresetError {
    PresetError::Malformed(None, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::fixture::{TableSpec, database};

    // -------------------------------------------------------------- fixtures

    /// The columns of `Variant` this importer actually reads, in an order that
    /// is deliberately *not* the order the code reads them in — the point of
    /// the schema being name-addressed is that neither one matters.
    const VARIANT_COLUMNS: [&str; 36] = [
        "VariantID",
        "BrushRotation",
        "Opacity",
        "BrushThickness",
        "BrushSize",
        "BrushSizeEffector",
        "BrushHardness",
        "BrushInterval",
        "BrushAutoIntervalType",
        "BrushOpacityEffector",
        "BrushVerticalThicknes",
        "BrushRotationEffector",
        "BrushRotationRandomScale",
        "BrushUsePatternImage",
        "BrushPatternImageArray",
        "TextureImage",
        "TextureDensity",
        "TextureScale2",
        "BrushUseWaterColor",
        "BrushMixColor",
        "BrushMixAlpha",
        "BrushMixColorExtension",
        "UseDualBrush",
        "FlickerReduction",
        "BrushUseSpray",
        "BrushSpraySize",
        "BrushContinuousPlot",
        "BrushUseWaterEdge",
        "BrushUseIn",
        "BrushUseOut",
        "BrushRibbon",
        "CompositeMode",
        "BrushUseVectorEraser",
        "BrushUseVectorMagnet",
        "BrushChangeStrokeColor",
        "BrushHueChange",
    ];

    /// A settings row, named the way the file names them.
    #[derive(Default)]
    struct Variant {
        id: i64,
        fields: Vec<(&'static str, Value)>,
    }

    impl Variant {
        fn new(id: i64) -> Self {
            Self {
                id,
                fields: Vec::new(),
            }
        }

        /// Replaces rather than appends, so a test can take [`Variant::plain`]
        /// and change the one column it is about.
        fn set(mut self, name: &'static str, value: Value) -> Self {
            match self.fields.iter_mut().find(|(n, _)| *n == name) {
                Some(slot) => slot.1 = value,
                None => self.fields.push((name, value)),
            }
            self
        }

        fn int(self, name: &'static str, value: i64) -> Self {
            self.set(name, Value::Integer(value))
        }

        fn real(self, name: &'static str, value: f64) -> Self {
            self.set(name, Value::Real(value))
        }

        /// A brush that is plausible in every column this importer reads, so
        /// that a test can change one thing and attribute the difference to it.
        fn plain(id: i64) -> Self {
            Self::new(id)
                .real("BrushSize", 24.0)
                .int("BrushHardness", 50)
                .int("Opacity", 100)
                .int("BrushThickness", 100)
                .real("BrushRotation", 0.0)
                .int("BrushAutoIntervalType", 0)
                .real("BrushInterval", 10.0)
                .int("FlickerReduction", 35)
        }

        fn row(&self) -> Vec<Value> {
            VARIANT_COLUMNS
                .iter()
                .map(|column| {
                    if *column == "VariantID" {
                        return Value::Integer(self.id);
                    }
                    self.fields
                        .iter()
                        .find(|(name, _)| name == column)
                        .map_or(Value::Null, |(_, v)| v.clone())
                })
                .collect()
        }
    }

    /// Build a `.sut` or `.sutg` out of `(name, variant)` pairs.
    ///
    /// The nodes are chained the way Clip Studio chains them — a root that owns
    /// the first and a `NodeNextUuid` from each to the one after — and the
    /// *table* order is reversed, so a reader that ignored the chain would put
    /// the brushes out of order and the test would say so.
    fn sut(tools: &[(&str, Variant)], materials: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let uuid = |n: u8| Value::Blob(vec![n; 16]);
        let none = Value::Blob(vec![0]);

        let mut node = TableSpec::new(
            "Node",
            &[
                "NodeUuid",
                "NodeName",
                "NodeVariantID",
                "NodeInitVariantID",
                "NodeNextUuid",
                "NodeFirstChildUuid",
            ],
        );
        // The root is a group with no settings of its own, exactly as a `.sutg`
        // has one.
        node = node.row(vec![
            uuid(0),
            Value::Text(String::new()),
            Value::Integer(0),
            Value::Integer(0),
            none.clone(),
            uuid(1),
        ]);

        let mut rows: Vec<Vec<Value>> = Vec::new();
        for (i, (name, variant)) in tools.iter().enumerate() {
            let next = if i + 1 < tools.len() {
                uuid(i as u8 + 2)
            } else {
                none.clone()
            };
            rows.push(vec![
                uuid(i as u8 + 1),
                Value::Text((*name).to_string()),
                Value::Integer(variant.id),
                Value::Integer(variant.id + 1000),
                next,
                none.clone(),
            ]);
        }
        for row in rows.into_iter().rev() {
            node = node.row(row);
        }

        let mut variant = TableSpec::new("Variant", &VARIANT_COLUMNS);
        for (_, v) in tools {
            variant = variant.row(v.row());
            // The "reset to defaults" twin, which holds nothing and must never
            // be the one that is read.
            variant = variant.row(Variant::new(v.id + 1000).row());
        }

        let mut material = TableSpec::new("MaterialFile", &["OriginalPath", "FileData"]);
        for (path, bytes) in materials {
            material = material.row(vec![
                Value::Text((*path).to_string()),
                Value::Blob(bytes.clone()),
            ]);
        }

        database(&[
            TableSpec::new("Manager", &["ToolType", "Version", "RootUuid"]).row(vec![
                Value::Integer(0),
                Value::Integer(144),
                uuid(0),
            ]),
            node,
            variant,
            material,
        ])
    }

    /// A `*Effector` blob: which inputs are on, their minimums, and a curve.
    fn effector(enabled: u32, minimums: [i32; 5], curve: &[(f64, f64)]) -> Value {
        let mut out = Vec::new();
        out.extend_from_slice(&44u32.to_be_bytes());
        out.extend_from_slice(&0x1f0u32.to_be_bytes());
        out.extend_from_slice(&enabled.to_be_bytes());
        for m in minimums {
            out.extend_from_slice(&m.to_be_bytes());
        }
        out.extend_from_slice(&0i32.to_be_bytes());
        out.extend_from_slice(&0i32.to_be_bytes());
        out.extend_from_slice(&100i32.to_be_bytes());
        if !curve.is_empty() {
            out.extend_from_slice(&12u32.to_be_bytes());
            out.extend_from_slice(&(curve.len() as u32).to_be_bytes());
            out.extend_from_slice(&16u32.to_be_bytes());
            for (x, y) in curve {
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
            }
        }
        Value::Blob(out)
    }

    /// A material reference: the path of the `MaterialFile` row holding it.
    fn reference(path: &str, count: u32) -> Value {
        let text: Vec<u8> = path.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(&8u32.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&(text.len() as u32 + 4).to_be_bytes());
        out.extend_from_slice(&(text.len() as u32).to_be_bytes());
        out.extend_from_slice(&text);
        Value::Blob(out)
    }

    /// A USTAR archive holding exactly these members.
    fn tar(members: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, body) in members {
            let mut header = vec![0u8; 512];
            header[..name.len()].copy_from_slice(name.as_bytes());
            let size = format!("{:011o}\0", body.len());
            header[124..136].copy_from_slice(size.as_bytes());
            header[257..262].copy_from_slice(b"ustar");
            header[263..265].copy_from_slice(b"00");
            // The checksum field is spaces while the sum is taken; nothing here
            // verifies it, and tar itself tolerates the field being left blank.
            for b in &mut header[148..156] {
                *b = b' ';
            }
            out.extend_from_slice(&header);
            out.extend_from_slice(body);
            out.resize(out.len().next_multiple_of(512), 0);
        }
        out.extend_from_slice(&[0u8; 1024]);
        out
    }

    /// A material archive whose thumbnail is `w` by `h` of one RGBA pixel.
    fn material(w: u32, h: u32, pixel: [u8; 4]) -> Vec<u8> {
        let rgba: Vec<u8> = std::iter::repeat_n(pixel, (w * h) as usize)
            .flatten()
            .collect();
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("fixture png header");
            writer.write_image_data(&rgba).expect("fixture png data");
        }
        tar(&[
            ("catalog.zip", vec![0x89, b'C', b'2', b'F']),
            ("thumbnail/thumbnail.png", png_bytes),
            ("icedata/layerData.xml", b"<infolist/>".to_vec()),
        ])
    }

    // ----------------------------------------------------------------- tests

    #[test]
    fn a_single_sub_tool_arrives_with_its_name_and_size() {
        let bytes = sut(
            &[("Sketch", Variant::plain(1082).real("BrushSize", 6.5))],
            &[],
        );
        let file = from_sut(&bytes).expect("read");

        assert_eq!(file.tools.len(), 1);
        assert_eq!(file.tools[0].name, "Sketch");
        assert_eq!(file.tools[0].brush.size, 6.5);
        assert_eq!(file.tools[0].brush.hardness, 0.5);
        assert_eq!(file.tools[0].brush.opacity, 1.0);
        // A plain round brush with no effect sources has nothing to apologise
        // for, which is what makes the warnings mean something when they appear.
        assert!(
            file.tools[0].dropped.is_empty(),
            "{:?}",
            file.tools[0].dropped
        );
        assert!(file.dropped.is_empty());
    }

    /// A `.sutg` is a linked list, and the rows are written in whatever order
    /// the file was edited in. The fixture reverses the table deliberately.
    #[test]
    fn a_group_keeps_the_order_the_tool_palette_shows() {
        let bytes = sut(
            &[
                ("Pencil", Variant::plain(1)),
                ("Ink", Variant::plain(2)),
                ("Wash", Variant::plain(3)),
            ],
            &[],
        );
        let file = from_sut(&bytes).expect("read");
        let names: Vec<&str> = file.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Pencil", "Ink", "Wash"]);
    }

    /// `NodeInitVariantID` names a second settings block holding almost
    /// nothing. Reading it instead of `NodeVariantID` gives a brush with no
    /// size at all, which would then be skipped as "not a brush" — so this
    /// pins the right one rather than the presence of any.
    #[test]
    fn the_reset_to_defaults_variant_is_not_the_one_that_is_read() {
        let bytes = sut(
            &[("Sketch", Variant::plain(1082).real("BrushSize", 9.0))],
            &[],
        );
        let file = from_sut(&bytes).expect("read");
        assert_eq!(file.tools[0].brush.size, 9.0);
    }

    /// The schema is not fixed: a group holding a fill tool declares columns a
    /// lone brush's does not, interleaved with the rest. Every column is
    /// therefore addressed by name, and this is what says so.
    #[test]
    fn columns_are_found_by_name_whatever_order_the_schema_declares_them_in() {
        // Same values, a schema with the brush columns shuffled and padded.
        let columns = [
            "FillColorMargin",
            "BrushHardness",
            "SelectUseSnap",
            "VariantID",
            "BrushSize",
            "FillExpandLength",
            "Opacity",
        ];
        let node = TableSpec::new("Node", &["NodeUuid", "NodeName", "NodeVariantID"]).row(vec![
            Value::Blob(vec![1; 16]),
            Value::Text("Odd".to_string()),
            Value::Integer(7),
        ]);
        let variant = TableSpec::new("Variant", &columns).row(vec![
            Value::Real(10.0),
            Value::Integer(80),
            Value::Integer(1),
            Value::Integer(7),
            Value::Real(12.0),
            Value::Real(10.0),
            Value::Integer(60),
        ]);
        let bytes = database(&[node, variant]);

        let file = from_sut(&bytes).expect("read");
        assert_eq!(file.tools[0].name, "Odd");
        assert_eq!(file.tools[0].brush.size, 12.0);
        assert_eq!(file.tools[0].brush.hardness, 0.8);
        assert_eq!(file.tools[0].brush.opacity, 0.6);
    }

    /// The fill, selection and shape tools share these tables and leave every
    /// brush column null. Importing one as a brush would produce a preset made
    /// entirely of defaults wearing somebody else's name.
    #[test]
    fn a_sub_tool_that_is_not_a_brush_is_skipped_and_said_so() {
        let bytes = sut(
            &[
                ("Pencil", Variant::plain(1)),
                // No BrushSize at all: a fill tool.
                ("Fill", Variant::new(2).int("Opacity", 100)),
            ],
            &[],
        );
        let file = from_sut(&bytes).expect("read");
        assert_eq!(file.tools.len(), 1);
        assert_eq!(file.tools[0].name, "Pencil");
        assert_eq!(file.dropped, [dropped::NOT_A_BRUSH]);
    }

    #[test]
    fn a_file_with_nothing_but_a_fill_tool_is_refused_with_a_reason() {
        let bytes = sut(&[("Fill", Variant::new(2).int("Opacity", 100))], &[]);
        let err = from_sut(&bytes).unwrap_err();
        assert!(err.to_string().contains("no brushes"), "{err}");
    }

    /// A `.sut` is somebody else's binary file, and this reader is a hand-written
    /// walk over a page tree with offsets taken out of that file. Every slice it
    /// cuts is one a corrupt header could aim past the end.
    ///
    /// So the standard is the same one the importers are all held to: a file
    /// that makes no sense is **refused**, never fatal. A panic here would take
    /// the whole application down — with every unsaved document in it — because
    /// somebody opened the wrong file. That is a far worse outcome than a
    /// refusal, and it is not a theoretical risk: the failure would arrive as a
    /// bug report saying "Umber closes when I import my brushes".
    ///
    /// Truncations walk the length; the corruptions concentrate on the first
    /// kilobyte, where the file header, the schema and the root page live and
    /// where a wrong number does the most damage. Deterministic rather than
    /// random, so a failure can be reproduced from the seed alone.
    #[test]
    fn a_corrupt_file_is_refused_and_never_panics() {
        let good = sut(
            &[
                ("Sketch", Variant::plain(1082).real("BrushSize", 6.5)),
                ("Ink", Variant::plain(1082).real("BrushSize", 22.0)),
            ],
            &[],
        );

        let mut cases: Vec<Vec<u8>> = Vec::new();
        for i in 0..good.len().min(256) {
            cases.push(good[..good.len() * i / 256].to_vec());
        }
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..512 {
            let mut c = good.clone();
            for _ in 0..6 {
                seed = seed
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let reach = c.len().min(1024);
                let at = (seed >> 33) as usize % reach;
                c[at] ^= ((seed >> 11) & 0xff) as u8;
            }
            cases.push(c);
        }

        for (i, case) in cases.iter().enumerate() {
            // Whatever it decides, it must decide it — `from_sut` returning
            // either arm is a pass; unwinding is not.
            let verdict = std::panic::catch_unwind(|| from_sut(case).is_ok());
            assert!(
                verdict.is_ok(),
                "case {i} ({} bytes) panicked instead of being refused",
                case.len()
            );
        }
    }

    #[test]
    fn a_file_that_is_not_a_database_is_refused() {
        assert!(from_sut(b"this is not a sub tool").is_err());
        assert!(from_sut(&[]).is_err());
    }

    /// Pen pressure is the one effect source that carries across whole: the
    /// flag, the floor and the curve.
    #[test]
    fn pressure_drives_size_through_its_floor_and_its_curve() {
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1).set(
                    "BrushSizeEffector",
                    // Pressure only, a floor of 20%, and a curve that stays
                    // flat for the first half of the range.
                    effector(
                        PRESSURE,
                        [20, 0, 0, 0, 0],
                        &[(0.0, 0.0), (0.5, 0.0), (1.0, 1.0)],
                    ),
                ),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;

        assert!(brush.pressure_size);
        assert!((brush.min_size_ratio - 0.2).abs() < 1e-6);
        // Sampled at 0, 0.25, 0.5, 0.75 and 1: flat, then straight up.
        assert_eq!(brush.size_curve.points[0], 0.0);
        assert_eq!(brush.size_curve.points[2], 0.0);
        assert!((brush.size_curve.points[3] - 0.5).abs() < 1e-5);
        assert!((brush.size_curve.points[4] - 1.0).abs() < 1e-5);
        // And the radius the engine will actually ask for.
        assert!((brush.radius_at(0.0) - brush.size * 0.5 * 0.2).abs() < 1e-4);
        assert!((brush.radius_at(1.0) - brush.size * 0.5).abs() < 1e-4);
    }

    /// Umber's default brush *is* pressure-sensitive, so a Clip Studio brush
    /// that is not has to switch it off rather than leave it alone.
    #[test]
    fn a_brush_with_no_pressure_on_size_does_not_get_umbers_default() {
        assert!(Brush::default().pressure_size, "the default has moved");
        let bytes = sut(
            &[(
                "Marker",
                Variant::plain(1).set("BrushSizeEffector", effector(0, [0; 5], &[])),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!(!brush.pressure_size);
        assert_eq!(brush.radius_at(0.0), brush.radius_at(1.0));
    }

    /// Everything but pressure and randomness is named rather than wired to
    /// whichever of Umber's inputs looks closest.
    #[test]
    fn an_input_this_reader_cannot_identify_is_named_rather_than_guessed_at() {
        let bytes = sut(
            &[(
                "Tilt pen",
                Variant::plain(1).set("BrushSizeEffector", effector(1 << 5, [0; 5], &[])),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.dropped.contains(&dropped::OTHER_INPUTS));
        // And it must not have quietly become pressure on the way past.
        assert!(!tool.brush.pressure_size);
        assert!(tool.brush.modulations.is_empty());
    }

    #[test]
    fn a_flattened_dab_keeps_its_long_axis_and_its_angle() {
        let bytes = sut(
            &[(
                "Chisel",
                Variant::plain(1)
                    .int("BrushThickness", 25)
                    .real("BrushRotation", 90.0),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!((brush.dab_ratio - 4.0).abs() < 1e-5);
        assert_eq!(brush.dab_angle, 90.0);
        assert!(brush.dab_has_angle());

        // The flattening can be stated on the other axis instead, which is a
        // quarter turn and nothing else.
        let bytes = sut(
            &[(
                "Upright chisel",
                Variant::plain(1)
                    .int("BrushThickness", 25)
                    .int("BrushVerticalThicknes", 1),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!((brush.dab_ratio - 4.0).abs() < 1e-5);
        assert_eq!(brush.dab_angle, 90.0);
    }

    /// Rotation states its inputs as a bare integer rather than as the record
    /// every other setting uses, with the amount in a column beside it.
    #[test]
    fn random_rotation_becomes_angle_jitter_and_only_when_it_is_switched_on() {
        let bytes = sut(
            &[(
                "Charcoal",
                Variant::plain(1)
                    .int("BrushThickness", 50)
                    .int("BrushRotationEffector", (RANDOM | 3) as i64)
                    .int("BrushRotationRandomScale", 50),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert_eq!(brush.dab_angle_jitter, 180.0);

        // The amount stays in the file when the source is switched off, so the
        // flag is what decides — reading the number alone would put a random
        // turn on every dab of a brush that asked for none.
        let bytes = sut(
            &[(
                "Nib",
                Variant::plain(1)
                    .int("BrushThickness", 50)
                    .int("BrushRotationEffector", 3)
                    .int("BrushRotationRandomScale", 100),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert_eq!(brush.dab_angle_jitter, 0.0);
    }

    /// Clip Studio chooses the spacing itself unless the control says
    /// "fixed", and the number left in the file is then whatever it last was —
    /// 0.1 in a real file, which as a spacing is ten thousand dabs to the
    /// diameter and a stroke that takes a minute to draw.
    #[test]
    fn an_automatic_interval_is_not_read_as_a_spacing() {
        let bytes = sut(
            &[(
                "Marker",
                Variant::plain(1)
                    .int("BrushAutoIntervalType", 2)
                    .real("BrushInterval", 0.1),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.brush.spacing, Brush::default().spacing);
        // And it is not reported: Umber picks a spacing as well, so an
        // automatic one arrives as an automatic one rather than as a loss.
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);

        // A fixed interval is a percentage of the brush size, which is the same
        // unit GIMP states its own in.
        let bytes = sut(
            &[(
                "Dotted",
                Variant::plain(1)
                    .int("BrushAutoIntervalType", 0)
                    .real("BrushInterval", 40.0),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!((tool.brush.spacing - 0.4).abs() < 1e-6);
    }

    /// The dab angle names its sources as a bare integer rather than as the
    /// record every other setting uses, so it is easily missed by a sweep of
    /// the effector columns — and a chisel that turns with the stroke is
    /// exactly the brush that would then arrive silently wrong.
    #[test]
    fn an_input_driving_the_dab_angle_is_reported_too() {
        let bytes = sut(
            &[(
                "Rake",
                Variant::plain(1)
                    .int("BrushThickness", 40)
                    .int("BrushRotationEffector", ((1 << 6) | 3) as i64),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.dropped.contains(&dropped::OTHER_INPUTS));
        assert_eq!(tool.brush.dab_angle_jitter, 0.0);
    }

    /// The whole point of the exercise: a stamp brush has to arrive with the
    /// picture it stamps, out of the tar inside the blob inside the database.
    #[test]
    fn a_stamp_brush_arrives_with_its_mask() {
        let path = ".:94:68:abcd:data:material_0.layer";
        let bytes = sut(
            &[(
                "Triangle",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 1)),
            )],
            // Black at full alpha: solid ink, which is a mask that paints.
            &[(path, material(6, 4, [0, 0, 0, 255]))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);

        let mask = tool.tip.as_ref().expect("a mask came with it");
        assert_eq!((mask.width(), mask.height()), (6, 4));
        assert_eq!(mask.at(0, 0), 255);
        // A solid stamp is the same mark under either coverage rule, so it does
        // not need build-up — measured rather than assumed.
        assert!(!tool.brush.build_up);
        // The thumbnail is not the material's full resolution, and that is
        // named however well it works out.
        assert!(tool.dropped.contains(&dropped::THUMBNAIL_TIP));
    }

    /// Clip Studio composites every dab, so a stamp whose brightest texel is
    /// half way up builds to solid along a stroke. Under Umber's default `max`
    /// it never could, which would be somebody's brush at half strength.
    #[test]
    fn a_faint_stamp_is_imported_with_build_up() {
        let path = ".:1:data:material_0.layer";
        let bytes = sut(
            &[(
                "Grain",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 1)),
            )],
            // Mid grey at full alpha: every texel about half covered.
            &[(path, material(8, 8, [128, 128, 128, 255]))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        let mask = tool.tip.as_ref().expect("a mask");
        assert!(
            (i32::from(mask.at(0, 0)) - 128).abs() <= 2,
            "{}",
            mask.at(0, 0)
        );
        assert!(tool.brush.build_up);
    }

    /// The two kinds of material disagree about which channel carries the
    /// shape, so coverage has to be both of them.
    #[test]
    fn a_transparent_tip_and_an_opaque_paper_both_become_masks() {
        // A brush tip: black, and the alpha is the shape.
        let tip = ".:tip:data:material_0.layer";
        // A paper: fully opaque, and the luminance is the shape.
        let paper = ".:paper:data:material_0.layer";
        let bytes = sut(
            &[
                (
                    "Tip",
                    Variant::plain(1)
                        .int("BrushUsePatternImage", 1)
                        .set("BrushPatternImageArray", reference(tip, 1)),
                ),
                (
                    "Paper",
                    Variant::plain(2)
                        .int("BrushUsePatternImage", 1)
                        .set("BrushPatternImageArray", reference(paper, 1)),
                ),
            ],
            &[
                (tip, material(4, 4, [0, 0, 0, 200])),
                (paper, material(4, 4, [55, 55, 55, 255])),
            ],
        );
        let tools = from_sut(&bytes).expect("read").tools;
        assert_eq!(tools[0].tip.as_ref().expect("tip").at(1, 1), 200);
        assert_eq!(tools[1].tip.as_ref().expect("paper").at(1, 1), 200);
    }

    /// A mask with nothing dark in it is a brush that paints nothing, which is
    /// a far worse outcome than one that paints round.
    #[test]
    fn a_tip_that_would_paint_nothing_is_dropped_and_named() {
        let path = ".:blank:data:material_0.layer";
        let bytes = sut(
            &[(
                "Ghost",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 1)),
            )],
            &[(path, material(8, 8, [255, 255, 255, 255]))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.tip.is_none());
        assert!(tool.dropped.contains(&dropped::UNUSABLE_TIP));

        // And a reference to a material the file does not carry — Clip Studio
        // leaves an installed one out — reports the same way rather than
        // failing the whole import.
        let bytes = sut(
            &[(
                "Missing",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 1)),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.tip.is_none());
        assert!(tool.dropped.contains(&dropped::UNUSABLE_TIP));
    }

    #[test]
    fn a_brush_that_cycles_several_tips_says_so() {
        let path = ".:many:data:material_0.layer";
        let bytes = sut(
            &[(
                "Leaves",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 4)),
            )],
            &[(path, material(4, 4, [0, 0, 0, 255]))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        // The first one is still bound: one tip is much closer than none.
        assert!(tool.tip.is_some());
        assert!(tool.dropped.contains(&dropped::SEVERAL_TIPS));
    }

    /// Umber's grain is a closed set of three papers, so the strength and the
    /// tile size come across and the picture does not.
    #[test]
    fn a_paper_texture_becomes_strength_and_a_tile_size() {
        let path = ".:paper:data:material_0.layer";
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 80)
                    .real("TextureScale2", 25.0),
            )],
            &[(path, material(8, 8, [128, 128, 128, 255]))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!((tool.brush.grain - 0.8).abs() < 1e-6);
        assert!((tool.brush.grain_scale - 64.0).abs() < 1e-4);
        assert!(tool.brush.has_grain());
        assert!(tool.dropped.contains(&dropped::PAPER_TEXTURE));
    }

    /// Colour pickup is what puts a stroke on the per-dab colour path, and that
    /// costs a second scratch target — so a brush that does not mix must not
    /// end up on it.
    #[test]
    fn only_a_brush_with_mixing_switched_on_picks_colour_up() {
        let bytes = sut(
            &[
                (
                    "Blender",
                    Variant::plain(1)
                        .int("BrushUseWaterColor", 1)
                        // Carries no paint at all: a pure blender.
                        .int("BrushMixAlpha", 0)
                        .int("BrushMixColor", 0)
                        .int("BrushMixColorExtension", 100),
                ),
                (
                    "Oil",
                    Variant::plain(2)
                        .int("BrushUseWaterColor", 1)
                        // A full load of paint, laid down thinly: still mixes.
                        .int("BrushMixAlpha", 100)
                        .int("BrushMixColor", 80)
                        .int("BrushMixColorExtension", 50),
                ),
                (
                    "Pen",
                    // The same numbers with mixing switched off must not mix.
                    Variant::plain(3)
                        .int("BrushUseWaterColor", 0)
                        .int("BrushMixAlpha", 0)
                        .int("BrushMixColor", 80),
                ),
            ],
            &[],
        );
        let tools = from_sut(&bytes).expect("read").tools;

        assert!((tools[0].brush.smudge - 1.0).abs() < 1e-6);
        assert!((tools[0].brush.smudge_length - 0.99).abs() < 1e-6);
        assert!(tools[0].brush.smudges());
        assert!((tools[1].brush.smudge - 0.8).abs() < 1e-6);
        assert!(tools[1].brush.smudges());
        assert_eq!(tools[2].brush.smudge, 0.0);
        assert!(!tools[2].brush.smudges());
        assert!(!tools[2].brush.colours_dabs(), "the fast path must be kept");
        assert!(tools[0].dropped.contains(&dropped::MIXING));
        assert!(!tools[2].dropped.contains(&dropped::MIXING));
    }

    #[test]
    fn features_with_no_engine_behind_them_are_named() {
        let bytes = sut(
            &[("Everything", Variant::plain(1).int("UseDualBrush", 1))],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::DUAL_BRUSH]);
    }

    /// The whole-file answer the import notice uses: one list, no repeats,
    /// however many brushes contributed to it.
    #[test]
    fn the_files_losses_are_gathered_without_repeats() {
        let bytes = sut(
            &[
                ("A", Variant::plain(1).int("UseDualBrush", 1)),
                ("B", Variant::plain(2).int("UseDualBrush", 1)),
                ("C", Variant::plain(3).int("BrushUseWaterEdge", 1)),
            ],
            &[],
        );
        let losses = dropped_features(&bytes);
        assert_eq!(losses, [dropped::DUAL_BRUSH, dropped::WATER_EDGE]);
        // An unreadable file answers with nothing here and fails properly in
        // `from_sut`, so it is never reported on twice.
        assert!(dropped_features(b"not a sub tool").is_empty());
    }

    #[test]
    fn a_tar_member_is_found_past_one_that_is_not_a_round_number_of_blocks() {
        let archive = tar(&[("first", vec![7; 600]), ("wanted", b"the payload".to_vec())]);
        assert_eq!(
            tar_member(&archive, "wanted").as_deref(),
            Some(&b"the payload"[..])
        );
        assert!(tar_member(&archive, "absent").is_none());
        // Truncated and empty archives must answer, not panic.
        assert!(tar_member(&archive[..300], "wanted").is_none());
        assert!(tar_member(&[], "wanted").is_none());
    }

    #[test]
    fn a_reference_names_the_material_row_that_holds_it() {
        let path = ".:94:68:abcd:data:material_0.layer";
        let Value::Blob(blob) = reference(path, 3) else {
            unreachable!("the fixture builds a blob")
        };
        assert_eq!(reference_path(&blob).as_deref(), Some(path));
        assert_eq!(reference_count(&blob), 3);
        // Anything that is not one of these is not resolved into a search.
        assert!(reference_path(b"short").is_none());
        let Value::Blob(other) = reference("just a name", 1) else {
            unreachable!("the fixture builds a blob")
        };
        assert!(reference_path(&other).is_none());
    }
}
