//! The paths a vector layer holds.
//!
//! A vector layer is a layer that carries a [`VectorObject`] beside its (blank)
//! slot, the way a text layer carries a `TextObject`. This module is the model
//! and its record; hanging it on a layer, saving it and drawing it are other
//! modules' business, and nothing here knows about a GPU or a window.
//!
//! A path is a chain of cubic segments between consecutive nodes: `at`,
//! `at + handle_out`, `next.at + next.handle_in`, `next.at`. A zero handle at
//! both ends makes the segment straight. Everything is in document pixels,
//! y-down, so a path means the same thing at every zoom.
//!
//! - **The role spellings and every field name are a file format.** The record
//!   is written through a private mirror ([`Record`]) for the reason
//!   `textobj::Record` is one: the names are stated once, where a test can pin
//!   the whole text as a literal, and a rename in the model cannot rename a
//!   field on disk. `PathRole` derives serde because its three spellings are
//!   exactly what the file carries, and the pin is what catches a rename.
//! - **`Opaque` exists so that a newer file is not destroyed by an older
//!   build.** A record whose `version` is past [`VERSION`] cannot be read, but
//!   the layer is still a vector layer and a save must still write something
//!   back. So the bytes are kept whole, refuse every edit, and are written out
//!   verbatim. Read as an error instead, the layer would turn into a blank
//!   raster layer and the next save would delete the artist's paths.
//! - **`print` is optional and is written only beside ink.** It is the text
//!   record's [`Fingerprint`] of the layer image. Cut and Plot paths describe no
//!   pixels, so they cannot go stale and a fingerprint beside them could only
//!   ever make a reader discard something that was fine. Ink describes pixels.
//! - **Ink is reserved and nothing renders it.** The type is here so the file
//!   format does not have to change when a later stage draws it.
//! - **Every figure is bounded** ([`MAX_COORD`]) and finite, and the counts are
//!   bounded ([`MAX_NODES`], [`MAX_PATHS`]). The bound on the *record* is
//!   derived from those, so an over-long record is unwritable and the reading
//!   side is the only place that has to enforce it.

use glam::{UVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::geom::{FlipAxis, PixelRect};
use crate::textobj::Fingerprint;

/// The record's own revision.
pub const VERSION: u32 = 1;
/// Nodes in one layer.
pub const MAX_NODES: usize = 65_536;
/// Paths in one layer.
pub const MAX_PATHS: usize = 4_096;
/// The largest figure, in document pixels, a record may carry.
pub const MAX_COORD: f32 = 1.0e6;
/// The longest brush id an [`Ink`] may name, in bytes. A string is the one field
/// the counts above do not bound, so it needs a bound of its own for the
/// record's to be derivable.
pub const MAX_BRUSH_ID: usize = 128;

/// The widest a figure prints. `f32`'s shortest form is at worst
/// `-1.1754944e-38`, fourteen characters; the test that builds exactly that
/// node is what keeps this honest.
const WORST_FIGURE: usize = 14;
/// One node's text, measured at its worst: seven figures at [`WORST_FIGURE`],
/// the punctuation and field names, and the comma after it.
const PER_NODE_BYTES: usize = 7 * WORST_FIGURE + 46;
/// One path's text without its nodes, with an [`Ink`] whose brush id is as long
/// as it may be and every character in it escaped to six bytes (768).
const PER_PATH_BYTES: usize = 1024;
const MIB: usize = 1 << 20;

/// The bound on a record's size, derived from the bounds on what it may hold.
///
/// Twice what the largest legal record encodes to, rounded up to a MiB. The
/// slack covers the `print` field and a future field without this having to be
/// revisited; `the_byte_bound_admits_the_largest_legal_record` is what proves
/// the worst case fits.
pub const MAX_RECORD_BYTES: usize =
    (2 * (MAX_NODES * PER_NODE_BYTES + MAX_PATHS * PER_PATH_BYTES)).div_ceil(MIB) * MIB;

/// What a path is for.
///
/// The three spellings are written into the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathRole {
    /// A line a cutting machine follows.
    Cut,
    /// A line a plotter draws.
    Plot,
    /// A brush-look stroke. Reserved: nothing renders it yet.
    Ink,
}

impl PathRole {
    /// Every role, for pickers and for the guard that walks them.
    pub const ALL: [PathRole; 3] = [PathRole::Cut, PathRole::Plot, PathRole::Ink];

    /// The interface's word for it.
    pub fn label(self) -> &'static str {
        match self {
            PathRole::Cut => "Cut",
            PathRole::Plot => "Plot",
            PathRole::Ink => "Ink",
        }
    }
}

/// One point of a path, with the two handles that shape the segments either
/// side of it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    /// Document pixels.
    pub at: Vec2,
    /// Offset from `at`; zero is a corner.
    pub handle_in: Vec2,
    /// Offset from `at`; zero is a corner.
    pub handle_out: Vec2,
    /// Reserved for ink; 1.0 elsewhere.
    pub pressure: f32,
}

impl Node {
    /// A node with no handles.
    pub fn corner(at: Vec2) -> Self {
        Self {
            at,
            handle_in: Vec2::ZERO,
            handle_out: Vec2::ZERO,
            pressure: 1.0,
        }
    }
}

/// How an ink path would be drawn. Reserved.
#[derive(Clone, Debug, PartialEq)]
pub struct Ink {
    /// A preset's stable id.
    pub brush: String,
    pub size: f32,
    /// Linear RGBA, the shape `effect::linear_rgba` hands out.
    pub colour: [f32; 4],
}

/// A chain of cubic segments.
#[derive(Clone, Debug, PartialEq)]
pub struct Path {
    pub role: PathRole,
    pub closed: bool,
    pub nodes: Vec<Node>,
    /// Reserved; `None` for Cut and Plot.
    pub ink: Option<Ink>,
}

/// The most segments one cubic is cut into. A handle of a million pixels would
/// otherwise ask for thousands per segment, on the drawing path.
const MAX_SEGMENTS: usize = 256;

impl Path {
    /// Append this path as a polyline within `tolerance` document pixels, and
    /// clear nothing already in `out`.
    ///
    /// The one flattener: the overlay uses it now, and hit testing and the
    /// machine will later. A segment with no handle at either end emits only its
    /// end point. A closed path emits the closing segment and ends on its first
    /// point again.
    pub fn flatten(&self, tolerance: f32, out: &mut Vec<Vec2>) {
        let Some(first) = self.nodes.first() else {
            return;
        };
        out.push(first.at);
        for cubic in self.segments() {
            flatten_cubic(cubic, tolerance, out);
        }
    }

    /// The path's segments as the four control points of each cubic, start,
    /// start's handle, end's handle and end, in order, the closing segment last
    /// where the path is closed.
    ///
    /// The door for a caller that has to decide *per segment* whether to
    /// flatten at all — the overlay culls a cubic by its control-point hull
    /// before spending anything on it, because one legal path at the highest
    /// zoom flattens to millions of points almost all of which are off screen.
    /// [`flatten_cubic`] is what turns one into points, and
    /// [`Path::flatten`] is this and that and nothing else, so there is still
    /// one flattener.
    pub fn segments(&self) -> impl Iterator<Item = [Vec2; 4]> + '_ {
        let closing = match (self.closed, self.nodes.first(), self.nodes.last()) {
            (true, Some(first), Some(last)) if self.nodes.len() >= 2 => Some((last, first)),
            _ => None,
        };
        self.nodes
            .windows(2)
            .map(|pair| (&pair[0], &pair[1]))
            .chain(closing)
            .map(|(a, b)| [a.at, a.at + a.handle_out, b.at + b.handle_in, b.at])
    }

    /// The box of the nodes *and* their handles, which is a bound on the curve
    /// (a cubic lies inside the hull of its control points), or `None` for a
    /// path with no nodes.
    pub fn bounds(&self) -> Option<(Vec2, Vec2)> {
        let mut it = self.nodes.iter();
        let first = it.next()?;
        let mut lo = first.at;
        let mut hi = first.at;
        for n in self.nodes.iter() {
            for p in [n.at, n.at + n.handle_in, n.at + n.handle_out] {
                lo = lo.min(p);
                hi = hi.max(p);
            }
        }
        Some((lo, hi))
    }
}

/// Append one cubic within `tolerance` document pixels, its end point last.
/// The start point `p[0]` is the caller's and is not emitted.
///
/// **The one flattener**, which [`Path::flatten`] calls per segment and the
/// overlay calls on the segments it did not cull. A cubic whose handles both
/// sit on their own ends is a straight line and emits only its end point.
pub fn flatten_cubic(p: [Vec2; 4], tolerance: f32, out: &mut Vec<Vec2>) {
    let [p0, p1, p2, p3] = p;
    if p1 == p0 && p2 == p3 {
        out.push(p3);
        return;
    }
    let tolerance = tolerance.max(1.0e-3);
    // The standard second-difference bound on how far the curve strays from its
    // chord when cut into `n` equal steps in `t`.
    let d = (p0 - 2.0 * p1 + p2)
        .length()
        .max((p1 - 2.0 * p2 + p3).length());
    let n = (d / (8.0 / 6.0 * tolerance)).sqrt().ceil();
    let n = if n.is_finite() {
        (n as usize).clamp(1, MAX_SEGMENTS)
    } else {
        MAX_SEGMENTS
    };
    for i in 1..n {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        out.push(
            p0 * (u * u * u) + p1 * (3.0 * u * u * t) + p2 * (3.0 * u * t * t) + p3 * (t * t * t),
        );
    }
    // Exactly the end point, not an evaluation that lands near it, so a closed
    // path really does return to where it started.
    out.push(p3);
}

/// Why a path could not be installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VectorRefusal {
    TooManyNodes,
    TooManyPaths,
    /// A figure is not finite or is out of bounds, or a path has too few nodes.
    Impossible,
    /// The record was made by a newer Umber and cannot be changed here.
    Opaque,
}

impl VectorRefusal {
    pub const ALL: [VectorRefusal; 4] = [
        VectorRefusal::TooManyNodes,
        VectorRefusal::TooManyPaths,
        VectorRefusal::Impossible,
        VectorRefusal::Opaque,
    ];

    /// What the interface says.
    pub fn sentence(self) -> &'static str {
        match self {
            VectorRefusal::TooManyNodes => {
                "This layer cannot hold that many points. Simplify the shape or use another vector layer."
            }
            VectorRefusal::TooManyPaths => {
                "This layer cannot hold more paths. Use another vector layer."
            }
            VectorRefusal::Impossible => "That shape could not be turned into a path.",
            VectorRefusal::Opaque => {
                "This layer's paths were made by a newer Umber and cannot be changed here."
            }
        }
    }
}

/// Why a record could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecordError {
    /// Longer than [`MAX_RECORD_BYTES`]; not parsed.
    TooLarge,
    Unreadable(String),
}

/// The per-path half of [`VectorObject::check`]: at least two nodes, and every
/// figure finite and within [`MAX_COORD`]. One statement, so `check` and
/// [`VectorObject::bounded`] cannot disagree about which path is legal.
fn path_in_bounds(path: &Path) -> bool {
    let ok = |v: f32| v.is_finite() && v.abs() <= MAX_COORD;
    let ok2 = |v: Vec2| ok(v.x) && ok(v.y);
    if path.nodes.len() < 2 {
        return false;
    }
    let nodes = path
        .nodes
        .iter()
        .all(|n| ok2(n.at) && ok2(n.handle_in) && ok2(n.handle_out) && ok(n.pressure));
    let ink = path.ink.as_ref().is_none_or(|ink| {
        ok(ink.size) && ink.colour.iter().all(|c| ok(*c)) && ink.brush.len() <= MAX_BRUSH_ID
    });
    nodes && ink
}

/// What a vector layer holds.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorObject {
    Paths(Vec<Path>),
    /// A record from a newer Umber, kept byte for byte and refusing every edit.
    Opaque(Vec<u8>),
}

impl VectorObject {
    pub fn empty() -> Self {
        VectorObject::Paths(Vec::new())
    }

    pub fn paths(&self) -> &[Path] {
        match self {
            VectorObject::Paths(p) => p,
            VectorObject::Opaque(_) => &[],
        }
    }

    pub fn is_opaque(&self) -> bool {
        match self {
            VectorObject::Paths(_) => false,
            VectorObject::Opaque(_) => true,
        }
    }

    /// Does any path describe pixels?
    pub fn has_ink(&self) -> bool {
        self.paths()
            .iter()
            .any(|p| p.ink.is_some() || p.role == PathRole::Ink)
    }

    pub fn node_count(&self) -> usize {
        self.paths().iter().map(|p| p.nodes.len()).sum()
    }

    /// What this costs the undo budget.
    pub fn byte_len(&self) -> usize {
        match self {
            VectorObject::Paths(paths) => paths
                .iter()
                .map(|p| {
                    std::mem::size_of::<Path>()
                        + p.nodes.len() * std::mem::size_of::<Node>()
                        + p.ink.as_ref().map_or(0, |i| i.brush.len())
                })
                .sum(),
            VectorObject::Opaque(bytes) => bytes.len(),
        }
    }

    /// The bounds a record is held to: counts, finiteness and size of every
    /// figure, and at least two nodes to a path.
    pub fn check(&self) -> Result<(), VectorRefusal> {
        let paths = match self {
            VectorObject::Paths(p) => p,
            VectorObject::Opaque(_) => return Ok(()),
        };
        if paths.len() > MAX_PATHS {
            return Err(VectorRefusal::TooManyPaths);
        }
        if self.node_count() > MAX_NODES {
            return Err(VectorRefusal::TooManyNodes);
        }
        if paths.iter().all(path_in_bounds) {
            Ok(())
        } else {
            Err(VectorRefusal::Impossible)
        }
    }

    /// This record with every path that fails [`VectorObject::check`]'s
    /// per-path rule taken out, and whether any was.
    ///
    /// **What a transform runs after it has moved the nodes.** `flipped` and
    /// `translated` do not clamp, because clamping deforms a shape, so a path
    /// far off the canvas can be pushed past [`MAX_COORD`] by a mirror, or by
    /// enough anchored resizes. Left there, the record would fail `check` at the
    /// next save and `to_json` would refuse the *whole* record, taking every
    /// other path on the layer with it. Judged per path so the rest survive; the
    /// counts cannot have grown, since a transform adds no path and no node.
    pub fn bounded(self) -> (Self, bool) {
        match self {
            VectorObject::Paths(paths) => {
                let before = paths.len();
                let kept: Vec<Path> = paths.into_iter().filter(path_in_bounds).collect();
                let lost = kept.len() != before;
                (VectorObject::Paths(kept), lost)
            }
            VectorObject::Opaque(bytes) => (VectorObject::Opaque(bytes), false),
        }
    }

    /// This record with `more` after its paths, or why not. Changes nothing on a
    /// refusal.
    pub fn appended(&self, more: Vec<Path>) -> Result<Self, VectorRefusal> {
        let mut paths = match self {
            VectorObject::Paths(p) => p.clone(),
            VectorObject::Opaque(_) => return Err(VectorRefusal::Opaque),
        };
        paths.extend(more);
        let next = VectorObject::Paths(paths);
        next.check()?;
        Ok(next)
    }

    /// This record with path `index` replaced by `path`, or why not. Changes
    /// nothing on a refusal. An index off the end is `Impossible`, since there
    /// is no path to replace.
    pub fn with_path(&self, index: usize, path: Path) -> Result<Self, VectorRefusal> {
        let mut paths = match self {
            VectorObject::Paths(p) => p.clone(),
            VectorObject::Opaque(_) => return Err(VectorRefusal::Opaque),
        };
        let slot = paths.get_mut(index).ok_or(VectorRefusal::Impossible)?;
        *slot = path;
        let next = VectorObject::Paths(paths);
        next.check()?;
        Ok(next)
    }

    /// This record with path `index` taken out and the rest shifted down, or
    /// why not.
    pub fn without_path(&self, index: usize) -> Result<Self, VectorRefusal> {
        let mut paths = match self {
            VectorObject::Paths(p) => p.clone(),
            VectorObject::Opaque(_) => return Err(VectorRefusal::Opaque),
        };
        if index >= paths.len() {
            return Err(VectorRefusal::Impossible);
        }
        paths.remove(index);
        let next = VectorObject::Paths(paths);
        next.check()?;
        Ok(next)
    }

    /// The record on a mirrored canvas.
    ///
    /// A flip is **not** exactly its own inverse in `f32`: `c - (c - x)` rounds
    /// for a figure that is not a dyadic fraction. A round trip moves a figure
    /// by at most one rounding at the canvas's magnitude and repeating it does
    /// not drift further; `a_double_flip_costs_one_rounding_and_never_drifts`
    /// states and tests both. The in and out handles are *not* swapped: each stays on the
    /// side of its node it was on, and a handle is an offset, so only the
    /// mirrored component changes sign.
    pub fn flipped(&self, axis: FlipAxis, canvas: UVec2) -> Self {
        let paths = match self {
            VectorObject::Paths(p) => p,
            VectorObject::Opaque(_) => return self.clone(),
        };
        let mirror = |p: Vec2| match axis {
            FlipAxis::Horizontal => Vec2::new(canvas.x as f32 - p.x, p.y),
            FlipAxis::Vertical => Vec2::new(p.x, canvas.y as f32 - p.y),
        };
        let turn = |h: Vec2| match axis {
            FlipAxis::Horizontal => Vec2::new(-h.x, h.y),
            FlipAxis::Vertical => Vec2::new(h.x, -h.y),
        };
        VectorObject::Paths(
            paths
                .iter()
                .map(|p| Path {
                    nodes: p
                        .nodes
                        .iter()
                        .map(|n| Node {
                            at: mirror(n.at),
                            handle_in: turn(n.handle_in),
                            handle_out: turn(n.handle_out),
                            pressure: n.pressure,
                        })
                        .collect(),
                    ..p.clone()
                })
                .collect(),
        )
    }

    /// Every node moved by `by`; handles are offsets and stay put.
    ///
    /// Not clamped: clamping would deform the shape. A figure pushed past
    /// [`MAX_COORD`] is left as it is here, and the caller runs
    /// [`VectorObject::bounded`] afterwards to take that path out, as
    /// `LayerStack::translate_vectors` does; [`VectorObject::check`], which
    /// [`VectorObject::to_json`] runs, is the backstop behind that.
    pub fn translated(&self, by: Vec2) -> Self {
        let paths = match self {
            VectorObject::Paths(p) => p,
            VectorObject::Opaque(_) => return self.clone(),
        };
        VectorObject::Paths(
            paths
                .iter()
                .map(|p| Path {
                    nodes: p
                        .nodes
                        .iter()
                        .map(|n| Node {
                            at: n.at + by,
                            ..*n
                        })
                        .collect(),
                    ..p.clone()
                })
                .collect(),
        )
    }

    /// The Cut and Plot paths alone, for when the pixels ink described are gone.
    pub fn without_ink(&self) -> Self {
        let paths = match self {
            VectorObject::Paths(p) => p,
            VectorObject::Opaque(_) => return self.clone(),
        };
        VectorObject::Paths(
            paths
                .iter()
                .filter(|p| p.role != PathRole::Ink)
                .map(|p| Path {
                    ink: None,
                    ..p.clone()
                })
                .collect(),
        )
    }

    /// One closed path of corners per ring.
    ///
    /// Merges exactly collinear runs and exact duplicates and nothing more, so
    /// a rectangle's ring is four nodes. A ring left with fewer than three nodes
    /// encloses nothing and is dropped. It does not smooth: a ring traced out of
    /// a mask is a staircase and stays one.
    pub fn from_rings(rings: &[Vec<Vec2>], role: PathRole) -> Vec<Path> {
        rings
            .iter()
            .filter_map(|ring| {
                let mut pts: Vec<Vec2> = Vec::with_capacity(ring.len());
                for &p in ring {
                    if pts.last() != Some(&p) {
                        pts.push(p);
                    }
                }
                while pts.len() > 1 && pts.first() == pts.last() {
                    pts.pop();
                }
                let n = pts.len();
                if n < 3 {
                    return None;
                }
                let nodes: Vec<Node> = (0..n)
                    .filter(|&i| {
                        let prev = pts[(i + n - 1) % n];
                        let next = pts[(i + 1) % n];
                        (pts[i] - prev).perp_dot(next - pts[i]) != 0.0
                    })
                    .map(|i| Node::corner(pts[i]))
                    .collect();
                if nodes.len() < 3 {
                    return None;
                }
                Some(Path {
                    role,
                    closed: true,
                    nodes,
                    ink: None,
                })
            })
            .collect()
    }

    /// The record as the archive holds it.
    ///
    /// Compact rather than pretty, for the reason `textobj` gives: a pretty
    /// printer takes the platform's line ending and a document travels.
    /// `print` is written only where there is ink for it to be about.
    pub fn to_json(&self, print: Option<&Fingerprint>) -> Result<Vec<u8>, VectorRefusal> {
        let paths = match self {
            VectorObject::Opaque(bytes) => return Ok(bytes.clone()),
            VectorObject::Paths(p) => p,
        };
        self.check()?;
        let record = Record {
            version: VERSION,
            paths: paths.iter().map(PathRecord::of).collect(),
            print: print.filter(|_| self.has_ink()).map(PrintRecord::of),
        };
        let json = serde_json::to_vec(&record).map_err(|_| VectorRefusal::Impossible)?;
        // Unreachable while the bound is derived from the counts, and a refusal
        // rather than an assertion because a writer that panics is worse.
        if json.len() > MAX_RECORD_BYTES {
            return Err(VectorRefusal::Impossible);
        }
        Ok(json)
    }

    /// Read a record back, with the fingerprint it was written with if any.
    ///
    /// Over [`MAX_RECORD_BYTES`] is refused before it is parsed. A newer
    /// `version` is kept whole as [`VectorObject::Opaque`]. A path with fewer
    /// than two nodes is dropped rather than refusing the record, because the
    /// other paths are still the artist's.
    pub fn from_json(bytes: &[u8]) -> Result<(Self, Option<Fingerprint>), RecordError> {
        if bytes.len() > MAX_RECORD_BYTES {
            return Err(RecordError::TooLarge);
        }
        let unreadable = |e: serde_json::Error| RecordError::Unreadable(e.to_string());
        let probe: Probe = serde_json::from_slice(bytes).map_err(unreadable)?;
        if probe.version > u64::from(VERSION) {
            return Ok((VectorObject::Opaque(bytes.to_vec()), None));
        }
        let record: Record = serde_json::from_slice(bytes).map_err(unreadable)?;
        let print = match record.print {
            Some(p) => Some(p.into_print()?),
            None => None,
        };
        let paths: Vec<Path> = record
            .paths
            .into_iter()
            .map(PathRecord::into_path)
            .filter(|p| p.nodes.len() >= 2)
            .collect();
        let object = VectorObject::Paths(paths);
        object
            .check()
            .map_err(|r| RecordError::Unreadable(r.sentence().to_string()))?;
        Ok((object, print))
    }
}

/// Just enough of a record to learn whether this build can read the rest.
#[derive(Deserialize)]
struct Probe {
    version: u64,
}

/// The record. **Field names and order are the file format.**
///
/// No container `#[serde(default)]`: each optional field has its own neutral,
/// and `role`, `at`, `version` and `paths` have none, so a file missing one is
/// refused rather than read as an arbitrary value.
#[derive(Serialize, Deserialize)]
struct Record {
    version: u32,
    paths: Vec<PathRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    print: Option<PrintRecord>,
}

#[derive(Serialize, Deserialize)]
struct PathRecord {
    role: PathRole,
    #[serde(default)]
    closed: bool,
    nodes: Vec<NodeRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ink: Option<InkRecord>,
}

#[derive(Serialize, Deserialize)]
struct NodeRecord {
    at: [f32; 2],
    #[serde(default, rename = "in")]
    handle_in: [f32; 2],
    #[serde(default, rename = "out")]
    handle_out: [f32; 2],
    #[serde(default = "one")]
    pressure: f32,
}

#[derive(Serialize, Deserialize)]
struct InkRecord {
    brush: String,
    size: f32,
    colour: [f32; 4],
}

/// The text record's fingerprint, spelt the way that record spells it: a hash as
/// sixteen hex digits, because a `u64` past 2^53 is not a number every JSON
/// reader can carry exactly.
#[derive(Serialize, Deserialize)]
struct PrintRecord {
    rect: [u32; 4],
    hash: String,
}

fn one() -> f32 {
    1.0
}

impl PathRecord {
    fn of(p: &Path) -> Self {
        Self {
            role: p.role,
            closed: p.closed,
            nodes: p
                .nodes
                .iter()
                .map(|n| NodeRecord {
                    at: n.at.to_array(),
                    handle_in: n.handle_in.to_array(),
                    handle_out: n.handle_out.to_array(),
                    pressure: n.pressure,
                })
                .collect(),
            ink: p.ink.as_ref().map(|i| InkRecord {
                brush: i.brush.clone(),
                size: i.size,
                colour: i.colour,
            }),
        }
    }

    fn into_path(self) -> Path {
        Path {
            role: self.role,
            closed: self.closed,
            nodes: self
                .nodes
                .into_iter()
                .map(|n| Node {
                    at: Vec2::from_array(n.at),
                    handle_in: Vec2::from_array(n.handle_in),
                    handle_out: Vec2::from_array(n.handle_out),
                    pressure: n.pressure,
                })
                .collect(),
            ink: self.ink.map(|i| Ink {
                brush: i.brush,
                size: i.size,
                colour: i.colour,
            }),
        }
    }
}

impl PrintRecord {
    fn of(f: &Fingerprint) -> Self {
        Self {
            rect: [f.rect.x, f.rect.y, f.rect.width, f.rect.height],
            hash: format!("{:016x}", f.hash),
        }
    }

    fn into_print(self) -> Result<Fingerprint, RecordError> {
        let hash = u64::from_str_radix(&self.hash, 16)
            .map_err(|_| RecordError::Unreadable("the fingerprint is not a hash".to_string()))?;
        Ok(Fingerprint {
            rect: PixelRect {
                x: self.rect[0],
                y: self.rect[1],
                width: self.rect[2],
                height: self.rect[3],
            },
            hash,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2::new(x, y)
    }

    fn open(points: &[(f32, f32)]) -> Path {
        Path {
            role: PathRole::Cut,
            closed: false,
            nodes: points.iter().map(|&(x, y)| Node::corner(v(x, y))).collect(),
            ink: None,
        }
    }

    /// A quarter of a circle of radius 100 about the origin, from (100, 0) to
    /// (0, 100), by the usual kappa.
    fn quarter() -> Path {
        const K: f32 = 0.552_284_7;
        Path {
            role: PathRole::Plot,
            closed: false,
            nodes: vec![
                Node {
                    at: v(100.0, 0.0),
                    handle_in: Vec2::ZERO,
                    handle_out: v(0.0, 100.0 * K),
                    pressure: 1.0,
                },
                Node {
                    at: v(0.0, 100.0),
                    handle_in: v(100.0 * K, 0.0),
                    handle_out: Vec2::ZERO,
                    pressure: 1.0,
                },
            ],
            ink: None,
        }
    }

    fn fixture() -> VectorObject {
        let mut curved = open(&[(5.0, 5.0), (50.0, 5.0)]);
        curved.role = PathRole::Plot;
        curved.nodes[0].handle_out = v(10.0, 20.0);
        curved.nodes[1].handle_in = v(-10.5, 0.0);
        let mut cut = open(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0)]);
        cut.closed = true;
        VectorObject::Paths(vec![cut, curved])
    }

    #[test]
    fn replacing_a_path_out_of_bounds_is_refused() {
        let far = open(&[(0.0, 0.0), (MAX_COORD * 2.0, 0.0)]);
        assert_eq!(fixture().with_path(0, far), Err(VectorRefusal::Impossible));
        let fine = open(&[(0.0, 0.0), (3.0, 0.0)]);
        let got = fixture().with_path(1, fine.clone()).unwrap();
        assert_eq!(got.paths()[1], fine);
        assert_eq!(got.paths().len(), 2);
    }

    #[test]
    fn an_opaque_record_takes_no_replacement() {
        let opaque = VectorObject::Opaque(vec![1, 2, 3]);
        let p = open(&[(0.0, 0.0), (1.0, 0.0)]);
        assert_eq!(opaque.with_path(0, p), Err(VectorRefusal::Opaque));
        assert_eq!(opaque.without_path(0), Err(VectorRefusal::Opaque));
    }

    #[test]
    fn removing_a_path_shifts_the_rest() {
        let f = fixture();
        let second = f.paths()[1].clone();
        let got = f.without_path(0).unwrap();
        assert_eq!(got.paths(), &[second]);
    }

    #[test]
    fn an_index_off_the_end_is_refused() {
        let f = fixture();
        assert_eq!(f.without_path(2), Err(VectorRefusal::Impossible));
        let p = open(&[(0.0, 0.0), (1.0, 0.0)]);
        assert_eq!(f.with_path(2, p), Err(VectorRefusal::Impossible));
    }

    #[test]
    fn a_straight_path_flattens_to_its_nodes() {
        let mut out = Vec::new();
        open(&[(0.0, 0.0), (5.0, 0.0), (5.0, 5.0)]).flatten(0.1, &mut out);
        assert_eq!(out, vec![v(0.0, 0.0), v(5.0, 0.0), v(5.0, 5.0)]);
    }

    #[test]
    fn a_closed_path_returns_to_its_start() {
        let mut p = open(&[(0.0, 0.0), (5.0, 0.0), (5.0, 5.0)]);
        p.closed = true;
        let mut out = Vec::new();
        p.flatten(0.1, &mut out);
        assert_eq!(out.len(), 4);
        assert_eq!(out.last(), out.first());
    }

    #[test]
    fn a_quarter_circle_flattens_within_tolerance() {
        let mut last = 0;
        for tol in [1.0, 0.25, 0.05] {
            let mut out = Vec::new();
            quarter().flatten(tol, &mut out);
            // The vertices lie on the Bezier, which is within 0.02 of the
            // circle however coarsely it is cut, so they say nothing about the
            // tolerance. What the tolerance bounds is how far the *chords* sag.
            for pair in out.windows(2) {
                for k in 0..=20 {
                    let p = pair[0].lerp(pair[1], k as f32 / 20.0);
                    assert!(
                        (p.length() - 100.0).abs() <= tol + 0.03,
                        "chord point {p} at {tol}"
                    );
                }
            }
            assert!(out.len() >= last, "finer tolerance gave fewer points");
            last = out.len();
        }
        assert!(last > 4);
    }

    #[test]
    fn flattening_is_bounded() {
        let mut p = quarter();
        p.nodes[0].handle_out = v(1.0e6, 1.0e6);
        p.nodes[1].handle_in = v(-1.0e6, 1.0e6);
        let mut out = Vec::new();
        p.flatten(1.0e-9, &mut out);
        assert!(out.len() <= 1 + 256);
    }

    /// `segments` is every cubic in order with the closing one last, and
    /// flattening them one by one from the first node is `flatten` exactly —
    /// so the overlay, which culls per segment, draws the curve `flatten` does.
    #[test]
    fn the_segments_flatten_to_exactly_what_the_path_does() {
        let mut p = quarter();
        p.nodes.push(Node::corner(v(-40.0, 30.0)));
        p.closed = true;
        let cubics: Vec<[Vec2; 4]> = p.segments().collect();
        assert_eq!(
            cubics.len(),
            3,
            "two runs between nodes and the closing one"
        );
        assert_eq!(cubics[2][0], v(-40.0, 30.0));
        assert_eq!(
            cubics[2][3],
            v(100.0, 0.0),
            "the closing cubic ends on the start"
        );
        let mut whole = Vec::new();
        p.flatten(0.1, &mut whole);
        let mut pieced = vec![p.nodes[0].at];
        for c in cubics {
            flatten_cubic(c, 0.1, &mut pieced);
        }
        assert_eq!(whole, pieced);

        let mut open_one = p.clone();
        open_one.closed = false;
        assert_eq!(open_one.segments().count(), 2);
    }

    #[test]
    fn flipping_twice_restores_figures_that_are_exact_in_f32() {
        let canvas = UVec2::new(700, 500);
        let a = fixture();
        for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
            let once = a.flipped(axis, canvas);
            assert_ne!(once, a);
            assert_eq!(once.flipped(axis, canvas), a);
        }
    }

    /// `c - (c - x)` rounds for a figure that is not a dyadic fraction, so a
    /// flip is not exactly its own inverse. What is true, and is what is tested:
    /// a round trip moves a figure by at most one rounding at the canvas's
    /// magnitude, and it does not accumulate.
    #[test]
    fn a_double_flip_costs_one_rounding_and_never_drifts() {
        for canvas in [UVec2::new(700, 500), UVec2::new(32768, 32768)] {
            let xs = [0.1_f32, 33.37, 699.9, -12.34, -0.7, 700.3, 5000.123];
            let nodes: Vec<Node> = xs
                .iter()
                .zip(xs.iter().rev())
                .map(|(&x, &y)| Node {
                    at: v(x, y),
                    handle_in: v(0.3, -0.7),
                    handle_out: v(-1.1, 2.3),
                    pressure: 1.0,
                })
                .collect();
            let a = VectorObject::Paths(vec![Path {
                role: PathRole::Cut,
                closed: false,
                nodes,
                ink: None,
            }]);
            let bound = canvas.x.max(canvas.y) as f32 * f32::EPSILON;
            for axis in [FlipAxis::Horizontal, FlipAxis::Vertical] {
                let twice = a.flipped(axis, canvas).flipped(axis, canvas);
                for (n, m) in a.paths()[0].nodes.iter().zip(&twice.paths()[0].nodes) {
                    assert!((n.at - m.at).abs().max_element() <= bound, "{n:?} {m:?}");
                    assert_eq!(n.handle_in, m.handle_in);
                    assert_eq!(n.handle_out, m.handle_out);
                }
                let mut state = twice.clone();
                for round in 2..=1000 {
                    state = state.flipped(axis, canvas).flipped(axis, canvas);
                    assert_eq!(state, twice, "drifted at round {round}");
                }
            }
        }
    }

    #[test]
    fn a_flip_mirrors_about_the_canvas() {
        let mut p = open(&[(10.0, 20.0), (30.0, 40.0)]);
        p.nodes[0].handle_in = v(3.0, 4.0);
        let flipped =
            VectorObject::Paths(vec![p]).flipped(FlipAxis::Horizontal, UVec2::new(700, 500));
        let n = flipped.paths()[0].nodes[0];
        assert_eq!(n.at, v(690.0, 20.0));
        assert_eq!(n.handle_in, v(-3.0, 4.0));
    }

    #[test]
    fn translating_moves_nodes_and_not_handles() {
        let mut p = open(&[(1.0, 2.0), (3.0, 4.0)]);
        p.nodes[0].handle_out = v(7.0, 8.0);
        let moved = VectorObject::Paths(vec![p]).translated(v(10.0, -1.0));
        let n = moved.paths()[0].nodes[0];
        assert_eq!(n.at, v(11.0, 1.0));
        assert_eq!(n.handle_out, v(7.0, 8.0));
    }

    #[test]
    fn a_rectangles_rings_become_four_nodes() {
        let ring = vec![
            v(0.0, 0.0),
            v(5.0, 0.0),
            v(10.0, 0.0),
            v(10.0, 5.0),
            v(10.0, 10.0),
            v(5.0, 10.0),
            v(0.0, 10.0),
            v(0.0, 5.0),
            v(0.0, 0.0),
        ];
        let paths = VectorObject::from_rings(&[ring], PathRole::Cut);
        assert_eq!(paths.len(), 1);
        assert!(paths[0].closed);
        let at: Vec<Vec2> = paths[0].nodes.iter().map(|n| n.at).collect();
        assert_eq!(
            at,
            vec![v(0.0, 0.0), v(10.0, 0.0), v(10.0, 10.0), v(0.0, 10.0)]
        );
    }

    #[test]
    fn a_ring_with_two_distinct_points_is_dropped() {
        let ring = vec![v(0.0, 0.0), v(5.0, 5.0), v(5.0, 5.0), v(0.0, 0.0)];
        assert!(VectorObject::from_rings(&[ring], PathRole::Plot).is_empty());
    }

    #[test]
    fn no_role_is_missing_from_all() {
        for (i, role) in PathRole::ALL.into_iter().enumerate() {
            // An exhaustive match that names its own position, so a fourth role
            // cannot compile without being placed.
            let at = match role {
                PathRole::Cut => 0,
                PathRole::Plot => 1,
                PathRole::Ink => 2,
            };
            assert_eq!(at, i);
        }
        assert_eq!(PathRole::ALL.len(), 3);
        assert_eq!(PathRole::Cut.label(), "Cut");
        assert_eq!(PathRole::Plot.label(), "Plot");
        assert_eq!(PathRole::Ink.label(), "Ink");
    }

    #[test]
    fn the_record_is_exactly_this_text() {
        let text = String::from_utf8(fixture().to_json(None).unwrap()).unwrap();
        assert_eq!(
            text,
            concat!(
                r#"{"version":1,"paths":["#,
                r#"{"role":"Cut","closed":true,"nodes":["#,
                r#"{"at":[0.0,0.0],"in":[0.0,0.0],"out":[0.0,0.0],"pressure":1.0},"#,
                r#"{"at":[10.0,0.0],"in":[0.0,0.0],"out":[0.0,0.0],"pressure":1.0},"#,
                r#"{"at":[10.0,10.0],"in":[0.0,0.0],"out":[0.0,0.0],"pressure":1.0}]},"#,
                r#"{"role":"Plot","closed":false,"nodes":["#,
                r#"{"at":[5.0,5.0],"in":[0.0,0.0],"out":[10.0,20.0],"pressure":1.0},"#,
                r#"{"at":[50.0,5.0],"in":[-10.5,0.0],"out":[0.0,0.0],"pressure":1.0}]}]}"#,
            )
        );
        assert!(!text.contains("print"));
        assert!(!text.contains("ink"));
    }

    #[test]
    fn a_record_round_trips() {
        let mut a = fixture();
        if let VectorObject::Paths(p) = &mut a {
            p[1].ink = Some(Ink {
                brush: "pack01/pen".to_string(),
                size: 3.5,
                colour: [0.1, 0.2, 0.3, 1.0],
            });
            p[1].role = PathRole::Ink;
            p[0].nodes[1].pressure = 0.25;
        }
        let print = Fingerprint {
            rect: PixelRect {
                x: 1,
                y: 2,
                width: 30,
                height: 40,
            },
            hash: u64::MAX - 3,
        };
        let bytes = a.to_json(Some(&print)).unwrap();
        let (back, p) = VectorObject::from_json(&bytes).unwrap();
        assert_eq!(back, a);
        assert_eq!(p, Some(print));
    }

    #[test]
    fn a_path_with_no_role_is_refused() {
        let text = br#"{"version":1,"paths":[{"nodes":[{"at":[0,0]},{"at":[1,1]}]}]}"#;
        assert!(matches_unreadable(VectorObject::from_json(text)));
    }

    fn matches_unreadable(r: Result<(VectorObject, Option<Fingerprint>), RecordError>) -> bool {
        match r {
            Err(RecordError::Unreadable(_)) => true,
            Err(RecordError::TooLarge) | Ok(_) => false,
        }
    }

    #[test]
    fn a_nan_or_huge_figure_is_refused() {
        for figure in ["1e30", "-1e30", "1e39", "1e999"] {
            let text = format!(
                r#"{{"version":1,"paths":[{{"role":"Cut","nodes":[{{"at":[{figure},0]}},{{"at":[1,1]}}]}}]}}"#
            );
            assert!(
                matches_unreadable(VectorObject::from_json(text.as_bytes())),
                "{figure}"
            );
        }
    }

    #[test]
    fn a_newer_record_is_kept_whole() {
        let bytes = br#"{"version":2,"shape":"x"}"#;
        let (obj, print) = VectorObject::from_json(bytes).unwrap();
        assert!(obj.is_opaque());
        assert_eq!(print, None);
        assert_eq!(obj.to_json(None).unwrap(), bytes.to_vec());
    }

    #[test]
    fn a_version_too_large_for_u32_is_still_a_newer_record() {
        let bytes = br#"{"version":99999999999,"paths":[]}"#;
        let (obj, _) = VectorObject::from_json(bytes).unwrap();
        assert!(obj.is_opaque());
        assert_eq!(obj.to_json(None).unwrap(), bytes.to_vec());
    }

    #[test]
    fn a_path_with_one_node_is_dropped_and_the_rest_kept() {
        let text = br#"{"version":1,"paths":[{"role":"Cut","nodes":[{"at":[0,0]}]},{"role":"Plot","nodes":[{"at":[0,0]},{"at":[4,4]}]}]}"#;
        let (obj, _) = VectorObject::from_json(text).unwrap();
        assert_eq!(obj.paths().len(), 1);
        assert_eq!(obj.paths()[0].role, PathRole::Plot);
    }

    #[test]
    fn a_record_translated_out_of_bounds_refuses_to_serialise() {
        let moved = fixture().translated(v(MAX_COORD, 0.0));
        assert_eq!(moved.check(), Err(VectorRefusal::Impossible));
        assert_eq!(moved.to_json(None), Err(VectorRefusal::Impossible));
    }

    #[test]
    fn a_record_over_the_bound_is_refused_unparsed() {
        let bytes = vec![b'x'; MAX_RECORD_BYTES + 1];
        assert_eq!(VectorObject::from_json(&bytes), Err(RecordError::TooLarge));
    }

    #[test]
    fn a_print_is_written_only_beside_ink() {
        let print = Fingerprint {
            rect: PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
            hash: 7,
        };
        let plain = fixture().to_json(Some(&print)).unwrap();
        assert!(!String::from_utf8(plain).unwrap().contains("print"));
        let mut inked = fixture();
        if let VectorObject::Paths(p) = &mut inked {
            p[0].role = PathRole::Ink;
        }
        let with = String::from_utf8(inked.to_json(Some(&print)).unwrap()).unwrap();
        assert!(with.contains(r#""print":{"rect":[0,0,1,1],"hash":"0000000000000007"}"#));
        let without = String::from_utf8(inked.to_json(None).unwrap()).unwrap();
        assert!(!without.contains("print"));
    }

    #[test]
    fn too_many_nodes_is_refused_and_changes_nothing() {
        let base = fixture();
        let big = Path {
            role: PathRole::Cut,
            closed: false,
            nodes: vec![Node::corner(Vec2::ZERO); MAX_NODES + 1],
            ink: None,
        };
        assert_eq!(base.appended(vec![big]), Err(VectorRefusal::TooManyNodes));
        assert_eq!(base, fixture());
        let ok = base
            .appended(vec![open(&[(0.0, 0.0), (1.0, 1.0)])])
            .unwrap();
        assert_eq!(ok.paths().len(), 3);
    }

    #[test]
    fn an_opaque_record_cannot_be_appended_to() {
        let o = VectorObject::Opaque(b"{}".to_vec());
        assert_eq!(
            o.appended(vec![open(&[(0.0, 0.0), (1.0, 1.0)])]),
            Err(VectorRefusal::Opaque)
        );
        for r in VectorRefusal::ALL {
            assert!(!r.sentence().contains('\u{2014}'));
        }
    }

    #[test]
    fn the_byte_bound_admits_the_largest_legal_record() {
        // The widest a figure can print, in every slot of every node.
        let w = -1.175_494_4e-38_f32;
        let node = Node {
            at: v(w, w),
            handle_in: v(w, w),
            handle_out: v(w, w),
            pressure: w,
        };
        let one_node = serde_json::to_string(&NodeRecord {
            at: node.at.to_array(),
            handle_in: node.handle_in.to_array(),
            handle_out: node.handle_out.to_array(),
            pressure: node.pressure,
        })
        .unwrap();
        assert!(
            one_node.len() < PER_NODE_BYTES,
            "a node is {} bytes",
            one_node.len()
        );
        // Every character of the brush id escaped to six bytes.
        let ink = Ink {
            brush: "\u{1}".repeat(MAX_BRUSH_ID),
            size: w,
            colour: [w; 4],
        };
        let per = MAX_NODES / MAX_PATHS;
        let paths: Vec<Path> = (0..MAX_PATHS)
            .map(|_| Path {
                role: PathRole::Ink,
                closed: true,
                nodes: vec![node; per],
                ink: Some(ink.clone()),
            })
            .collect();
        let object = VectorObject::Paths(paths);
        let bytes = object.to_json(None).unwrap();
        assert!(
            bytes.len() * 2 <= MAX_RECORD_BYTES,
            "{} bytes against {}",
            bytes.len(),
            MAX_RECORD_BYTES
        );
        let (back, _) = VectorObject::from_json(&bytes).unwrap();
        assert_eq!(back.node_count(), MAX_NODES);
    }
}
