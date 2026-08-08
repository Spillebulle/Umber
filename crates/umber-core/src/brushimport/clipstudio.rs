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
//! [u32 checksum]` tagged `HEAD`, `dATA` and `TAIL`.
//!
//! **A tip and a paper both come from `data/material_0.layer`**, through
//! [`super::csmaterial`], and `thumbnail.png` is the fallback for a material
//! that reader will not guess at or that Clip Studio left out of the file. The
//! thumbnail is a real PNG of the real material and needs no guesswork, at the
//! cost of a longest side of 300 — and, for a paper, of a picture under no
//! obligation to tile. Taking the fallback is named; taking the material is
//! not, because there is then nothing to apologise for.
//!
//! A material reference — `TextureImage`, `BrushPatternImageArray` — is a small
//! blob whose first field is the `OriginalPath` of the row in `MaterialFile`
//! that holds it, so a tip resolves by exact string match and never by
//! searching.
//!
//! # What is dropped
//!
//! The list is in [`dropped`]. The ones worth stating here are the shape of the
//! whole thing rather than a detail:
//!
//! - **Clip Studio's per-setting "effect sources" are a table Umber does not
//!   have.** Every one of them carries a bitmask of which inputs drive it, a
//!   floor per input, and a response curve. Pen pressure, stroke speed and the
//!   per-dab random draw are read. **Pen tilt is named and dropped**, and that
//!   is not because the bit cannot be identified — it can, see [`TILT`] — but
//!   because no platform Umber runs on reports tilt at all, so the modulation
//!   would sit at a value the pen never produces.
//! - **The dab's angle answers to a different list of sources in the same
//!   bits**, and that is the one place a bitmask here means two things. Clip
//!   Studio's *Direction* dynamic offers None, direction of pen, pen tilt,
//!   **direction of line** and random — no velocity in it anywhere. So
//!   `1 << 6` on `BrushRotationEffector` is the stroke's own heading, which is
//!   [`Brush::dab_angle_follows_stroke`] exactly, and reading it as the sweep's
//!   velocity imported every sketching pencil in the sample files as a fixed
//!   nib *and* apologised for a stroke speed Clip Studio cannot drive an angle
//!   with. See the [`DIR_LINE`] constants for what pins each bit.
//! - **Pressure and the random draw driving a setting Umber has no field for
//!   are lost and deliberately not named.** They are as lost as a tilt mapping
//!   is, and reporting them was tried: the sweep cannot tell a live effector
//!   from one whose bits Clip Studio left behind when the setting was switched
//!   off, and those two bits are set on far more columns than tilt or velocity
//!   ever are — so the sentence appeared on nearly every import and was often
//!   about a mapping the brush does not have. [`unreachable_inputs`] has the
//!   whole argument. Silence beats a false apology until the enable flag beside
//!   each effector can be read.
//! - **A floor is part of a mapping and is carried with it.** Clip Studio
//!   states each dynamic's minimum as a percentage of the setting's own value,
//!   and for size that is [`Brush::min_size_ratio`] exactly. Opacity has no
//!   such field, so the floor is folded into the response curve, where it is
//!   exact — see [`floored`]. Dropping it is a brush that paints from nothing
//!   where its author had it painting from six tenths, which looks like an
//!   opacity setting that does not work.
//! - **The taper arrives at the start of a stroke and not at its end.** "In"
//!   is a size ramp over the first stretch of the mark, which is precisely what
//!   [`DabInput::Stroke`] is. "Out" is measured back from an end the engine
//!   does not know until the stroke is over.
//! - **The paper texture arrives as a picture, at the material's own
//!   resolution, into the user's texture library.** It used to become one of
//!   Umber's three, on the reasoning that the grain is a closed set — which it
//!   still is; what changed is that [`crate::BrushPreset::paper`] can name a
//!   tile beside it. Substituting inside the closed set was wrong because grain
//!   **multiplies coverage**: `Tooth`'s mean is 0.775, so every textured brush
//!   arrived painting at about 78% of the opacity its author set, through pits
//!   nobody drew. A texture this reader cannot resolve is named as a loss and
//!   the brush paints flat. It is read only where the reference actually names
//!   a material: a stale reference left behind by a texture that was switched
//!   off would be a brush painting through paper it does not have. The tile
//!   size is the material's own size times `TextureScale2`, which is what that
//!   percentage means — see `GRAIN_TILE_AT_FULL_SCALE` for the figure that
//!   stands in when the material could not be read, and for what it used to
//!   cost when it stood in for every case. A paper that cannot be resolved at
//!   all paints **flat**, strength and picture both, because leaving the
//!   strength behind falls back to `Brush::default()`'s `Tooth`.
//! - **Dual brushes, watercolour edges, colour jitter and the vector settings
//!   have no engine behind them at all** and are named. The dual brush is the
//!   largest of the four and the one whose column family invites a guess: a
//!   parallel copy of the *whole* brush — `DualSize`, `DualFlow`,
//!   `DualHardness`, `DualInterval`, `DualRotation`, `DualPatternImageArray`,
//!   a complete `DualTexture*` block and a complete `DualSpray*` one — which is
//!   Clip Studio's `2-Brush tip`, `2-Spray effect`, `2-Stroke` and `2-Paper
//!   quality` under `2-Brush shape`, a second brush stamped on top of the first
//!   at the same time. Four columns are about the pairing rather than copies of
//!   it: `UseDualBrush`, `DualBrushCompositeMode` (thirteen modes, of which
//!   Height (Linear) exists only here), `SyncDualBrushSize` ("Link to main
//!   brush size") and `ChangeRGBByDual` ("Apply RGB value"). Umber binds one
//!   tip and one paper per brush, so there is no half of this worth painting.
//!   **`UseDualBrush` is the field that says whether any of it is live**, and
//!   it is zero on all thirty variants of both sample files while stale values
//!   sit beside it — see
//!   `a_dual_brush_that_is_switched_off_is_not_reported_from_the_values_left_
//!   beside_it` for the residue and for why no neighbour may stand in for the
//!   flag.
//! - **A sub-tool that is not a brush is skipped without a word.** A `.sutg` is
//!   a tool group and a group holding a fill or a selection tool is the
//!   ordinary case; a note about it would appear on nearly every import ever
//!   made and teach the reader to skip the list that carries the rest.
//!
//! Nothing here is refused for being approximate: this is a user's own import,
//! which `CLAUDE.md` holds to a different standard than the shipped library —
//! a usable approximation that says what it lost beats a rejection.

use crate::brush::Brush;
use crate::brushimport::csmaterial;
use crate::curve::ResponseCurve;
use crate::dynamics::{DabInput, DabTarget, Modulation};
use crate::preset::PresetError;
use crate::sqlite::{Database, Row, Table, Value};
use crate::tip::{self, TipMask, stroke_coverage};

/// Every loss this importer can report, in one place.
///
/// Constants rather than literals at the call site because several are pushed
/// from more than one place, and because a list of what an importer knows it
/// cannot do is worth being able to read in one screen.
pub mod dropped {
    /// The tip is the material's thumbnail, not its full-resolution pixels.
    ///
    /// Named only where the fallback was actually taken — see `tip_for`. It
    /// used to be named on every brush that had a tip at all, which was true
    /// when the thumbnail was the only route and is a false apology now that
    /// it is the second one.
    pub const THUMBNAIL_TIP: &str = "bitmap tips at their full resolution";
    /// The material was read whole and is larger than the engine can stamp.
    ///
    /// It names the *strength* as well as the size, and that is not padding:
    /// reducing a stamp that is not solid lowers its peak, and a `max` stroke
    /// is capped at the mask's own brightest texel. `TipMask::reduced`'s docs
    /// have the argument, and `stroke_coverage` runs on the reduced mask so
    /// that a stamp thinned far enough arrives with `build_up` set. No figure
    /// is quoted, because `TipMask::MAX_SIZE` is the one that decides it and a
    /// number written here would go stale the moment it moved.
    pub const REDUCED_TIP: &str =
        "a bitmap tip larger than Umber can stamp (reduced to fit, which softens it)";
    pub const SEVERAL_TIPS: &str = "brushes that cycle through several tip images";
    /// The material was named and its picture could not be turned into a tile.
    /// The strength and the tile size are still the author's numbers, but the
    /// paper itself is gone and the brush paints flat — the same answer
    /// [`UNUSABLE_TIP`] gives for the analogous tip, and for the same reason:
    /// a substituted paper is a grain nobody drew, and substituting one is what
    /// made an imported brush paint at 78% of the opacity it was set to.
    ///
    /// **A noun phrase, with no comma and no "so"**, like every other constant
    /// here. `brushlib`'s notice joins these into "Umber could not bring across
    /// A, B and C, so it will paint differently", so a comma inside one reads
    /// as a fourth item and a "so" inside one collides with the frame's own.
    pub const PAPER_TEXTURE: &str = "the paper texture's own picture";
    /// The paper that arrived is the material's thumbnail, not its full pixels
    /// — [`THUMBNAIL_TIP`]'s twin, and named on the same terms: **only where
    /// the fallback was actually taken**, which since `paper_for` learned to
    /// read [`super::csmaterial`] is the rarer half.
    ///
    /// It is the sharper of the two losses. The tile size comes from the file's
    /// own `TextureScale2`, which is a percentage of the *material's* size, so
    /// a thumbnail is stretched to the author's spatial frequency at a fraction
    /// of the author's detail: soft blotches where there was tooth. And a
    /// preview render is under no obligation to **tile**, where the material
    /// itself declares `isTiling` and does — measured on the sample files, the
    /// 500 × 500 paper joins to itself within its own noise and its 300 × 300
    /// thumbnail steps by 62 levels across the join against an interior figure
    /// of 2.9, which the browser reports as a grid. That grid was the preview's
    /// and never the author's.
    pub const THUMBNAIL_PAPER: &str = "paper textures at their full resolution";
    /// The paper material was read whole and is larger than a tile may be.
    ///
    /// [`REDUCED_TIP`]'s twin and reduced by the same function, but what it
    /// costs is different: a stamp loses its peak, where a paper loses its
    /// **tooth** — the pits and peaks average towards the middle, so the grain
    /// bites more evenly and more weakly than the author drew it.
    pub const REDUCED_PAPER: &str =
        "a paper texture larger than Umber can hold at full resolution (reduced to fit)";
    /// `Brush::MAX_GRAIN_SCALE` — or, far more rarely, `MIN_GRAIN_SCALE` — cut
    /// the tile the file asked for.
    ///
    /// **Distinct from [`REDUCED_PAPER`] and it must stay distinct**, because
    /// the two fire on almost the same condition and mean opposite things. That
    /// one is the *picture* being coarser than it was drawn; this is the
    /// picture being repeated at a **finer pitch** than the author set — a
    /// 4096-texel paper at 100% asks for a 4096-pixel tile and gets 2048, so
    /// the grain recurs twice as often across the canvas. Folding it into
    /// `REDUCED_PAPER` would put a notice about softening over a change of
    /// spatial frequency.
    pub const PAPER_SPACING: &str = "the paper texture's own spacing";
    /// Umber has no tilt input on any platform it runs on, so a tilt mapping
    /// could only ever be evaluated at a value the pen never reports.
    pub const TILT_INPUT: &str = "settings driven by pen tilt";
    /// A fifth effect source that every setting declares support for and that
    /// nothing in the sample files ever switches on. Named rather than guessed.
    pub const UNKNOWN_INPUT: &str = "settings driven by an effect source this reader cannot name";
    /// Stroke speed reaches size and per-dab opacity; on anything else there is
    /// no Umber setting for it to drive.
    ///
    /// It is **not** what the dab's angle answers to — see the `DIR_*`
    /// constants. Reading that column in this vocabulary is what put this
    /// sentence on every brush whose tip merely follows the line.
    pub const SPEED_ELSEWHERE: &str = "stroke speed driving a setting Umber has no equivalent for";
    /// The brush names a paper and this reader could not read the reference.
    /// The strength and the tile size are still in the file; applying them to
    /// one of Umber's papers would be a grain invented out of a reference
    /// nobody could resolve, so it is named instead — the same answer
    /// [`UNUSABLE_TIP`] gives for the analogous tip.
    pub const UNREADABLE_TEXTURE: &str = "a paper texture this reader could not resolve";
    pub const MIXING: &str = "the detail of Clip Studio's underlying-colour mixing";
    pub const DUAL_BRUSH: &str = "dual brushes";
    pub const WATER_EDGE: &str = "watercolour edges";
    /// The taper *in* is imported as a stroke-position ramp. The taper out
    /// cannot be: it is measured back from an end the engine does not know
    /// until the stroke is over.
    pub const TAPER_OUT: &str = "the stroke's taper at its end";
    pub const COLOUR_JITTER: &str = "per-dab hue, saturation and brightness shifts";
    pub const RIBBON: &str = "ribbon and continuous-image strokes";
    pub const BLEND_MODE: &str = "a blending mode set on the brush itself";
    pub const SPRAY_SHAPE: &str = "the spray's particle count and bias";
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
    /// The paper this sub-tool paints through, where the texture material was
    /// in the file. See [`crate::brushimport::Imported::paper`].
    pub paper: Option<TipMask>,
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
        //
        // Deliberately **not** reported. A `.sutg` is a whole tool group and
        // legitimately holds fill and selection tools; saying so on every group
        // ever imported is how a reader learns to skip the list that carries
        // the losses that matter.
        if settings.real("BrushSize").is_none() {
            continue;
        }

        let name = node_name(&node_table, node);
        let converted = convert(&settings, &materials);
        file.tools.push(SubTool {
            name,
            brush: converted.brush,
            tip: converted.tip,
            paper: converted.paper,
            dropped: converted.dropped,
        });
    }

    if file.tools.is_empty() {
        return Err(malformed(
            "it holds no brushes. Every sub-tool in it is a fill, selection or shape tool"
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
/// **The dab's angle is the one exception and has its own list in the same
/// bits** — see [`DIR_LINE`]. Everything else is the ordinary Dynamics dialog:
///
/// Clip Studio's Dynamics dialog lists its effect sources as **Pen pressure,
/// Tilt, Velocity, Random**, and the bits are that list from bit 4 upwards.
/// Three things agree on it:
///
/// - Bit 4 is the only bit set on the size effector of every pressure-sensitive
///   brush in the sample files, and bit 7 is the only bit ever set on the hue,
///   saturation and brightness effectors — which is colour jitter, so random.
///   The two ends of the list pin the order of the middle.
/// - `usedFlag` in Ken Evans' `CSPBrushInfo`, an independent decoding of these
///   same blobs, reads `0x10` pressure, `0x20` tilt, `0x40` velocity, `0x80`
///   random.
/// - The `sup` word says every setting supports exactly these four (`0xf0`),
///   and brush size and its neighbours one more (`0x1f0`) that nothing in
///   either sample file ever switches on. That fifth is [`UNKNOWN`].
///
/// Velocity is Umber's [`DabInput::Speed`]. Tilt is not anything: no platform
/// Umber runs on reports it, so a tilt mapping would be a modulation that can
/// never fire — see [`dropped::TILT_INPUT`].
const PRESSURE: u32 = 1 << 4;
const TILT: u32 = 1 << 5;
const VELOCITY: u32 = 1 << 6;
const RANDOM: u32 = 1 << 7;
const UNKNOWN: u32 = 1 << 8;

/// Which input drives the **dab's angle** — a different list, in the same bits.
///
/// This is the one setting whose sources are not the four above, and reading it
/// as though they were is a mistake with a visible cost at both ends. Clip
/// Studio's *Direction* dynamic has its own dialog, and the manual lists it as
/// **None, Direction of pen, Pen tilt, Direction of line, Random** — with no
/// velocity anywhere in it. So `1 << 6` on `BrushRotationEffector` is not
/// stroke speed; it is **Direction of line**, and that is
/// [`Brush::dab_angle_follows_stroke`] exactly. Umber used to import such a
/// brush as a fixed nib and apologise for a stroke speed Clip Studio cannot
/// drive an angle with — a rake arriving as a ruling pen, under a sentence that
/// sent the reader looking for a feature that was never the problem.
///
/// One bit is anchored in the sample files and the rest follow the dialog's
/// order, which is a **weaker footing than the four above** and is said so
/// here rather than left for somebody to discover:
///
/// - `1 << 7` is random, and it is the bit that carries an amount. Of the
///   thirteen brushes, the eight without it hold `BrushRotationRandomScale` at
///   its untouched 100 and not one holds anything else, while four of the five
///   with it hold a deliberate 45 or 10. The correlation runs one way — the
///   fifth sets the bit and leaves the amount at 100, which is a full turn and
///   a legitimate setting — so what it pins is that **nobody sets the amount
///   without the bit**. That is also the one bit this reader already had right,
///   so the jitter it imports is unchanged.
/// - Random being last then puts the other three sources on bits 4, 5 and 6 in
///   the dialog's own order, which is the whole of the argument for `1 << 6`.
///   It sits on four elongated, textured sketch pencils, one of them leaning
///   45° off the line — a reading a painter would recognise, and **not proof**:
///   the same four are a plausible pen-tilt brush too, so if the manual's order
///   is not the file's order, `1 << 6` is pen tilt and those four import as
///   rakes that should be nibs. That is the way this change can be wrong, and
///   it is a wrong *mark* where the bug it replaces was only a wrong note.
/// - `1 << 5` then falls on the two flat brushes, both 30% thick and stated at
///   90°, and driving a flat marker's angle from pen tilt is a stock Clip
///   Studio recipe. `1 << 4` falls on three round brushes; "Direction of pen"
///   is the *azimuth* of the tilt rather than its amount, so it is still tilt
///   and still something no platform Umber runs on reports.
/// - `1 << 8` is **never set in either file**, so it keeps
///   [`dropped::UNKNOWN_INPUT`] rather than being named. A later Clip Studio
///   adds "Rotation of pen axis" to this dialog and appending it is the reading
///   that fits — but that is an inference about a version and an insertion
///   point, stacked on the inference above, for a bit nobody has observed. The
///   old sentence is *literally* true meanwhile, which is this module's own
///   rule; naming it costs nothing to defer and cannot be taken back.
const DIR_PEN: u32 = 1 << 4;
const DIR_TILT: u32 = 1 << 5;
const DIR_LINE: u32 = 1 << 6;
const DIR_RANDOM: u32 = 1 << 7;
const DIR_PEN_AXIS: u32 = 1 << 8;

/// One `*Effector` blob.
///
/// Big-endian throughout, unlike the container it sits in:
///
/// ```text
/// u32  44           length of this record
/// u32               which inputs this setting supports
/// u32               which are switched on
/// i32               the floor under pen pressure, as a percentage
/// i32               the floor under tilt
/// i32               the floor under velocity
/// i32               the floor under the random draw
/// i32               unread
/// u32               bytes of the first curve record, or zero
/// u32               bytes of the second curve record, or zero
/// i32               tilt's ceiling, which is the one input that may exceed 100
/// -- then each curve record that the two lengths above declare --
/// u32  12           length of the curve header
/// u32               how many control points
/// u32  16           bytes per point
/// (f64, f64) x n    the points, x then y, both 0..1
/// ```
///
/// **There are only ever two curve records**: the first belongs to pen
/// pressure and the second to whichever of tilt and velocity is switched on.
/// A `.sut` written by a brush that has been edited keeps the second record
/// after its source is switched off, so its presence proves nothing — which is
/// why [`Effector::curve_for`] asks what is *enabled* before it reads one.
#[derive(Clone, Debug)]
struct Effector {
    enabled: u32,
    /// Floor for each of pressure, tilt, velocity and random, in that order.
    minimums: [i32; 4],
    /// The pressure curve, and the tilt-or-velocity curve.
    curves: [Vec<(f64, f64)>; 2],
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
        let mut minimums = [0i32; 4];
        for (i, slot) in minimums.iter_mut().enumerate() {
            *slot = word(12 + i * 4) as i32;
        }

        // The two curve records sit end to end after the header, and either may
        // be absent — which is what the declared lengths are for. Walking them
        // rather than assuming the first record is at 44 is what tells the
        // pressure curve apart from the one beside it.
        let mut curves = [Vec::new(), Vec::new()];
        let mut at = 44usize;
        for (slot, length) in curves
            .iter_mut()
            .zip([word(32) as usize, word(36) as usize])
        {
            if length == 0 {
                continue;
            }
            let Some(record) = bytes.get(at..at.saturating_add(length)) else {
                break;
            };
            at += length;
            if record.len() < 12 {
                continue;
            }
            let count = u32::from_be_bytes(record[4..8].try_into().expect("four bytes")) as usize;
            for i in 0..count {
                let Some(pair) = record.get(12 + i * 16..12 + i * 16 + 16) else {
                    break;
                };
                let x = f64::from_be_bytes(pair[..8].try_into().expect("eight bytes"));
                let y = f64::from_be_bytes(pair[8..].try_into().expect("eight bytes"));
                if x.is_finite() && y.is_finite() {
                    slot.push((x, y));
                }
            }
        }

        Some(Self {
            enabled,
            minimums,
            curves,
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

    /// The curve for one input, as one of Umber's fixed-sample response curves.
    ///
    /// Piecewise linear between the points and held flat outside them, which is
    /// what the curve editor draws. Clip Studio's own interpolation is smoother
    /// than that between widely spaced points; five evenly spaced samples is
    /// the resolution [`ResponseCurve`] has, so the difference is below what it
    /// could record anyway.
    ///
    /// The second record is shared by tilt and velocity, so a brush driven by
    /// **both** cannot say which of them the curve belongs to. That brush gets
    /// a straight line rather than a curve that might be the other input's:
    /// the range still carries how far the setting travels, which is the part
    /// that shows.
    fn curve_for(&self, input: u32) -> ResponseCurve {
        let points = match input {
            PRESSURE => &self.curves[0],
            _ if self.drives(TILT) && self.drives(VELOCITY) => return ResponseCurve::LINEAR,
            _ => &self.curves[1],
        };
        if points.len() < 2 {
            return ResponseCurve::LINEAR;
        }
        let mut curve = ResponseCurve::LINEAR;
        for i in 0..ResponseCurve::N {
            let x = f64::from(ResponseCurve::x_of(i));
            curve.set(i, at(points, x) as f32);
        }
        curve
    }
}

fn at(points: &[(f64, f64)], x: f64) -> f64 {
    let first = points[0];
    if x <= first.0 {
        return first.1;
    }
    for pair in points.windows(2) {
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
    points.last().expect("at least two points").1
}

/// The effect sources on this brush that Umber will not reproduce.
///
/// Every `*Effector` column is swept rather than the handful this importer
/// reads, because the question is what the *brush* does and not what this
/// function happens to look at — a setting Umber has no field for at all is
/// still a setting whose behaviour will not arrive. `driven` names the columns
/// whose velocity mapping *was* imported, so speed is only reported where it
/// really had nowhere to go.
///
/// The dab's angle is asked separately and in **its own vocabulary**, because
/// the Direction dynamic's sources are not these four — see the `DIR_*`
/// constants. It is not in the sweep either way: it is stored as an integer
/// rather than as a record, so [`Settings::effector`] declines it.
///
/// **Only tilt, the unnamed fifth source and stray velocity are reported, and
/// pressure and the random draw deliberately are not** — even though a mapping
/// of either onto a setting Umber has no field for is just as lost. Reporting
/// them was tried and is wrong here for two compounding reasons. The sweep is
/// over a schema of 187 to 214 columns and cannot tell an effector whose bits
/// are live from one whose bits are *stale*, and Clip Studio leaves a setting's
/// value in the file when the setting is switched off — the trap the taper, the
/// angle jitter, the spacing and the paper each read a separate field to avoid.
/// Pressure and randomness are set on far more of those columns than tilt or
/// velocity ever are, so the result was a sentence appearing on nearly every
/// import, frequently about a mapping the brush does not have. `docs/brushes.md`
/// records that the random bit is the *only* bit ever set on the hue,
/// saturation and brightness effectors in either sample file — so a brush with
/// colour jitter switched off would have apologised for it, and one with it
/// switched on would have said the same loss twice, once vaguely.
///
/// A list that cries wolf is one a reader learns to skip, which costs the
/// losses that do matter — the same argument the skipped fill tool and the
/// automatic dab interval already make. Naming these properly needs the enable
/// flag beside each effector, which means knowing what those columns are
/// called; until then silence beats a false apology.
fn unreachable_inputs(
    settings: &Settings,
    driven: &[&str],
    angle_shows: bool,
) -> Vec<&'static str> {
    let mut out = Vec::new();
    let mut note = |sources: u32, column: &str| {
        if sources & TILT != 0 {
            push_once(&mut out, dropped::TILT_INPUT);
        }
        if sources & UNKNOWN != 0 {
            push_once(&mut out, dropped::UNKNOWN_INPUT);
        }
        if sources & VELOCITY != 0 && !driven.contains(&column) {
            push_once(&mut out, dropped::SPEED_ELSEWHERE);
        }
    };

    for name in settings.table.columns() {
        // The dual brush's own are not consulted: the whole dual brush is
        // dropped and already says so.
        if !name.ends_with("Effector") || name.starts_with("Dual") {
            continue;
        }
        if let Some(effector) = settings.effector(name) {
            note(effector.enabled, name);
        }
    }

    // Rotation states its sources as a bare integer rather than as a record, so
    // it is not in the sweep above and has to be asked separately — and it
    // answers in **its own vocabulary**, which is why it is read here rather
    // than handed to `note`. See the `DIR_*` constants: the Direction dialog
    // offers no velocity at all, so putting this column through the sweep's
    // reading reported a stroke speed that was really a stroke *direction* —
    // the one source of that apology in both sample files, and Umber now paints
    // it instead of naming it. Its low bits are its own and are not effect
    // sources.
    //
    // `angle_shows` is why this takes an argument at all. A round dab with no
    // tip looks identical at every angle, so a direction source on one is a
    // setting whose absence cannot be seen, and reporting it is the cry-wolf
    // failure the rest of this function spends two paragraphs refusing. Two of
    // the thirteen brushes in the sample files are exactly that — round,
    // untipped, and naming a tilt direction — and the first draft of this
    // reading gave both of them an apology they had no use for.
    let rotation = settings.int("BrushRotationEffector").unwrap_or(0) as u32;
    if angle_shows {
        // Both of the first two are tilt: one is how far the pen leans and the
        // other is which way, and no platform Umber runs on reports either.
        if rotation & (DIR_PEN | DIR_TILT) != 0 {
            push_once(&mut out, dropped::TILT_INPUT);
        }
        // Never observed set. Kept under the honest name until it is — see the
        // `DIR_PEN_AXIS` bullet.
        if rotation & DIR_PEN_AXIS != 0 {
            push_once(&mut out, dropped::UNKNOWN_INPUT);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Materials
// ---------------------------------------------------------------------------

/// The bitmap materials in a file, indexed by the path a reference names.
#[derive(Default)]
struct Materials {
    by_path: Vec<(String, Vec<u8>)>,
    /// What [`csmaterial::from_archive`] answered for each archive already
    /// asked about, including the `None`s.
    ///
    /// **A group shares its materials**, and heavily: in the sample `.sutg`
    /// four of the thirteen sub-tools name one archive and three name another.
    /// Reading a material is a tar walk, a page scan that materialises every
    /// row's blob, a zlib inflate per block and a canvas-sized allocation — so
    /// without this a group of fifty brushes cut from one 1174 × 1120 stamp
    /// pays all of that fifty times, where the thumbnail it replaced was one
    /// small PNG decode. An import is not a drawing path, so this is somebody
    /// waiting rather than a dropped frame; it is still the difference between
    /// a moment and a minute.
    ///
    /// Keyed by the path rather than by the archive, because that is what a
    /// reference names and what `by_path` has already made unique.
    read: std::cell::RefCell<Vec<(String, Option<csmaterial::Material>)>>,
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
        Self {
            by_path,
            read: std::cell::RefCell::new(Vec::new()),
        }
    }

    /// The tar archive a reference blob points at.
    fn resolve(&self, reference: &[u8]) -> Option<&[u8]> {
        let path = reference_path(reference)?;
        self.by_path
            .iter()
            .find(|(key, _)| *key == path)
            .map(|(_, bytes)| bytes.as_slice())
    }

    /// The full-resolution pixels a reference names, read once per file.
    fn pixels(&self, reference: &[u8]) -> Option<csmaterial::Material> {
        let path = reference_path(reference)?;
        if let Some((_, cached)) = self.read.borrow().iter().find(|(key, _)| *key == path) {
            return cached.clone();
        }
        let material = self.resolve(reference).and_then(csmaterial::from_archive);
        self.read.borrow_mut().push((path, material.clone()));
        material
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
pub(super) fn tar_member(archive: &[u8], wanted: &str) -> Option<Vec<u8>> {
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

/// Decode a material's thumbnail to straight-alpha, sRGB RGBA8, for the paper
/// reading.
///
/// **Not shared with [`mask_from_thumbnail`], which keeps its own decoder**, and
/// that is worth stating because it looks like an oversight. The tip reading is
/// `alpha × (1 − Rec.601 luma)` computed in place; this one has to hand whole
/// pixels to [`crate::tip::grain_of`], which is the *shared* rule — the one an
/// interactive paper import goes through as well, and Rec.709 like every other
/// luminance in `umber-core`. Merging the two would mean choosing one set of
/// luma weights for both, which is a change to what every existing tip import
/// produces, for no gain: on the neutral grey a paper material actually is, the
/// two agree exactly.
///
/// `Transformations::ALPHA` is what lets the match below cover only two colour
/// types; a shape that guessed at an alpha for the other three would be the
/// silent kind of wrong. `a_greyscale_paper_material_decodes` pins that the
/// expansion really happens, because a real material's thumbnail is as likely
/// to be greyscale as RGBA.
fn thumbnail_rgba(png_bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_bytes));
    // Expands a palette or a low bit depth, and 16-bit down to 8, and adds the
    // alpha channel every reading below wants, so one shape covers all five
    // colour types.
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut buffer = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buffer).ok()?;

    let texels = (info.width as usize).checked_mul(info.height as usize)?;
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buffer[..texels * 4].to_vec(),
        png::ColorType::GrayscaleAlpha => buffer[..texels * 2]
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        // `Transformations::ALPHA` gives every other type one, so these are
        // unreachable rather than approximated — and a shape that guessed at
        // an alpha would be the silent kind of wrong.
        _ => return None,
    };
    Some((info.width, info.height, rgba))
}

/// Turn a material's thumbnail into a paper tile.
///
/// [`crate::tip::grain_of`]'s rule, not a second statement of it: brightness,
/// with transparency composited over white. That matters more than it looks —
/// the tip reading immediately below is very nearly its negative, and a paper
/// read as ink bites exactly where its author drew a peak.
fn paper_from_thumbnail(png_bytes: &[u8]) -> Option<TipMask> {
    let (width, height, rgba) = thumbnail_rgba(png_bytes)?;
    TipMask::new(width, height, crate::tip::grain_of(&rgba)).ok()
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

/// A mask a material reference resolved to, and the one thing it may have cost.
struct ResolvedTip {
    mask: TipMask,
    /// At most one, because the two cannot both happen: a thumbnail's longest
    /// side is 300 and the cap it would have to breach is [`TipMask::MAX_SIZE`].
    lost: Option<&'static str>,
}

/// The mask a material reference resolves to, if there is one.
///
/// **The material's own pixels first, its thumbnail second.** Both are in the
/// archive; the first is what the artist drew and the second is a preview of
/// it with a longest side of 300, which for the spatter brush in the sample
/// files is a fifteenth of the area. [`csmaterial`] has the route and the
/// cases where it answers nothing — a material shape it will not guess at, or
/// one Clip Studio left out of the file — and the thumbnail is what those fall
/// back to, which is what this reader did before it existed.
///
/// Nothing about the *reading* changes with the route.
/// [`csmaterial::Material::coverage`] is measured against
/// [`mask_from_thumbnail`]'s own answer, material by material, so a brush that
/// takes the fallback is the same stamp at a coarser resolution rather than a
/// different one.
fn tip_for(reference: &[u8], materials: &Materials) -> Option<ResolvedTip> {
    let archive = materials.resolve(reference)?;
    if let Some(material) = materials.pixels(reference) {
        // A material may be larger than the engine can stamp — see
        // `TipMask::reduced` for why the ceiling is not simply raised. Reduced
        // rather than refused, because the alternative here is not the mask on
        // disk but a 300-pixel preview of it.
        if let Ok((mask, reduced)) =
            TipMask::reduced(material.width, material.height, material.coverage)
        {
            return Some(ResolvedTip {
                mask,
                lost: reduced.then_some(dropped::REDUCED_TIP),
            });
        }
    }
    let png_bytes = tar_member(archive, "thumbnail/thumbnail.png")?;
    Some(ResolvedTip {
        mask: mask_from_thumbnail(&png_bytes)?,
        lost: Some(dropped::THUMBNAIL_TIP),
    })
}

/// A paper tile a texture reference resolved to.
struct ResolvedPaper {
    tile: TipMask,
    /// The material's own longest side in texels, **before** any reduction, or
    /// `None` where the thumbnail stood in.
    ///
    /// This is what `TextureScale2` is a percentage of, which is why it is
    /// carried out rather than taken off `tile`: a reduced tile is meant to
    /// cover the same document ground as the material it came from, and a
    /// thumbnail's longest side is capped at 300 and therefore says nothing
    /// about the picture's. See `GRAIN_TILE_AT_FULL_SCALE` for the figure that
    /// stands in when this is absent — and `dropped::PAPER_SPACING` for the one
    /// case where covering the same ground is not possible, because
    /// [`Brush::MAX_GRAIN_SCALE`] is smaller than the tile the file asked for.
    ///
    /// The *longest* side, because [`Brush::grain_scale`] is one number and the
    /// dab pass divides the document position by it on both axes — so a
    /// non-square paper is stretched square whatever is chosen here, and the
    /// long axis is the one `Brush::size` already sets the precedent for. Both
    /// papers in the sample files are square.
    native: Option<u32>,
    lost: Option<&'static str>,
}

/// The paper tile a material reference resolves to, if there is one.
///
/// **The material's own pixels first, its thumbnail second**, exactly as
/// [`tip_for`] does and for one reason more: the tile size is derived from the
/// material's size, and a preview is under no obligation to tile. See
/// [`dropped::THUMBNAIL_PAPER`].
///
/// The material's coverage plane is **complemented** on the way in, and getting
/// that backwards inverts somebody's paper. [`csmaterial::Material::coverage`]
/// is *ink* — it is measured against [`mask_from_thumbnail`]'s
/// `alpha × (1 − luma)` material by material, to a mean absolute 0.0002..0.068
/// of a level — while a grain texel is the fraction of the dab that **stays**.
/// The two are exact complements rather than merely opposite in spirit: for
/// straight-alpha pixels over white, `1 − a(1 − L)` is `(1 − a) + aL`, which is
/// [`crate::tip::grain_of`] written out, so the composite-over-white that rule
/// insists on comes across for free. On the neutral grey a paper material is,
/// Rec. 601 and Rec. 709 agree exactly, so the two routes into a tile do not
/// disagree about a texel either.
fn paper_for(reference: &[u8], materials: &Materials) -> Option<ResolvedPaper> {
    let archive = materials.resolve(reference)?;
    if let Some(csmaterial::Material {
        width,
        height,
        coverage,
    }) = materials.pixels(reference)
    {
        // `into_iter` rather than `iter`, so the complement is written back
        // into the buffer the cache already handed over rather than allocating
        // a third copy of a picture that can be four megabytes.
        let grain = coverage.into_iter().map(|v| 255 - v).collect();
        if let Ok((tile, reduced)) = TipMask::reduced(width, height, grain) {
            return Some(ResolvedPaper {
                tile,
                native: Some(width.max(height)),
                lost: reduced.then_some(dropped::REDUCED_PAPER),
            });
        }
    }
    let png_bytes = tar_member(archive, "thumbnail/thumbnail.png")?;
    Some(ResolvedPaper {
        tile: paper_from_thumbnail(&png_bytes)?,
        native: None,
        lost: Some(dropped::THUMBNAIL_PAPER),
    })
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/// Clip Studio's interval is a percentage of the brush size, the same unit
/// GIMP's `.gbr` and `.vbr` state theirs in.
const INTERVAL_PER_CENT: f32 = 100.0;

/// The tile a hundred per cent means for a texture whose material could not be
/// read.
///
/// Umber's grain tile is stated in document pixels and Clip Studio's
/// `TextureScale2` is a percentage of the material's **own** size, so the
/// honest conversion is that size times the percentage — and
/// [`ResolvedPaper::native`] now carries it, because the material's pixels are
/// read rather than its 300-pixel preview. This constant is what stands in
/// where they are not: Umber's own default tile, which keeps the *relative*
/// coarseness of two brushes out of one file right even where neither matches
/// Clip Studio's absolute size.
///
/// It used to be the answer in every case, and on the sample files that put the
/// Sketch brushes' paper at 256 × 0.19 ≈ 49 document pixels where their 500 ×
/// 500 material at 19% is 95 — a paper twice as fine as its author's, which on
/// a 6-pixel pencil is the difference between tooth and a wash.
const GRAIN_TILE_AT_FULL_SCALE: f32 = 256.0;

/// Dabs a second for a brush set to keep spraying while the pen is held still.
///
/// Clip Studio states that as a flag and takes the rate from its own timer,
/// which is not in the file. Zero — the alternative — is an airbrush that stops
/// the moment the hand does, which is the one thing an airbrush must not do.
const HELD_SPRAY_RATE: f32 = 30.0;

/// The smallest size a modulation may state, as a log offset on the radius.
///
/// [`DabTarget::Size`]'s own editor range is ±2, and a taper or a velocity
/// mapping that reaches zero cannot be written as a log at all. Clamping here
/// rather than letting `ln(0)` through keeps the stored number one the brush
/// editor can draw and drag, and `exp(-2)` is a dab an eighth of its width —
/// a point as far as a stroke is concerned.
const MIN_SIZE_LOG: f32 = -2.0;

/// The settings whose velocity mapping has an Umber target, and which one.
///
/// One table rather than a branch per column, because
/// [`unreachable_inputs`] has to name exactly the settings this does *not*
/// cover — two lists would drift into a brush that both imports its speed
/// mapping and apologises for it.
///
/// Clip Studio's Opacity is the whole stroke's and its Flow is one dab's, and
/// they multiply; Umber reaches both through per-dab coverage, and the
/// modulation table composes opacity by multiplication for the same reason.
const SPEED_TARGETS: [(&str, DabTarget); 3] = [
    ("BrushSizeEffector", DabTarget::Size),
    ("BrushOpacityEffector", DabTarget::Opacity),
    ("BrushFlowEffector", DabTarget::Opacity),
];

/// [`Brush::stroke_hold`] at MyPaint's own ceiling, which the stroke builder
/// reads as "the input reaches 1 and stays there". A taper happens once at the
/// start of a mark; a ramp that wrapped would be a row of tapers.
const NEVER_WRAP: f32 = 10.0;

/// What [`convert`] made of one sub-tool's settings.
///
/// A struct rather than the tuple this used to be, because it now carries two
/// `Option<TipMask>`s side by side: a caller can hand a tuple's pair over the
/// wrong way round and the compiler will not say a word, and a paper stamped as
/// a tip is a brush shaped like a sheet of paper.
struct Converted {
    brush: Brush,
    tip: Option<TipMask>,
    paper: Option<TipMask>,
    dropped: Vec<&'static str>,
}

fn convert(settings: &Settings, materials: &Materials) -> Converted {
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

    // ---- per-dab density, as a stroke opacity --------------------------
    // `BrushFlow` is Clip Studio's Density: the alpha **one dab** deposits,
    // where `Opacity` is the whole stroke's. Two comments in this file already
    // said the two multiply — `SPEED_TARGETS`' and the pressure block's below —
    // and the code multiplied only their *pressure curves*, discarding the
    // constant entirely. `BrushFlow` was read nowhere at all, and
    // `VARIANT_COLUMNS` could not even hold it, so no fixture could ask.
    //
    // It is live on every brush: Clip Studio has no "use density" switch to
    // leave a stale value behind, which is why this is read unconditionally
    // where the taper and the texture are gated. In `Used.sutg` three of
    // thirteen sub-tools set it below full — 20, 44 and 80 — so "Soft 2"
    // arrived painting at five times the density its author chose. That is the
    // paper bug's mirror image: a brush too *strong* rather than too weak, and
    // a brush already at full density is the one shape that hides it, which is
    // exactly the shape of the reported file's only sub-tool.
    //
    // **A per-dab alpha is not a stroke opacity and must not be assigned as
    // one.** Clip Studio composites every dab; `Brush::opacity` is applied once
    // at commit over coverage the dab pass has already saturated. So it goes
    // through `tip::dab_stack_alpha`, the third of `stroke_coverage`'s family,
    // which asks what stroke opacity reproduces the mark a compositing engine
    // makes at this spacing and hardness — the conversion `mypaint.rs` already
    // does and that `CLAUDE.md` names as this reader's outstanding half.
    // Reading 0.2 as an opacity of 0.2 would be the `4H_pencil` mistake with
    // the sign flipped, and it is why this sits *after* the spacing block: the
    // conversion is a function of the step, so a fixed interval has to have
    // been read first.
    //
    // Full density is the exact identity — `dab_stack_alpha` returns 1.0 at or
    // above 1.0 — so an absent column and the ten of thirteen sub-tools set to
    // 100 all arrive byte for byte as they did before.
    if let Some(flow) = settings.percent("BrushFlow") {
        brush.opacity = (brush.opacity * tip::dab_stack_alpha(flow, brush.spacing, brush.hardness))
            .clamp(0.0, 1.0);
    }

    // ---- pressure ------------------------------------------------------
    let size_effector = settings.effector("BrushSizeEffector");
    brush.pressure_size = size_effector.as_ref().is_some_and(|e| e.drives(PRESSURE));
    if let Some(effector) = size_effector.as_ref().filter(|e| e.drives(PRESSURE)) {
        brush.min_size_ratio = effector.minimum(PRESSURE);
        brush.size_curve = effector.curve_for(PRESSURE);
    }
    // The same effector's random input is a per-dab size draw, which is
    // Umber's radius jitter. Stated as a floor rather than a spread, so a
    // minimum of forty per cent is a dab that may be anywhere from that to
    // full — half a factor of two and a half, in the log space the jitter is
    // measured in.
    if let Some(effector) = size_effector.as_ref().filter(|e| e.drives(RANDOM)) {
        brush.radius_jitter = spread_from_floor(effector.minimum(RANDOM));
    }

    // Hardness follows pressure in Clip Studio exactly as size does, and Umber
    // has the whole shape of it — the flag, the floor and the curve. Reading it
    // is what makes a soft pencil feathery at a light touch instead of drawing
    // one edge for every pressure the hand can produce.
    //
    // The floor is taken **as the file states it**, including zero, where
    // `Brush::min_hardness_ratio`'s own default of 0.5 exists to keep the soft
    // end "a brush rather than a cloud". That default is Umber's taste for a
    // hand-written preset; this is somebody's brush, and a Clip Studio dynamic
    // whose minimum is zero really does go fully diffuse at a feather touch.
    // Same rule `min_size_ratio` is already read by, one line up.
    if let Some(effector) = settings
        .effector("BrushHardnessEffector")
        .filter(|e| e.drives(PRESSURE))
    {
        brush.pressure_hardness = true;
        brush.min_hardness_ratio = effector.minimum(PRESSURE);
        brush.hardness_curve = effector.curve_for(PRESSURE);
    }

    // ---- per-dab coverage ----------------------------------------------
    // Clip Studio reaches per-dab coverage through two settings that
    // **multiply**: Opacity, which is the whole stroke's, and Brush density,
    // which is one dab's. `SPEED_TARGETS` already composes them that way for
    // velocity; pressure has to be composed the same, or the two halves of one
    // brush disagree about what it does. `BrushFlowEffector`'s pressure mapping
    // used to be read for velocity alone and dropped here, so a brush whose
    // density followed the pen arrived painting at full density throughout.
    //
    // Each carries a **floor**, exactly as size does. Clip Studio states it as
    // a percentage of the setting's own value, so a floor of 60 is a brush that
    // never paints below 60% of its opacity however lightly it is touched.
    // Umber has no `min_opacity_ratio` field and does not need one: a floor is
    // exactly representable in the curve as `f + (1 - f) * curve(p)`, which is
    // the arithmetic `radius_at` does around `min_size_ratio`. Reading the
    // curve alone dropped it — and that is a brush whose every stroke comes out
    // at a fraction of the strength its author set, and has to be laid down
    // several times to reach the colour that was asked for.
    let mut coverage: Option<ResponseCurve> = None;
    for column in ["BrushOpacityEffector", "BrushFlowEffector"] {
        let Some(effector) = settings.effector(column).filter(|e| e.drives(PRESSURE)) else {
            continue;
        };
        let one = floored(effector.curve_for(PRESSURE), effector.minimum(PRESSURE));
        coverage = Some(match coverage {
            None => one,
            Some(first) => multiplied(first, one),
        });
    }
    brush.pressure_opacity = coverage.is_some();
    if let Some(curve) = coverage {
        brush.opacity_curve = curve;
    }

    // ---- stroke speed --------------------------------------------------
    // Clip Studio's velocity input is Umber's `Speed`, and the shape is the
    // same in both: the setting is full at a standstill and falls towards the
    // input's floor as the pen moves — which is why `low` is the floor and
    // `high` is the untouched value rather than the other way round.
    //
    // Only these three settings have somewhere to land. Velocity on anything
    // else is named through `dropped::SPEED_ELSEWHERE`, and `SPEED_TARGETS`
    // is what keeps the two halves from disagreeing.
    for (column, target) in SPEED_TARGETS {
        let Some(effector) = settings.effector(column).filter(|e| e.drives(VELOCITY)) else {
            continue;
        };
        let floor = effector.minimum(VELOCITY);
        let (low, high) = match target {
            // Size is a log offset, so a factor composes by addition here and
            // by multiplication in pixels.
            DabTarget::Size => (floor.max(0.001).ln().max(MIN_SIZE_LOG), 0.0),
            // Opacity is a factor, because that is how the table composes it.
            _ => (floor, 1.0),
        };
        brush.modulations.push(Modulation {
            target,
            input: DabInput::Speed,
            low,
            high,
            curve: effector.curve_for(VELOCITY),
        });
    }

    // ---- the dab's angle -----------------------------------------------
    // Rotation has its own encoding — a plain integer rather than the record
    // every other setting uses, with the random amount in a column beside it —
    // and its own list of sources, which is the `DIR_*` constants' whole point.
    //
    // "Direction of line" is `dab_angle_follows_stroke` exactly: the tip turns
    // with the mark, and `dab_angle` above becomes the lean on top of it, which
    // is what Clip Studio's own angle means once a direction source is on.
    // Assigned rather than only set: Umber's own default is a fixed angle
    // *today*, so the two agree, and writing it keeps them agreeing the day
    // that default moves — which is the trap `pressure_size` fell into, where
    // Umber's default really is on and a Clip Studio brush without it has to
    // say so.
    //
    // This is the difference between a rake and a nib and it is not cosmetic.
    // Every sketch pencil in the sample files asks for it, and every one of
    // them used to arrive holding one angle down the whole stroke.
    let rotation_inputs = settings.int("BrushRotationEffector").unwrap_or(0) as u32;
    brush.dab_angle_follows_stroke = rotation_inputs & DIR_LINE != 0;
    if rotation_inputs & DIR_RANDOM != 0 {
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
    // Gated on the reference actually **naming a material**, not on the column
    // holding a blob at all. Clip Studio leaves a setting's value in the file
    // when the setting itself is switched off — which is why the taper reads
    // `BrushUseIn` rather than `BrushInLength`, why the angle jitter reads its
    // effector rather than `BrushRotationRandomScale`, and why the spacing
    // reads `BrushAutoIntervalType`. A texture reference is the same trap and
    // the failure is worse than any of those, because grain **multiplies
    // coverage**: a brush that was never textured arrives painting through a
    // paper it does not have — mottled, weaker than its opacity says, and
    // darker each time the stroke is laid down again, since the pits are
    // anchored to the document and a second pass composites over the first.
    //
    // Deliberately not gated on the material being *present*: Clip Studio
    // leaves an installed one out of the file and expects to find it locally,
    // exactly as it does for a tip, and the strength and the tile size are
    // still the author's numbers. What is checked is that the reference names
    // one at all.
    //
    // The two failures are told apart rather than both reading as "no paper",
    // because they are not the same thing. A reference holding no materials is
    // a texture that was never set, and there is nothing to report. One that
    // holds a material this reader could not resolve is a paper the brush
    // genuinely has, and going quiet about it would be a brush that paints
    // smoother than its author's with nothing saying why — which is the answer
    // `UNUSABLE_TIP` already gives for the analogous tip.
    let texture = settings
        .blob("TextureImage")
        .filter(|r| reference_count(r) > 0);
    if texture.is_some_and(|r| reference_path(r).is_none()) {
        push_once(&mut dropped, dropped::UNREADABLE_TEXTURE);
    }
    let mut paper = None;
    if let Some(reference) = texture.filter(|r| reference_path(r).is_some()) {
        brush.grain = settings
            .percent("TextureDensity")
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let scale = settings.real("TextureScale2").unwrap_or(100.0) as f32 / 100.0;
        // The tile the percentage is *of*. The material's own size where it was
        // read, and `GRAIN_TILE_AT_FULL_SCALE` where it was not — see that
        // constant for what the invented figure used to cost.
        let mut full = GRAIN_TILE_AT_FULL_SCALE;
        // The paper's own picture, where the material is in the file. It goes
        // into the user's texture library and the preset names it — see
        // `BrushPreset::paper`.
        //
        // This used to be `GrainPattern::Tooth` at whatever strength the file
        // asked for, on the reasoning that Umber's papers were a closed set. It
        // still is one, and substituting inside it was the wrong answer: grain
        // **multiplies coverage**, and Tooth's mean is 0.775, so every textured
        // brush arrived painting at about 78% of the opacity its author set —
        // through pits in places that author never drew. A paper Umber cannot
        // resolve now paints flat rather than through a stranger's tile, which
        // is the honest half of the same rule.
        //
        // Gated on the grain actually biting, which is the same threshold the
        // renderer binds a tile at: Clip Studio leaves a texture's strength in
        // the file at zero when the setting is switched off, and a picture
        // stored for a brush that cannot feel it is a file per sub-tool that
        // nothing ever samples. A `.sutg` is fifteen of them.
        if brush.has_grain() {
            match paper_for(reference, materials) {
                Some(ResolvedPaper { tile, native, lost }) => {
                    // Named only where something was actually given up — the
                    // thumbnail fallback, or a reduction. Naming the thumbnail
                    // on every textured brush was true when it was the only
                    // route and is a false apology now that it is the second
                    // one, which is exactly the rule `THUMBNAIL_TIP` follows.
                    if let Some(loss) = lost {
                        push_once(&mut dropped, loss);
                    }
                    if let Some(side) = native {
                        full = side as f32;
                    }
                    paper = Some(tile);
                }
                // **And the strength goes with it**, which is the half that
                // was missing and made three doc comments false. `paper` being
                // `None` leaves `BrushPreset::paper` unset, and
                // `Editor::paper_tile`'s `None` arm falls back to
                // `brush.grain_pattern` — which this converter never writes, so
                // it is `Brush::default()`'s `Tooth`. A brush whose material
                // Clip Studio left out of the file therefore arrived painting
                // through Tooth at whatever strength the file asked for: the
                // exact 78%-of-its-own-opacity substitution the rest of this
                // block exists to have stopped doing. Zero is the identity the
                // dab pass documents, so this is "paints flat" actually meant.
                None => {
                    brush.grain = 0.0;
                    push_once(&mut dropped, dropped::PAPER_TEXTURE);
                }
            }
        }
        // **A paper decides build-up exactly as a tip does**, and leaving it to
        // the tip alone is what made a textured brush arrive nearly
        // transparent. Clip Studio composites every dab — the sentence the tip
        // block below already rests on — and the grain is anchored to the
        // document, so every dab reaching a pixel is scaled by the *same*
        // texel: under compositing those faint texels build towards solid, and
        // under Umber's `max` they cap the whole stroke at the tile's own value
        // for as long as the stroke lasts.
        //
        // The reported brush is the measurement: a 500×500 grunge scatter at
        // `TextureDensity` 100, mean 0.272, painting at 27% of the opacity its
        // author set where Clip Studio's stroke reaches 77%. The tip's rule
        // could not have caught it and this is not a threshold that was too
        // slack — two of the four sub-tools carry no tip at all, so it never
        // ran, and the tile's brightest texel is 255, so the peak it takes
        // agrees with itself. See `tip::grain_coverage` for why the paper's
        // statistic is the mean.
        //
        // `|=` in both places rather than an assignment in each: either the tip
        // or the paper is reason enough, and whichever is read second must not
        // be able to take the other's answer off.
        if let Some(tile) = &paper {
            brush.build_up |=
                tip::grain_coverage(tile, brush.grain, brush.spacing).needs_build_up();
        }
        // Umber's own bound on a tile, and it is newly reachable: `full` used
        // to be 256, so the ceiling needed a `TextureScale2` of 800%. Now that
        // it is the material's own longest side the two overlap **exactly** —
        // `REDUCED_PAPER` fires above `TipMask::MAX_SIZE` and at the default
        // 100% that is the same condition as this clamp — so a paper reduced to
        // fit would also have been silently re-spaced, repeating twice as often
        // as its author drew it under a notice that spoke only of softening.
        // Two different losses may not hide under one name.
        let wanted = full * scale;
        brush.grain_scale = wanted.clamp(Brush::MIN_GRAIN_SCALE, Brush::MAX_GRAIN_SCALE);
        // Only where the brush can feel the grain at all: with the strength
        // zeroed above there is no spacing left to have lost.
        if brush.has_grain() && (brush.grain_scale - wanted).abs() > 0.5 {
            push_once(&mut dropped, dropped::PAPER_SPACING);
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
            Some(ResolvedTip { mask, lost }) => {
                if let Some(loss) = lost {
                    push_once(&mut dropped, loss);
                }
                // A stamp is the overlap of many faint impressions, and Clip
                // Studio composites every dab as GIMP and Krita do. Measured
                // rather than assumed, by the same function that decides it for
                // the shipped library.
                let measured = stroke_coverage(&mask, brush.spacing);
                if measured.is_usable() {
                    brush.build_up |= measured.needs_build_up();
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

    // ---- the taper -----------------------------------------------------
    // "In" is a size ramp over the first stretch of the mark, which is exactly
    // what the `Stroke` input is: it counts travel from the start of the stroke
    // and, held at its ceiling, never wraps. `BrushInLength` is in the same
    // unit as the brush size and the ramp is measured in dab radii, so a brush
    // scaled up tapers over a proportionally longer mark — which is what Clip
    // Studio does too, since it scales the taper with the tool.
    //
    // "Out" cannot follow: it is measured back from an end the engine does not
    // know until the stroke is over, and there is nothing to look ahead with.
    if settings.flag("BrushUseIn")
        && let Some(length) = settings.real("BrushInLength").filter(|v| *v > 0.0)
    {
        let radius = (brush.size * 0.5).max(0.5);
        brush.stroke_span = (length as f32 / radius).clamp(0.1, 1000.0);
        brush.stroke_hold = NEVER_WRAP;
        brush.modulations.push(Modulation {
            target: DabTarget::Size,
            input: DabInput::Stroke,
            low: MIN_SIZE_LOG,
            high: 0.0,
            curve: ResponseCurve::LINEAR,
        });
    }
    if settings.flag("BrushUseOut") {
        push_once(&mut dropped, dropped::TAPER_OUT);
    }

    // ---- what is left over ---------------------------------------------
    if settings.flag("BrushContinuousPlot") {
        brush.dabs_per_second = HELD_SPRAY_RATE;
    }
    // A round dab with no tip is the same picture at every angle, so whether an
    // angle *shows* is the two things the brush knows and the settings row does
    // not. `dab_has_angle` answers the elliptical half and the tip is the
    // other, exactly as `Editor::tip` combines them for the brush editor.
    let angle_shows = brush.dab_has_angle() || tip.is_some();
    for loss in unreachable_inputs(
        settings,
        &SPEED_TARGETS.map(|(column, _)| column),
        angle_shows,
    ) {
        push_once(&mut dropped, loss);
    }
    if settings.flag("UseDualBrush") {
        push_once(&mut dropped, dropped::DUAL_BRUSH);
    }
    if settings.flag("BrushUseWaterEdge") {
        push_once(&mut dropped, dropped::WATER_EDGE);
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

    Converted {
        brush,
        tip,
        paper,
        dropped,
    }
}

/// A response with a floor under it: the curve rescaled into `floor..=1`.
///
/// Clip Studio states a dynamic's minimum as a percentage of the setting's own
/// value, which is precisely what [`Brush::min_size_ratio`] means for size —
/// and `radius_at` composes it as `min + (1 - min) * curve(p)`. Opacity has no
/// such field, deliberately (`CLAUDE.md`: coverage genuinely reaches zero), so
/// the floor is folded into the curve instead, where it is exact. A floor of
/// zero is the identity and a floor of one is a flat response, which is what a
/// setting pressure cannot move actually does.
fn floored(curve: ResponseCurve, floor: f32) -> ResponseCurve {
    if floor <= 0.0 {
        return curve;
    }
    let mut out = curve;
    for i in 0..ResponseCurve::N {
        out.set(i, floor + (1.0 - floor) * curve.points[i]);
    }
    out
}

/// Two responses multiplied sample by sample, which is what lets one Umber
/// curve carry two Clip Studio settings that multiply.
///
/// **Exact at the five knots and an approximation between them**, and the
/// difference is worth stating because [`floored`] beside it genuinely is
/// exact everywhere. The product of two piecewise-linear curves is
/// *quadratic*; this re-linearises it over each quarter of the range, which
/// bows by at most `Δa·Δb/4` — four levels of eight-bit coverage in the worst
/// case, two `LINEAR` curves. That is the resolution [`ResponseCurve`] has at
/// all, and the same bound [`Effector::curve_for`] already accepts when it
/// resamples Clip Studio's own control points onto those five knots.
fn multiplied(a: ResponseCurve, b: ResponseCurve) -> ResponseCurve {
    let mut out = a;
    for i in 0..ResponseCurve::N {
        out.set(i, a.points[i] * b.points[i]);
    }
    out
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

    /// The columns of `Variant` this importer reads, plus the five below that
    /// it deliberately does not, in an order that
    /// is deliberately *not* the order the code reads them in — the point of
    /// the schema being name-addressed is that neither one matters.
    const VARIANT_COLUMNS: [&str; 47] = [
        "TextureDensityEffector",
        "VariantID",
        // The per-dab density. Its absence here is why nothing caught it being
        // unread: a fixture that cannot hold a value cannot ask what happens to
        // it, and `BrushFlowEffector` sitting below reads as coverage of the
        // same setting while covering only its pressure mapping.
        "BrushFlow",
        // Five columns this importer never reads, and that is why they are
        // here: the row has to be able to carry the dual brush's leftovers so
        // that `a_dual_brush_that_is_switched_off_is_not_reported_from_the_
        // values_left_beside_it` can put them in it. A fixture that could not
        // hold them would make that test vacuous.
        "DualBrushCompositeMode",
        "DualSize",
        "DualTextureDensity",
        "ChangeRGBByDual",
        "DualUsePatternImage",
        "BrushRotation",
        "Opacity",
        "BrushThickness",
        "BrushSize",
        "BrushSizeEffector",
        "BrushHardness",
        "BrushHardnessEffector",
        "BrushInterval",
        "BrushAutoIntervalType",
        "BrushFlowEffector",
        "BrushInLength",
        "BrushOutLength",
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

    /// A `*Effector` blob: which inputs are on, their floors, and up to two
    /// curve records — the pressure one and the one tilt and velocity share.
    ///
    /// Either may be absent, and the header's two lengths are what says so.
    /// A brush edited into using tilt and then out of it again leaves the
    /// second record behind with nothing enabled, which the fixture can build
    /// by passing a curve for an input that is off.
    fn effector(
        enabled: u32,
        minimums: [i32; 4],
        pressure_curve: &[(f64, f64)],
        other_curve: &[(f64, f64)],
    ) -> Value {
        let record = |points: &[(f64, f64)]| -> Vec<u8> {
            if points.is_empty() {
                return Vec::new();
            }
            let mut out = Vec::new();
            out.extend_from_slice(&12u32.to_be_bytes());
            out.extend_from_slice(&(points.len() as u32).to_be_bytes());
            out.extend_from_slice(&16u32.to_be_bytes());
            for (x, y) in points {
                out.extend_from_slice(&x.to_be_bytes());
                out.extend_from_slice(&y.to_be_bytes());
            }
            out
        };
        let first = record(pressure_curve);
        let second = record(other_curve);

        let mut out = Vec::new();
        out.extend_from_slice(&44u32.to_be_bytes());
        out.extend_from_slice(&0x1f0u32.to_be_bytes());
        out.extend_from_slice(&enabled.to_be_bytes());
        for m in minimums {
            out.extend_from_slice(&m.to_be_bytes());
        }
        out.extend_from_slice(&0i32.to_be_bytes());
        out.extend_from_slice(&(first.len() as u32).to_be_bytes());
        out.extend_from_slice(&(second.len() as u32).to_be_bytes());
        // Tilt's ceiling, at the default that means "no gain".
        out.extend_from_slice(&100i32.to_be_bytes());
        out.extend_from_slice(&first);
        out.extend_from_slice(&second);
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

    /// A material archive whose thumbnail is `w` by `h` of one RGBA pixel, and
    /// which carries no full-resolution layer at all.
    ///
    /// That is a real case — Clip Studio leaves an installed material out of
    /// the file — and it is the one that exercises the thumbnail fallback.
    fn material(w: u32, h: u32, pixel: [u8; 4]) -> Vec<u8> {
        material_with_pixels(w, h, pixel, None)
    }

    /// The same, plus the material's own pixels as a `data/material_0.layer`.
    fn material_with_pixels(
        w: u32,
        h: u32,
        pixel: [u8; 4],
        layer: Option<(u32, u32, Vec<u8>)>,
    ) -> Vec<u8> {
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
        let mut members = vec![
            ("catalog.zip", vec![0x89, b'C', b'2', b'F']),
            ("thumbnail/thumbnail.png", png_bytes),
            ("icedata/layerData.xml", b"<infolist/>".to_vec()),
        ];
        if let Some((width, height, coverage)) = layer {
            members.insert(
                1,
                (
                    "data/material_0.layer",
                    csmaterial::fixture::material_layer(width, height, &coverage, 1),
                ),
            );
        }
        tar(&members)
    }

    /// A material whose thumbnail is an 8-bit **greyscale** PNG with no alpha
    /// channel — which is what a real paper material's preview is as likely to
    /// be as RGBA, and the case `thumbnail_rgba` leans on
    /// `Transformations::ALPHA` to expand.
    fn grey_material(w: u32, h: u32, value: u8) -> Vec<u8> {
        let mut png_bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png_bytes, w, h);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("fixture png header");
            writer
                .write_image_data(&vec![value; (w * h) as usize])
                .expect("fixture png data");
        }
        tar(&[
            ("catalog.zip", vec![0x89, b'C', b'2', b'F']),
            ("thumbnail/thumbnail.png", png_bytes),
            ("icedata/layerData.xml", b"<infolist/>".to_vec()),
        ])
    }

    // ----------------------------------------------------------------- tests

    /// `thumbnail_rgba` matches only the two colour types that carry an alpha
    /// channel and relies on `Transformations::ALPHA` to expand the other
    /// three. Nothing exercised that: the RGBA fixture takes the first arm
    /// whatever the transformation does. If the expansion ever stopped
    /// happening, every paper import would quietly degrade to "paints flat"
    /// with a loss notice, which reads as the feature not working.
    #[test]
    fn a_greyscale_paper_material_decodes() {
        let path = ".:paper:data:material_0.layer";
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 60),
            )],
            &[(path, grey_material(4, 4, 200))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        let paper = tool.paper.as_ref().expect("a greyscale thumbnail decodes");
        assert_eq!(paper.at(1, 1), 200, "brightness straight through");
    }

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
    ///
    /// Skipping it is **not** reported: a `.sutg` is a tool group and a group
    /// holding a fill tool is the ordinary case, so a note about it would
    /// appear on nearly every import and teach the reader to skip the list.
    #[test]
    fn a_sub_tool_that_is_not_a_brush_is_skipped_quietly() {
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
        assert!(file.dropped.is_empty(), "{:?}", file.dropped);
        assert!(dropped_features(&bytes).is_empty());
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
                        [20, 0, 0, 0],
                        &[(0.0, 0.0), (0.5, 0.0), (1.0, 1.0)],
                        &[],
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
                Variant::plain(1).set("BrushSizeEffector", effector(0, [0; 4], &[], &[])),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!(!brush.pressure_size);
        assert_eq!(brush.radius_at(0.0), brush.radius_at(1.0));
    }

    /// Umber has no tilt input on any platform it runs on, so a tilt mapping
    /// is named rather than wired to whichever of Umber's inputs looks closest.
    /// Storing it against speed would be a brush that thins when you *move*
    /// where the author meant it to thin when you *lift* — wrong in a way that
    /// looks deliberate.
    #[test]
    fn a_tilt_mapping_is_named_rather_than_wired_to_speed() {
        let bytes = sut(
            &[(
                "Tilt pen",
                Variant::plain(1).set("BrushSizeEffector", effector(TILT, [0, 40, 0, 0], &[], &[])),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::TILT_INPUT]);
        // And it must not have quietly become pressure or speed on the way past.
        assert!(!tool.brush.pressure_size);
        assert!(tool.brush.modulations.is_empty());
    }

    /// The fifth effect source. Nothing in either sample file switches it on,
    /// so it is named rather than guessed at — the same rule tilt is held to.
    #[test]
    fn an_effect_source_this_reader_cannot_name_says_so() {
        let bytes = sut(
            &[(
                "Odd",
                Variant::plain(1).set("BrushSizeEffector", effector(UNKNOWN, [0; 4], &[], &[])),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::UNKNOWN_INPUT]);
        assert!(tool.brush.modulations.is_empty());
    }

    /// Clip Studio's velocity input is Umber's speed, and the whole point is
    /// that a brush set to thin as the hand moves arrives thinning as the hand
    /// moves. The floor is the size at full speed and the curve is the shape
    /// between; `Size` is a *log* offset, so a floor of 25% is `ln(0.25)`.
    #[test]
    fn stroke_speed_drives_size_through_its_floor_and_its_curve() {
        let bytes = sut(
            &[(
                "Velocity pen",
                Variant::plain(1).set(
                    "BrushSizeEffector",
                    effector(
                        PRESSURE | VELOCITY,
                        [10, 0, 25, 0],
                        &[(0.0, 0.0), (1.0, 1.0)],
                        // Full at a standstill, gone by two thirds of the way
                        // up the range: Clip Studio's own default shape.
                        &[(0.0, 1.0), (0.66, 0.0), (1.0, 0.0)],
                    ),
                ),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        // Speed is rendered, so there is nothing to apologise for.
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);

        let m = tool
            .brush
            .modulations
            .as_slice()
            .iter()
            .find(|m| m.input == DabInput::Speed)
            .expect("a speed modulation");
        assert_eq!(m.target, DabTarget::Size);
        assert!((m.high - 0.0).abs() < 1e-6, "{}", m.high);
        assert!((m.low - 0.25f32.ln()).abs() < 1e-5, "{}", m.low);
        // Standing still leaves the dab alone; moving fast shrinks it to the
        // floor. `at` takes the input already normalised onto 0..1.
        assert!((m.at(0.0).exp() - 1.0).abs() < 1e-5);
        assert!((m.at(1.0).exp() - 0.25).abs() < 1e-4);
        // Pressure is untouched by any of it, and reads its own curve.
        assert!(tool.brush.pressure_size);
        assert!((tool.brush.min_size_ratio - 0.1).abs() < 1e-6);
        assert_eq!(tool.brush.size_curve, ResponseCurve::LINEAR);
    }

    /// The two curve records are the pressure one and the one tilt and velocity
    /// share, in that order — and either may be missing. Reading the first
    /// record whatever it is would give a velocity brush the pressure curve, or
    /// a pressure brush somebody else's.
    #[test]
    fn the_second_curve_record_belongs_to_speed_and_the_first_to_pressure() {
        let bytes = sut(
            &[(
                "Velocity only",
                Variant::plain(1).set(
                    "BrushSizeEffector",
                    // No pressure record at all: the only curve in the blob is
                    // the second one, and it is speed's.
                    effector(
                        VELOCITY,
                        [0, 0, 0, 0],
                        &[],
                        &[(0.0, 1.0), (0.5, 1.0), (1.0, 0.0)],
                    ),
                ),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        let m = brush.modulations.as_slice()[0];
        // Flat across the first half, then straight down.
        assert!((m.curve.sample(0.0) - 1.0).abs() < 1e-5);
        assert!((m.curve.sample(0.5) - 1.0).abs() < 1e-5);
        assert!((m.curve.sample(1.0) - 0.0).abs() < 1e-5);
        assert!(!brush.pressure_size);
    }

    /// One record cannot say whether it belongs to tilt or to velocity, so a
    /// brush driven by both gets a straight line rather than a shape that might
    /// be the other input's. The range still carries how far the size travels.
    #[test]
    fn a_setting_driven_by_both_tilt_and_speed_keeps_the_range_and_drops_the_shape() {
        let bytes = sut(
            &[(
                "Both",
                Variant::plain(1).set(
                    "BrushSizeEffector",
                    effector(
                        TILT | VELOCITY,
                        [0, 100, 30, 0],
                        &[],
                        &[(0.0, 1.0), (0.2, 0.0), (1.0, 0.0)],
                    ),
                ),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        let m = tool.brush.modulations.as_slice()[0];
        assert_eq!(m.curve, ResponseCurve::LINEAR);
        assert!((m.low - 0.3f32.ln()).abs() < 1e-5);
        // The tilt half is still a loss and still says so.
        assert_eq!(tool.dropped, [dropped::TILT_INPUT]);
    }

    /// A dynamic's **floor** is half of what it says, and opacity's used to be
    /// thrown away.
    ///
    /// Clip Studio states the minimum as a percentage of the setting's own
    /// value, so a floor of 60 is a brush that never paints below six tenths of
    /// its opacity however lightly it is touched. Reading the curve alone
    /// imported that as a brush painting from nothing — every stroke a fraction
    /// of the strength its author set, and only reaching the colour asked for
    /// once it had been laid down several times. Which is exactly what an
    /// opacity control that does not work looks like.
    #[test]
    fn a_pressure_opacity_mapping_keeps_the_floor_under_it() {
        let bytes = sut(
            &[(
                "Ink",
                Variant::plain(1).set(
                    "BrushOpacityEffector",
                    effector(PRESSURE, [60, 0, 0, 0], &[(0.0, 0.0), (1.0, 1.0)], &[]),
                ),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;

        assert!(brush.pressure_opacity);
        // A feather touch is six tenths, not nothing; a full press is full.
        assert!(
            (brush.coverage_at(0.0) - 0.6).abs() < 1e-5,
            "{}",
            brush.coverage_at(0.0)
        );
        assert!((brush.coverage_at(1.0) - 1.0).abs() < 1e-5);
        // And the shape between the two is still the file's, rescaled rather
        // than replaced: a linear curve stays linear over the new range.
        assert!((brush.coverage_at(0.5) - 0.8).abs() < 1e-5);
    }

    /// Clip Studio reaches per-dab coverage through Opacity *and* Brush
    /// density, and they multiply — which `SPEED_TARGETS` already said for
    /// velocity while pressure ignored the density half entirely. A brush whose
    /// density followed the pen therefore arrived painting at full density
    /// throughout, which is the same bug pointing the other way.
    #[test]
    fn opacity_and_dab_density_both_follow_pressure_and_multiply() {
        let ramp = [(0.0, 0.0), (1.0, 1.0)];
        let bytes = sut(
            &[(
                "Wash",
                Variant::plain(1)
                    .set(
                        "BrushOpacityEffector",
                        effector(PRESSURE, [50, 0, 0, 0], &ramp, &[]),
                    )
                    .set(
                        "BrushFlowEffector",
                        effector(PRESSURE, [50, 0, 0, 0], &ramp, &[]),
                    ),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        // Both mappings landed, so there is nothing to apologise for.
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);

        let brush = tool.brush;
        assert!(brush.pressure_opacity);
        // Half times half at no pressure, one times one at full.
        assert!((brush.coverage_at(0.0) - 0.25).abs() < 1e-5);
        assert!((brush.coverage_at(1.0) - 1.0).abs() < 1e-5);
        // And density alone still switches the flag on, which it did not
        // before: only the opacity effector was consulted.
        let bytes = sut(
            &[(
                "Density only",
                Variant::plain(2).set(
                    "BrushFlowEffector",
                    effector(PRESSURE, [0, 0, 0, 0], &ramp, &[]),
                ),
            )],
            &[],
        );
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!(brush.pressure_opacity);
        assert_eq!(brush.coverage_at(0.0), 0.0);
    }

    /// **A constant per-dab density is read, and it is converted rather than
    /// assigned.**
    ///
    /// `BrushFlow` is Clip Studio's Density and it was read nowhere at all,
    /// while two comments in this file said it multiplies with `Opacity` to
    /// reach per-dab coverage — the code multiplied only their *pressure
    /// curves*. Three of `Used.sutg`'s thirteen sub-tools set it below full, so
    /// "Soft 2" at 20 imported painting at five times its author's density: a
    /// mark that saturates in one pass and therefore cannot be built up by
    /// going over it again, which is the paper bug's mirror image.
    ///
    /// The conversion is the point. Clip Studio composites every dab where
    /// `Brush::opacity` is applied once over saturated coverage, so a per-dab
    /// alpha assigned straight across is the `4H_pencil` mistake with its sign
    /// flipped — a fifth of the mark rather than five times it.
    /// [`tip::dab_stack_alpha`] is what a compositing engine actually reaches,
    /// and this pins that the imported figure sits between the two readings
    /// rather than being either of them.
    #[test]
    fn a_constant_per_dab_density_becomes_the_stroke_opacity_it_builds_to() {
        let bytes = sut(&[("Soft", Variant::plain(1).int("BrushFlow", 20))], &[]);
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;

        // The engine's own answer for "what does a fifth per dab build to", at
        // the spacing and hardness this brush actually carries. Recomputed
        // rather than written as a literal because the figure is a function of
        // both, and a literal would pin the fixture instead of the rule.
        let wanted = tip::dab_stack_alpha(0.2, brush.spacing, brush.hardness);
        assert!(
            (brush.opacity - wanted).abs() < 1e-6,
            "opacity {} is not the built-up reading {wanted}",
            brush.opacity
        );
        // Both wrong readings are refused: neither the raw per-dab figure nor
        // the full density it used to arrive at.
        assert!(brush.opacity > 0.2, "a per-dab alpha read as a stroke one");
        assert!(brush.opacity < 1.0, "the density was discarded again");

        // `Opacity` is the stroke's and multiplies in, unconverted: half the
        // stroke of the same density is half the mark.
        let bytes = sut(
            &[(
                "Half",
                Variant::plain(2).int("BrushFlow", 20).int("Opacity", 50),
            )],
            &[],
        );
        let halved = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!((halved.opacity - brush.opacity * 0.5).abs() < 1e-6);

        // And full density is the exact identity, which is what keeps the ten
        // of thirteen sub-tools set to 100 — and every fixture without the
        // column — arriving exactly as they did before.
        for full in [Variant::plain(3).int("BrushFlow", 100), Variant::plain(4)] {
            let bytes = sut(&[("Full", full)], &[]);
            let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
            assert_eq!(brush.opacity, 1.0);
        }
    }

    /// Hardness follows pressure in Clip Studio exactly as size does, and Umber
    /// has the whole shape of it. Leaving it unread gave a soft pencil one edge
    /// for every pressure the hand can make.
    #[test]
    fn pressure_drives_hardness_through_its_floor_and_its_curve() {
        assert!(
            !Brush::default().pressure_hardness,
            "the default has moved, and an unread column would now inherit it"
        );
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1).int("BrushHardness", 100).set(
                    "BrushHardnessEffector",
                    effector(PRESSURE, [30, 0, 0, 0], &[(0.0, 0.0), (1.0, 1.0)], &[]),
                ),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);

        let brush = tool.brush;
        assert!(brush.pressure_hardness);
        assert!((brush.min_hardness_ratio - 0.3).abs() < 1e-6);
        assert!((brush.hardness_at(0.0) - 0.3).abs() < 1e-5);
        assert!((brush.hardness_at(1.0) - 1.0).abs() < 1e-5);

        // A brush that does not map it must not pick up Umber's own answer.
        let bytes = sut(&[("Nib", Variant::plain(1))], &[]);
        let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
        assert!(!brush.pressure_hardness);
    }

    /// Pressure and the random draw driving a setting Umber has no field for
    /// are as lost as a tilt mapping, and are deliberately **not** reported —
    /// see [`unreachable_inputs`] for the argument. This is what pins that the
    /// silence is a decision rather than an oversight: a list that apologises
    /// on nearly every import is one a reader learns to skip, taking the losses
    /// that matter with it.
    #[test]
    fn pressure_and_randomness_with_nowhere_to_land_stay_quiet() {
        let bytes = sut(
            &[
                (
                    "Textured",
                    Variant::plain(1).set(
                        "TextureDensityEffector",
                        effector(PRESSURE | RANDOM, [0, 0, 0, 40], &[], &[]),
                    ),
                ),
                // Tilt on the same column still speaks, so the sweep is
                // demonstrably still running over it.
                (
                    "Tilted",
                    Variant::plain(2).set(
                        "TextureDensityEffector",
                        effector(PRESSURE | TILT, [0; 4], &[], &[]),
                    ),
                ),
            ],
            &[],
        );
        let tools = from_sut(&bytes).expect("read").tools;
        assert!(tools[0].dropped.is_empty(), "{:?}", tools[0].dropped);
        assert_eq!(tools[1].dropped, [dropped::TILT_INPUT]);
    }

    /// Clip Studio leaves a setting's value in the file when the setting is
    /// switched off — the trap the taper, the angle jitter and the spacing are
    /// each guarded against by name. The texture reference is the same trap and
    /// the worst of them: grain **multiplies coverage**, so a brush that was
    /// never textured painted through a paper it does not have — mottled,
    /// weaker than its opacity claimed, and darker every time the stroke was
    /// laid down again.
    #[test]
    fn a_texture_reference_that_names_no_material_leaves_the_brush_ungrained() {
        let textured = |stale: Value| {
            sut(
                &[(
                    "Untextured",
                    Variant::plain(1)
                        .set("TextureImage", stale)
                        .int("TextureDensity", 80)
                        .real("TextureScale2", 100.0),
                )],
                &[],
            )
        };

        // A reference holding no materials at all: a texture that was never
        // set, so there is nothing to grain and nothing to report.
        let bytes = textured(reference(".:paper:data:material_0.layer", 0));
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.brush.grain, 0.0);
        assert!(!tool.brush.has_grain());
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);

        // One that holds a material this reader cannot resolve is a paper the
        // brush genuinely has. Still no invented grain — but it says so, which
        // is the answer the analogous tip already gives.
        let bytes = textured(reference("", 1));
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.brush.grain, 0.0);
        assert_eq!(tool.dropped, [dropped::UNREADABLE_TEXTURE]);
    }

    /// Speed on Clip Studio's per-dab density is Umber's per-dab opacity, and
    /// opacity composes as a *factor* rather than as an offset — so the floor
    /// is the factor at full speed and the untouched value is 1.
    #[test]
    fn stroke_speed_on_flow_becomes_a_per_dab_opacity_factor() {
        let bytes = sut(
            &[(
                "Fading marker",
                Variant::plain(1).set(
                    "BrushFlowEffector",
                    effector(VELOCITY, [0, 0, 40, 0], &[], &[(0.0, 1.0), (1.0, 0.0)]),
                ),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);
        let m = tool.brush.modulations.as_slice()[0];
        assert_eq!((m.target, m.input), (DabTarget::Opacity, DabInput::Speed));
        assert!((m.at(0.0) - 1.0).abs() < 1e-5);
        assert!((m.at(1.0) - 0.4).abs() < 1e-5);
    }

    /// Speed reaches size and per-dab opacity and nothing else, so a brush
    /// whose *texture density* follows velocity has to say the mapping did not
    /// arrive — and a brush whose size does must not say it anyway.
    #[test]
    fn stroke_speed_with_nowhere_to_land_is_named() {
        let bytes = sut(
            &[
                (
                    "Textured",
                    Variant::plain(1).set(
                        "TextureDensityEffector",
                        effector(VELOCITY, [0, 0, 50, 0], &[], &[]),
                    ),
                ),
                (
                    "Sized",
                    Variant::plain(2).set(
                        "BrushSizeEffector",
                        effector(VELOCITY, [0, 0, 50, 0], &[], &[]),
                    ),
                ),
            ],
            &[],
        );
        let tools = from_sut(&bytes).expect("read").tools;
        assert_eq!(tools[0].dropped, [dropped::SPEED_ELSEWHERE]);
        assert!(tools[1].dropped.is_empty(), "{:?}", tools[1].dropped);
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
                    .int("BrushRotationEffector", (DIR_RANDOM | 3) as i64)
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
    /// the effector columns — and a chisel that turns with the pen is exactly
    /// the brush that would then arrive silently wrong. Its two low bits are
    /// its own and must not be read as effect sources.
    #[test]
    fn an_input_driving_the_dab_angle_is_reported_too() {
        let bytes = sut(
            &[(
                "Rake",
                Variant::plain(1)
                    .int("BrushThickness", 40)
                    .int("BrushRotationEffector", (DIR_TILT | 3) as i64),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::TILT_INPUT]);
        assert_eq!(tool.brush.dab_angle_jitter, 0.0);
        assert!(!tool.brush.dab_angle_follows_stroke);

        // "Direction of pen" is the *azimuth* of the tilt rather than its
        // amount — still tilt, and still nothing any platform reports, so it is
        // named. Under the record effectors' vocabulary this same bit would
        // have read as pressure, which is the one reading dropped in silence;
        // that is what this pins it is no longer doing.
        let bytes = sut(
            &[(
                "Azimuth rake",
                Variant::plain(1)
                    .int("BrushThickness", 40)
                    .int("BrushRotationEffector", (DIR_PEN | 3) as i64),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::TILT_INPUT]);

        // The barrel's twist is never set in either sample file, so it keeps
        // the honest name rather than gaining a confident one for a bit nobody
        // has seen. Same rule the record effectors' own fifth bit follows.
        let bytes = sut(
            &[(
                "Twist rake",
                Variant::plain(1)
                    .int("BrushThickness", 40)
                    .int("BrushRotationEffector", (DIR_PEN_AXIS | 3) as i64),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::UNKNOWN_INPUT]);
        assert!(!tool.brush.dab_angle_follows_stroke);
    }

    /// A round dab with no tip is the same picture at every angle, so a
    /// direction source on one is a setting whose absence cannot be seen —
    /// and an apology for it is the cry-wolf failure `unreachable_inputs`
    /// exists to refuse. Two of the thirteen brushes in the sample files are
    /// exactly this, and the first draft of the Direction reading gave both an
    /// apology they had no use for: a loss list is only worth reading while
    /// every line on it is a mark somebody will miss.
    #[test]
    fn a_direction_source_on_a_dab_with_no_visible_angle_is_not_worth_reporting() {
        let round = |thickness: i64| {
            sut(
                &[(
                    "Round",
                    Variant::plain(1)
                        .int("BrushThickness", thickness)
                        .int("BrushRotationEffector", (DIR_TILT | 3) as i64),
                )],
                &[],
            )
        };
        let tool = from_sut(&round(100)).expect("read").tools.remove(0);
        assert!(!tool.brush.dab_has_angle());
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);

        // Flatten the same brush and the angle is suddenly the whole of what
        // the mark looks like, so the same bit is worth a line.
        let tool = from_sut(&round(30)).expect("read").tools.remove(0);
        assert!(tool.brush.dab_has_angle());
        assert_eq!(tool.dropped, [dropped::TILT_INPUT]);
    }

    /// A bitmap tip is not rotationally symmetric whatever its roundness, so
    /// it is the other half of "does the angle show" — the same pair
    /// `Brush::dab_has_angle` and `Editor::tip` are combined in for the brush
    /// editor. A round *stamp* brush losing its direction source is a real
    /// loss, and reading the ratio alone would have gone quiet about it.
    #[test]
    fn a_round_dab_with_a_stamp_still_reports_the_direction_it_lost() {
        let bytes = sut(
            &[(
                "Stamp",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference("m:data:tip", 1))
                    .int("BrushRotationEffector", (DIR_TILT | 3) as i64),
            )],
            &[("m:data:tip", material(8, 8, [0, 0, 0, 255]))],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.tip.is_some());
        assert!(!tool.brush.dab_has_angle(), "the dab itself is round");
        assert!(tool.dropped.contains(&dropped::TILT_INPUT));
    }

    /// The whole of the Clip Studio half of this. `1 << 6` on the *angle* is
    /// **Direction of line**, not velocity — the Direction dynamic's source
    /// list has no velocity in it at all — so a brush whose tip follows the
    /// mark arrives as a rake and has nothing to apologise for. Both halves
    /// matter: it used to import as a fixed nib *and* raise a sentence about a
    /// stroke speed that was never the setting in question, which is the worst
    /// shape this bug takes — a wrong mark under a note pointing elsewhere.
    #[test]
    fn an_angle_following_the_line_is_a_rake_and_not_a_lost_stroke_speed() {
        let bytes = sut(
            &[(
                "Sketch",
                Variant::plain(1)
                    .int("BrushThickness", 58)
                    .real("BrushRotation", 45.0)
                    .int("BrushRotationEffector", (DIR_LINE | 3) as i64),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.brush.dab_angle_follows_stroke);
        // The stated angle is the lean on top of the heading, which is what it
        // means in Clip Studio once a direction source is on.
        assert_eq!(tool.brush.dab_angle, 45.0);
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);
        assert!(!dropped_features(&bytes).contains(&dropped::SPEED_ELSEWHERE));

        // 195 — the two bits together — is what four of the thirteen brushes
        // in the sample files actually hold, and it was the one value the
        // flag and the jitter were only ever tested apart from. A tip that
        // follows the mark *and* wobbles is a sketching pencil, and the two
        // are read off one integer, so nothing but this says they compose.
        let bytes = sut(
            &[(
                "Sketch 2",
                Variant::plain(1)
                    .int("BrushThickness", 58)
                    .int("BrushRotationEffector", (DIR_LINE | DIR_RANDOM | 3) as i64)
                    .int("BrushRotationRandomScale", 45),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.brush.dab_angle_follows_stroke);
        assert!((tool.brush.dab_angle_jitter - 162.0).abs() < 1e-4);
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);
    }

    /// The pair to the rake above rather than a restatement of it: a brush
    /// asking for a direction source Umber cannot follow — or for none at
    /// all — must not fall back to the one source it can, which is the
    /// tempting shape of this fix and would turn every flat marker in
    /// somebody's library into a rake. The bit has to be *this* bit.
    ///
    /// It also pins the assignment against `Brush::default`, asserted here
    /// rather than assumed: the two agree today, so nothing but a test would
    /// notice the day that default moves.
    #[test]
    fn a_brush_that_does_not_ask_to_follow_the_line_holds_its_angle() {
        assert!(
            !Brush::default().dab_angle_follows_stroke,
            "the default has moved"
        );
        for sources in [
            3,
            DIR_RANDOM | 3,
            DIR_TILT | 3,
            DIR_PEN | 3,
            DIR_PEN_AXIS | 3,
        ] {
            let bytes = sut(
                &[(
                    "Nib",
                    Variant::plain(1)
                        .int("BrushThickness", 30)
                        .int("BrushRotationEffector", sources as i64),
                )],
                &[],
            );
            let brush = from_sut(&bytes).expect("read").tools.remove(0).brush;
            assert!(
                !brush.dab_angle_follows_stroke,
                "sources {sources:#x} turned a nib into a rake"
            );
        }
    }

    /// Stroke speed on a *record* effector is untouched by any of the above,
    /// and this is what says the apology still fires where it belongs. The
    /// angle column is the only one that speaks the other vocabulary.
    #[test]
    fn stroke_speed_is_still_named_where_it_really_had_nowhere_to_go() {
        let bytes = sut(
            &[(
                "Textured",
                Variant::plain(1)
                    .set(
                        "TextureDensityEffector",
                        effector(VELOCITY, [0, 0, 50, 0], &[], &[]),
                    )
                    // …even beside an angle that follows the line, which is the
                    // combination that would hide a real loss behind the fix.
                    .int("BrushRotationEffector", (DIR_LINE | 3) as i64),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.brush.dab_angle_follows_stroke);
        assert_eq!(tool.dropped, [dropped::SPEED_ELSEWHERE]);
    }

    /// Clip Studio's taper-in is a size ramp over the first stretch of the
    /// mark, and Umber's stroke-position input is exactly that. The ramp is
    /// measured in dab radii, so the same brush scaled up tapers over a
    /// proportionally longer mark rather than finishing in the first inch.
    #[test]
    fn a_taper_in_becomes_a_size_ramp_along_the_stroke() {
        let bytes = sut(
            &[(
                "Tapered",
                Variant::plain(1)
                    .real("BrushSize", 20.0)
                    .int("BrushUseIn", 1)
                    .real("BrushInLength", 40.0),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        // 40 px over a radius of 10 is four radii of travel.
        assert!((tool.brush.stroke_span - 4.0).abs() < 1e-5);
        // And it must not wrap: a taper happens once, at the start of a mark.
        assert_eq!(tool.brush.stroke_hold, 10.0);
        assert!(tool.brush.uses_stroke_position());

        let m = tool
            .brush
            .modulations
            .as_slice()
            .iter()
            .find(|m| m.input == DabInput::Stroke)
            .expect("a stroke-position modulation");
        assert_eq!(m.target, DabTarget::Size);
        // Small at the start of the mark, its full size once the ramp is over.
        assert!(m.at(0.0) < -1.9, "{}", m.at(0.0));
        assert!((m.at(1.0) - 0.0).abs() < 1e-6);
        // Nothing is claimed about the far end, which cannot be done at all.
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);
    }

    /// The taper *out* is measured back from an end the engine does not know
    /// until the stroke is over, so it is named rather than approximated with
    /// something that would fire in the wrong place.
    #[test]
    fn a_taper_out_is_named_and_the_stroke_ramp_is_left_alone() {
        let bytes = sut(
            &[(
                "Only out",
                Variant::plain(1)
                    .int("BrushUseOut", 1)
                    .real("BrushOutLength", 20.0),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert_eq!(tool.dropped, [dropped::TAPER_OUT]);
        assert!(tool.brush.modulations.is_empty());
        // The ramp is the fast path's business: a brush that reads no stroke
        // position must not make the stroke builder start measuring one.
        assert!(!tool.brush.uses_stroke_position());
        assert_eq!(tool.brush.stroke_hold, Brush::default().stroke_hold);
    }

    /// Clip Studio leaves the taper's length in the file when the taper itself
    /// is switched off, so reading the number alone would put a ramp on every
    /// brush that had ever had one.
    #[test]
    fn a_taper_that_is_switched_off_leaves_no_ramp() {
        let bytes = sut(
            &[(
                "Plain",
                Variant::plain(1)
                    .int("BrushUseIn", 0)
                    .real("BrushInLength", 20.0),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.brush.modulations.is_empty());
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);
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
        // This material carries no full-resolution layer at all, which is a
        // real case — Clip Studio leaves an installed one out of the file — so
        // the thumbnail is what was used, and that is named.
        assert!(tool.dropped.contains(&dropped::THUMBNAIL_TIP));
    }

    /// **A tip whose real peak coverage is 1.0 must not import as a mask that
    /// peaks at 0.6**, and this is the test that would have caught the thing
    /// two rounds of bug reports were spent on.
    ///
    /// The thumbnail is a *downscaled preview*, and downscaling lowers the peak
    /// of any stamp that is not solid — while `CLAUDE.md` says exactly what a
    /// lowered peak produces: "a `max` caps a stroke at the mask's own
    /// brightest texel and paints half the author's mark". So the fixture makes
    /// the two disagree on purpose: the material is solid ink and its thumbnail
    /// is the same picture at three fifths of the strength. Reading the
    /// thumbnail gives a brush that paints at 60% however hard it is pressed.
    ///
    /// It pins the resolution as well, because the two are the same defect: a
    /// 300-pixel preview of a 400-pixel material is both fainter *and* coarser.
    #[test]
    fn a_tip_arrives_at_the_materials_own_strength_and_resolution() {
        let path = ".:full:data:material_0.layer";
        let bytes = sut(
            &[(
                "Spatter",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 1)),
            )],
            &[(
                path,
                material_with_pixels(
                    300,
                    300,
                    // The preview: the same ink, three fifths as strong.
                    [0, 0, 0, 153],
                    Some((400, 400, vec![255u8; 400 * 400])),
                ),
            )],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        let mask = tool.tip.as_ref().expect("a mask came with it");

        assert_eq!((mask.width(), mask.height()), (400, 400));
        assert_eq!(
            mask.coverage().iter().copied().max(),
            Some(255),
            "the material is solid ink and the mask has to be too"
        );
        // And the notice no longer apologises for a resolution that was not
        // lost. Naming it here is the false apology `unreachable_inputs`
        // refuses for pressure and randomness.
        assert!(
            !tool.dropped.contains(&dropped::THUMBNAIL_TIP),
            "{:?}",
            tool.dropped
        );
    }

    /// The engine can bind a mask up to [`TipMask::MAX_SIZE`] and a material
    /// may be larger, so it is reduced to fit rather than refused — because
    /// what refusing falls back to here is a 300-pixel preview, not the picture
    /// on disk. Named, like every other approximation this reader makes.
    #[test]
    fn a_material_larger_than_the_engine_can_stamp_is_reduced_and_named() {
        let path = ".:huge:data:material_0.layer";
        let (w, h) = (TipMask::MAX_SIZE + 600, (TipMask::MAX_SIZE + 600) / 2);
        let bytes = sut(
            &[(
                "Huge",
                Variant::plain(1)
                    .int("BrushUsePatternImage", 1)
                    .set("BrushPatternImageArray", reference(path, 1)),
            )],
            &[(
                path,
                material_with_pixels(
                    64,
                    32,
                    [0, 0, 0, 255],
                    Some((w, h, vec![255u8; (w * h) as usize])),
                ),
            )],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        let mask = tool.tip.as_ref().expect("a mask");

        assert_eq!(mask.width(), TipMask::MAX_SIZE);
        // Both axes by the same factor, so `aspect` still hands the dab pass
        // the material's own proportions.
        assert_eq!(mask.height(), TipMask::MAX_SIZE / 2);
        // A solid material stays solid through the reduction — near enough,
        // because the last level is `image`'s rounding rather than ours.
        assert!(mask.coverage().iter().copied().max().unwrap_or(0) >= 250);
        assert!(tool.dropped.contains(&dropped::REDUCED_TIP));
        // It is still the material rather than the thumbnail, so the other
        // loss is not named.
        assert!(!tool.dropped.contains(&dropped::THUMBNAIL_TIP));
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

    /// The strength, the tile size **and the picture** come across. The last of
    /// those is why `dropped::PAPER_TEXTURE` no longer appears: the brush is
    /// painting through its author's paper rather than through one of Umber's
    /// three, which is what it was apologising for.
    ///
    /// This is the thumbnail *fallback*, which is why it still names a loss —
    /// the fixture's material carries no `data/material_0.layer`, exactly as a
    /// material Clip Studio left out of the file does not.
    /// `a_paper_texture_comes_from_the_material_and_not_its_thumbnail` is the
    /// ordinary case beside it.
    #[test]
    fn a_paper_texture_brings_its_own_picture() {
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
        // The thumbnail says nothing about the material's size, so the tile is
        // `GRAIN_TILE_AT_FULL_SCALE` — the invented figure, in the one case
        // where there is nothing to derive one from.
        assert!((tool.brush.grain_scale - 64.0).abs() < 1e-4);
        assert!(tool.brush.has_grain());
        assert!(!tool.dropped.contains(&dropped::PAPER_TEXTURE));
        // But it is the material's *thumbnail*, which is a loss of its own and
        // has to be named — a brush claiming to carry its author's texture
        // while carrying a preview of it is the quiet kind of wrong.
        assert!(tool.dropped.contains(&dropped::THUMBNAIL_PAPER));

        // Brightness, not ink: a mid-grey paper keeps about half the dab.
        // Read the other way round this tile would keep the other half, which
        // on a real paper is every pit where the author drew a peak.
        let paper = tool.paper.as_ref().expect("the picture came with it");
        assert_eq!((paper.width(), paper.height()), (8, 8));
        assert!(
            (paper.at(4, 4) as i32 - 128).abs() <= 2,
            "got {}",
            paper.at(4, 4)
        );
    }

    /// **A brush with no tip at all still has to be measured for build-up**,
    /// because its paper can cap the stroke just as a faint stamp does.
    ///
    /// This is the bug as it was reported, in the smallest form that carries
    /// it. A sketch pencil arrived painting at roughly a quarter of the opacity
    /// it was set to: its texture is a dark grunge scatter at `TextureDensity`
    /// 100, and under Umber's `max` the whole stroke is capped at the tile's own
    /// mean for as long as the stroke lasts, where Clip Studio composites every
    /// dab and builds the faint texels back towards solid.
    ///
    /// Two of the four textured sub-tools in the reported file carry no bitmap
    /// tip, so `stroke_coverage` never ran on them at all — which is why the
    /// fixture has none either. `tip::grain_coverage`'s own test covers the
    /// half where it runs and cannot see the paper.
    #[test]
    fn a_paper_that_caps_the_stroke_arrives_with_build_up() {
        let path = ".:paper:data:material_0.layer";
        // A tenth lit, which is a paper that bites hard and is not a stencil:
        // faint texels are what compositing builds and a `max` cannot.
        let dark = material(8, 8, [26, 26, 26, 255]);
        let bytes = sut(
            &[(
                "Sketch",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 100),
            )],
            &[(path, dark)],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);

        assert!(tool.tip.is_none(), "the fixture has no stamp to be read");
        assert_eq!(tool.brush.grain, 1.0);
        assert!(
            tool.brush.build_up,
            "a paper this dark caps the stroke at a fraction of its own opacity"
        );

        // And a brush whose texture is switched off is untouched by any of it,
        // so the `max` path stays the default it always was.
        let plain = sut(&[("Flat", Variant::plain(1))], &[]);
        let plain = from_sut(&plain).expect("read").tools.remove(0);
        assert!(!plain.brush.has_grain());
        assert!(!plain.brush.build_up);
    }

    /// The paper is the **material's own pixels**, and the thumbnail is only
    /// the fallback — the wiring `tip_for` already had.
    ///
    /// The fixture's two pictures deliberately disagree: the thumbnail is a
    /// flat mid-grey and the material is a black-and-white check, so a reader
    /// that took the preview cannot pass by accident. That is the shape the
    /// bug had — every claim about resolution held, and the tile was a
    /// 300-pixel render of it.
    ///
    /// Three things ride on the route and each is checked here.
    ///
    /// - **The polarity.** `csmaterial` hands back *ink* and a grain texel is
    ///   what the dab **keeps**, so the plane is complemented. Getting it
    ///   backwards inverts somebody's paper, which reads as a texture biting in
    ///   exactly the wrong places rather than as a bug.
    /// - **The tile size.** `TextureScale2` is a percentage of the material's
    ///   own size, so a 400-texel material at 25% is 100 document pixels — not
    ///   the 64 the stood-in constant gives, which is what every textured Clip
    ///   Studio brush used to import at.
    /// - **The silence.** Nothing was given up, so nothing is named.
    #[test]
    fn a_paper_texture_comes_from_the_material_and_not_its_thumbnail() {
        let path = ".:paper:data:material_0.layer";
        // Ink: 0 is paper and 255 is a pit, which the reader complements.
        let checks: Vec<u8> = (0..400 * 400)
            .map(|i| {
                let (x, y) = (i % 400, i / 400);
                if (x / 50 + y / 50) % 2 == 0 { 0 } else { 255 }
            })
            .collect();
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 80)
                    .real("TextureScale2", 25.0),
            )],
            &[(
                path,
                material_with_pixels(8, 8, [128, 128, 128, 255], Some((400, 400, checks))),
            )],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);

        let paper = tool.paper.as_ref().expect("the picture came with it");
        assert_eq!((paper.width(), paper.height()), (400, 400));
        assert_eq!(
            paper.at(10, 10),
            255,
            "ink of 0 is paper that keeps the dab"
        );
        assert_eq!(
            paper.at(60, 10),
            0,
            "ink of 255 is a pit that takes it away"
        );

        assert!(
            (tool.brush.grain_scale - 100.0).abs() < 1e-4,
            "got {}",
            tool.brush.grain_scale
        );
        assert!(!tool.dropped.contains(&dropped::THUMBNAIL_PAPER));
        assert!(!tool.dropped.contains(&dropped::REDUCED_PAPER));
        assert!(!tool.dropped.contains(&dropped::PAPER_TEXTURE));
    }

    /// A material wider than a tile may be is **reduced and named**, not
    /// refused — the answer `REDUCED_TIP` already gives, and for the same
    /// reason: the alternative is not the picture on disk but a 300-pixel
    /// preview of it.
    ///
    /// The tile *size* is untouched by the reduction, and that is the half
    /// worth pinning: `TextureScale2` is a percentage of the material the
    /// author scaled, so a reduced tile has to cover the same document area as
    /// the picture it came from or the grain changes frequency to fit Umber's
    /// texture budget.
    #[test]
    fn a_paper_larger_than_a_tile_may_be_is_reduced_and_still_covers_its_own_ground() {
        let path = ".:paper:data:material_0.layer";
        let side = TipMask::MAX_SIZE + 400;
        let flat = vec![64u8; (side * side) as usize];
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 80)
                    .real("TextureScale2", 50.0),
            )],
            &[(
                path,
                material_with_pixels(8, 8, [128, 128, 128, 255], Some((side, side, flat))),
            )],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);

        let paper = tool.paper.as_ref().expect("the picture came with it");
        assert_eq!(paper.width(), TipMask::MAX_SIZE);
        assert!(tool.dropped.contains(&dropped::REDUCED_PAPER));
        // Half of the material's own side, not half of the reduced tile's.
        assert!(
            (tool.brush.grain_scale - side as f32 * 0.5).abs() < 1e-4,
            "got {}",
            tool.brush.grain_scale
        );
    }

    /// A texture the reader cannot resolve is still named as a loss, and the
    /// brush paints flat rather than through a paper nobody chose. This is the
    /// half of the old behaviour that was right.
    #[test]
    fn a_paper_the_reader_cannot_resolve_is_named_and_paints_flat() {
        let path = ".:paper:data:material_0.layer";
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 80),
            )],
            // Clip Studio leaves an installed material out of the file.
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.paper.is_none());
        assert!(tool.dropped.contains(&dropped::PAPER_TEXTURE));
        // **Flat means the strength too**, and this assertion is the whole of
        // the claim. Without it the test passes on a brush that paints through
        // `GrainPattern::Tooth` at 0.8: `paper` is `None`, so the preset names
        // no tile, so `Editor::paper_tile` falls back to `brush.grain_pattern`,
        // which this converter never writes and which defaults to `Tooth`. That
        // is the 78% substitution three doc comments claim was removed, and it
        // survived because "paints flat" was checked by reading the loss string
        // rather than the brush.
        assert_eq!(tool.brush.grain, 0.0);
        assert!(!tool.brush.has_grain());
    }

    /// `Brush::MAX_GRAIN_SCALE` cutting the tile is a **second** loss, and it
    /// is named rather than folded into `REDUCED_PAPER`.
    ///
    /// The two fire on almost the same condition — a material past
    /// `TipMask::MAX_SIZE`, at the default 100% scale — and mean opposite
    /// things: one is a picture coarser than it was drawn, the other is that
    /// picture repeated at *twice the frequency* the author set. The clamp was
    /// unreachable while the tile was always 256, so nothing caught it when the
    /// material's own size started feeding it.
    #[test]
    fn a_paper_too_coarse_for_umbers_own_ceiling_says_so_rather_than_being_quietly_respaced() {
        let path = ".:paper:data:material_0.layer";
        let side = Brush::MAX_GRAIN_SCALE as u32 + 512;
        let flat = vec![64u8; (side * side) as usize];
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(1)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 80)
                    // The default, and `unwrap_or`'s value — so this is the
                    // ordinary case rather than a contrived one.
                    .real("TextureScale2", 100.0),
            )],
            &[(
                path,
                material_with_pixels(8, 8, [128, 128, 128, 255], Some((side, side, flat))),
            )],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);

        assert!(
            (tool.brush.grain_scale - Brush::MAX_GRAIN_SCALE).abs() < 1e-4,
            "got {}",
            tool.brush.grain_scale
        );
        assert!(tool.dropped.contains(&dropped::PAPER_SPACING));
        // Both losses, because both happened: the picture was reduced *and*
        // the tile it is repeated at is finer than the file asked for.
        assert!(tool.dropped.contains(&dropped::REDUCED_PAPER));

        // And the ordinary paper loses neither. This is what stops the notice
        // appearing on every textured import, which is the failure the whole
        // `dropped` list is written to avoid.
        let path = ".:small:data:material_0.layer";
        let bytes = sut(
            &[(
                "Pencil",
                Variant::plain(2)
                    .set("TextureImage", reference(path, 1))
                    .int("TextureDensity", 80)
                    .real("TextureScale2", 19.0),
            )],
            &[(
                path,
                material_with_pixels(
                    8,
                    8,
                    [128, 128, 128, 255],
                    Some((500, 500, vec![64u8; 500 * 500])),
                ),
            )],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!((tool.brush.grain_scale - 95.0).abs() < 1e-4);
        assert!(!tool.dropped.contains(&dropped::PAPER_SPACING));
        assert!(!tool.dropped.contains(&dropped::REDUCED_PAPER));
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

    /// A dual brush that is switched **off** says nothing, however much of its
    /// settings block is still lying in the row.
    ///
    /// `UseDualBrush` is the one field that decides, and the rest of the family
    /// is the trap `BrushUseIn`, `BrushAutoIntervalType`, the rotation effector
    /// and the texture reference are each read to avoid. Every one of the 30
    /// variants in the two sample files has `UseDualBrush = 0` and residue
    /// beside it, and the residue is not even the same residue: the `.sut`
    /// leaves `DualSize = 30` and the `.sutg` leaves that column null and
    /// `DualTextureDensity = 50` instead, with `DualBrushCompositeMode = 1` and
    /// a `DualTextureDensityEffector` blob in both. So no neighbour of the flag
    /// can stand in for it, and reading one would put "dual brushes" on the
    /// notice of every Clip Studio brush anybody has ever imported — which is
    /// the list that cries wolf, over a feature none of them uses.
    ///
    /// The `Dual*` family is a parallel copy of the whole brush: tip, spray,
    /// stroke and paper, which is Clip Studio's `2-Brush tip`, `2-Spray
    /// effect`, `2-Stroke` and `2-Paper quality` under `2-Brush shape`. Umber
    /// binds one tip and one paper per brush, so there is nothing to
    /// approximate it with and `DUAL_BRUSH` names it rather than half of it
    /// being painted.
    #[test]
    fn a_dual_brush_that_is_switched_off_is_not_reported_from_the_values_left_beside_it() {
        let bytes = sut(
            &[(
                "Sketch",
                Variant::plain(1)
                    .int("UseDualBrush", 0)
                    // Exactly what both sample files leave behind.
                    .int("DualBrushCompositeMode", 1)
                    .int("DualSize", 30)
                    .int("DualTextureDensity", 50)
                    .int("ChangeRGBByDual", 0)
                    .int("DualUsePatternImage", 0),
            )],
            &[],
        );
        let tool = from_sut(&bytes).expect("read").tools.remove(0);
        assert!(tool.dropped.is_empty(), "{:?}", tool.dropped);
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
