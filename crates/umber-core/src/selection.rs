//! Selections: which pixels of the document an edit is allowed to touch.
//!
//! # What a selection *is*
//!
//! A [`Selection`] is **an outline plus a coverage mask**, and it is both
//! deliberately.
//!
//! The obvious representation is one byte per document pixel. It is simple and
//! it is what the GPU eventually needs, but it is also 4 MB on a 2048² canvas
//! and 100 MB on a 10000² one — a canvas Umber supports, and the reason
//! `band_rows` exists in the renderer. Almost all of that would be zero: a
//! selection is usually a small part of the picture. So the mask here is
//! **bounded to the selection's own pixel rectangle**, which makes a lasso
//! round one eye of a portrait cost what that eye covers rather than what the
//! portrait does.
//!
//! The other obvious representation is the path alone, tested per pixel with
//! point-in-polygon. That is exact and tiny, and it is the wrong thing to hand
//! a fragment shader: clipping happens once per fragment of every dab, and no
//! amount of cleverness makes "walk a thousand lasso segments" a per-fragment
//! cost. It also has no answer for a partly covered pixel, and a selection edge
//! without antialiasing is a staircase the artist can see.
//!
//! So: the mask is what gets used, and the outline is kept because the mask
//! cannot answer the two questions the outline can. Drawing the marching ants
//! from a mask would mean tracing a boundary back out of pixels — a second
//! algorithm, approximate where the path is exact. And a mask is tied to one
//! canvas size, where the rings are geometry and can be rasterised again.
//!
//! Combining two selections is the one thing that cannot hold to that, and
//! "What a boolean costs the outline" below says exactly what it gives up.
//!
//! # Antialiasing and the fill rule
//!
//! [`rasterise`] runs [`SUB_SCANLINES`] sub-scanlines per pixel row and
//! accumulates **exact** horizontal coverage of each span, so a vertical edge
//! is continuous and a horizontal one lands on one of five levels. That
//! asymmetry is deliberate: exact horizontal coverage is nearly free (it is
//! arithmetic on the span ends) where exact vertical coverage would mean
//! clipping polygons, and four sub-rows is enough that the difference is
//! invisible. An axis-aligned rectangle — much the most common selection —
//! comes out exact on *both* axes, because its horizontal edges fall between
//! sub-scanlines rather than through them.
//!
//! The fill rule is **nonzero winding**, not even-odd. A freehand lasso that
//! crosses itself is one region to the person who drew it; even-odd would
//! punch a hole in the middle of their own loop.
//!
//! # Combining selections
//!
//! Holding a modifier while drawing adds the new shape to the selection or
//! takes it away — [`SelectionOp`]. **The boolean happens on the coverage, not
//! on the rings.** Coverage is a rectangle of bytes: union is `max`, difference
//! is `min(a, 255 - b)`, both exact per pixel and both linear in the area
//! touched. Polygon boolean geometry is the alternative and it is a large,
//! bug-prone algorithm — self-intersection, coincident edges, degenerate
//! vertices — for a result the mask already has exactly.
//!
//! `max`/`min(a, 255 - b)` rather than `a + b - ab` and its dual: they are
//! idempotent, so adding a shape to itself is the identity where the
//! probabilistic pair would fatten it a little every time; they are exact
//! wherever either operand is fully in or fully out, which is every pixel
//! except the one-wide antialiased band at an edge; and they never manufacture
//! coverage the geometry does not have. What they cost is a seam: two shapes
//! that meet along a shared edge each cover it about half, and `max` of two
//! halves is a half rather than the whole the two together really do cover. It
//! is one pixel wide, it is what Photoshop and GIMP do, and the alternative is
//! to keep the geometry — which is the thing this avoids.
//!
//! # What a boolean costs the outline
//!
//! The rings cannot survive it. Concatenating two ring lists is a union only
//! when the shapes are disjoint — where they overlap it draws a seam straight
//! through the middle of the merged region — and reversing a ring is a
//! difference only when it falls entirely inside. So after a boolean the rings
//! are **traced back out of the mask**, by [`trace_rings`], and that is the
//! second, approximate algorithm the section above says the rings exist to
//! avoid. Being honest about what that loses:
//!
//! - **The outline becomes pixel-quantised.** A lasso's smooth diagonal is a
//!   staircase once it has been added to something. Every application that
//!   draws marching ants from a mask has this, and the alternative is exactly
//!   the polygon boolean this design refused.
//! - **The geometry stops being resolution-independent.** Rasterising traced
//!   rings onto a different canvas reproduces the staircase rather than the
//!   curve. Nothing does that today — `Editor::apply_canvas` drops the
//!   selection outright on a resize, for the same reason it drops the history
//!   — so this is a door closed, not a behaviour lost.
//! - **The threshold is 50%.** [`Selection::contains`] answers off the traced
//!   ring and [`Selection::coverage_at`] off the mask, so along an antialiased
//!   edge the two can disagree by one pixel. They already could: `bounds` is
//!   the outline's box rounded outwards.
//!
//! An unmodified gesture — much the commonest — keeps its exact rings. Tracing
//! only ever runs where a boolean actually ran.
//!
//! # What is not here
//!
//! No intersect, no feather, no "select by colour". Each is a real feature and
//! none is drawn in the interface, which is the rule this file lives by as much
//! as any other.

use crate::geom::{PixelRect, Rect};
use glam::{UVec2, Vec2};

/// Sub-scanlines per pixel row. See the module docs.
const SUB_SCANLINES: u32 = 4;

/// Coverage at or above which a pixel counts as inside, for [`trace_rings`].
///
/// Half. A traced outline has to fall somewhere in the antialiased band and the
/// middle of it is the only choice that treats the two sides alike.
const INSIDE: u8 = 128;

/// How a selection outline is drawn.
///
/// One tool with a mode rather than three tools: they produce the same thing
/// and differ only in the gesture, so three entries in the rail would be three
/// names for one selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Drag a box.
    #[default]
    Rectangle,
    /// Freehand — the outline follows the pointer.
    Lasso,
    /// Click point to point; each click adds a straight edge. Usually called a
    /// polygonal lasso.
    Polygon,
}

impl SelectionMode {
    pub const ALL: [SelectionMode; 3] = [Self::Rectangle, Self::Lasso, Self::Polygon];

    pub fn label(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Lasso => "Lasso",
            Self::Polygon => "Polygon",
        }
    }

    /// What the gesture is, for the options strip.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Rectangle => "Drag a box.",
            Self::Lasso => "Draw round it freehand.",
            Self::Polygon => {
                "Click point to point. Click the first point again, or press \
                 Enter, to close the shape."
            }
        }
    }
}

/// What a new shape does to the selection already standing.
///
/// Which modifier means which is the interface's, not the engine's — see
/// `app.rs` — because it has to be reconciled with what Alt already does on the
/// canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionOp {
    /// The new shape *is* the selection. What an unmodified gesture does.
    #[default]
    Replace,
    /// Union. Two disjoint areas can both be selected; two that touch become
    /// one region with one outline.
    Add,
    /// Difference. The new shape is taken out of what was selected.
    Subtract,
}

/// A region of the document, as an outline and a coverage mask over the
/// outline's bounding rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Selection {
    /// Closed rings in document space. The closing edge is implicit: the last
    /// point joins the first, so a ring is never stored with its start
    /// repeated.
    rings: Vec<Vec<Vec2>>,
    bounds: PixelRect,
    /// `bounds.width * bounds.height` bytes, row-major from `bounds`'s
    /// top-left. `0` is outside, `255` is fully inside.
    coverage: Vec<u8>,
}

impl Selection {
    /// Build a selection from closed rings in document space.
    ///
    /// Returns `None` when nothing of the shape lands on the canvas — an empty
    /// selection and no selection are the same thing to every caller, and
    /// making that an `Option` here means none of them has to check for a
    /// zero-area rectangle later.
    pub fn from_rings(rings: Vec<Vec<Vec2>>, doc: UVec2) -> Option<Self> {
        let mut extent = Rect::empty();
        for ring in &rings {
            for p in ring {
                extent.union_box(*p, Vec2::ZERO);
            }
        }
        let bounds = extent.to_pixels_clamped(doc)?;
        let coverage = rasterise(&rings, bounds);
        // A shape thinner than a pixel can have a bounding rect and no
        // coverage at all. That is nothing selected, which is `None`.
        if coverage.iter().all(|c| *c == 0) {
            return None;
        }
        Some(Self {
            rings,
            bounds,
            coverage,
        })
    }

    /// An axis-aligned box between two corners, in either order.
    pub fn rectangle(a: Vec2, b: Vec2, doc: UVec2) -> Option<Self> {
        let min = a.min(b);
        let max = a.max(b);
        Self::from_rings(
            vec![vec![
                min,
                Vec2::new(max.x, min.y),
                max,
                Vec2::new(min.x, max.y),
            ]],
            doc,
        )
    }

    /// One closed ring through `points`, which is what both the lasso and the
    /// polygonal lasso produce — they differ in how the points were gathered,
    /// not in what they mean.
    pub fn polygon(points: &[Vec2], doc: UVec2) -> Option<Self> {
        if points.len() < 3 {
            return None;
        }
        Self::from_rings(vec![points.to_vec()], doc)
    }

    /// The pixel rectangle the selection covers. Never zero-area.
    pub fn bounds(&self) -> PixelRect {
        self.bounds
    }

    /// The mask, row-major over [`Selection::bounds`].
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// Coverage at one *document* pixel. Outside the bounds is outside the
    /// selection, which is `0` rather than a panic — callers walk rectangles
    /// that need not line up with this one.
    pub fn coverage_at(&self, x: u32, y: u32) -> u8 {
        let b = self.bounds;
        if x < b.x || y < b.y || x >= b.x + b.width || y >= b.y + b.height {
            return 0;
        }
        let i = (y - b.y) as usize * b.width as usize + (x - b.x) as usize;
        self.coverage[i]
    }

    /// Is this document point inside the outline?
    ///
    /// Answered from the **path**, by nonzero winding, not from the mask: this
    /// is what a hit test wants — "did the user press inside the selection" —
    /// and reading a rounded byte would put the boundary half a pixel away from
    /// where the outline is drawn.
    pub fn contains(&self, point: Vec2) -> bool {
        self.rings.iter().map(|r| winding(r, point)).sum::<i32>() != 0
    }

    /// The closed rings, for drawing the outline.
    pub fn rings(&self) -> &[Vec<Vec2>] {
        &self.rings
    }

    /// Build a selection from a coverage mask, trimming it to what is actually
    /// covered and tracing its outline.
    ///
    /// The two booleans below both end here, which is what keeps the trim and
    /// the trace in one place. Trimming is not tidiness: a difference leaves
    /// empty rows and columns behind, and `bounds` is what sizes the texture
    /// the dab pass samples, so an untrimmed mask would upload the hole it just
    /// cut. It is also how "subtracted down to nothing" becomes `None`, which
    /// is the same answer as no selection everywhere else in this file.
    fn from_mask(bounds: PixelRect, coverage: Vec<u8>) -> Option<Self> {
        let (bounds, coverage) = trim(bounds, coverage)?;
        let rings = trace_rings(bounds, &coverage);
        // A mask can be non-empty and still trace nothing: coverage below
        // `INSIDE` everywhere is a selection so faint no outline can be drawn
        // for it, and an outline nobody can see is a selection nobody can find.
        if rings.is_empty() {
            return None;
        }
        Some(Self {
            rings,
            bounds,
            coverage,
        })
    }

    /// This selection with `other` added to it.
    ///
    /// The bounding rectangle **grows**, so the mask is reallocated and
    /// re-origined against the union of the two — which is the one place an
    /// off-by-one here would put every selected pixel one across.
    pub fn union(&self, other: &Self) -> Option<Self> {
        let bounds = union_rects(self.bounds, other.bounds);
        let mut coverage = vec![0u8; bounds.area() as usize];
        // `max`, not a probabilistic add: see the module docs.
        self.blit_into(&mut coverage, bounds, |dst, src| dst.max(src));
        other.blit_into(&mut coverage, bounds, |dst, src| dst.max(src));
        Self::from_mask(bounds, coverage)
    }

    /// This selection with `other` taken out of it.
    ///
    /// The bounds can only shrink, so the mask starts as this one's and
    /// [`Selection::from_mask`] trims whatever the cut emptied.
    pub fn difference(&self, other: &Self) -> Option<Self> {
        let mut coverage = self.coverage.clone();
        // `min(a, 255 - b)`, the dual of the `max` above: where `other` is
        // fully in, nothing survives; where it is half in, at most half does.
        other.blit_into(&mut coverage, self.bounds, |dst, src| dst.min(255 - src));
        Self::from_mask(self.bounds, coverage)
    }

    /// Apply `op` to the selection standing, with the shape just drawn.
    ///
    /// Every empty case is decided here rather than at the call site, because
    /// they do not all answer the same way. A `Replace` that encloses nothing
    /// **deselects** — a bare click is how every paint application spells that.
    /// An `Add` or a `Subtract` that encloses nothing leaves the selection
    /// exactly as it was: a slip of the hand while holding a modifier is not a
    /// request to throw the work away. And subtracting from nothing is nothing,
    /// because "no selection" means the whole document and taking a shape out
    /// of it would be a bigger claim than the gesture made.
    ///
    /// `shape` is taken by value so `Replace` — much the commonest — moves it
    /// through untouched, with no copy of the mask and no outline traced.
    pub fn combined(base: Option<&Self>, shape: Option<Self>, op: SelectionOp) -> Option<Self> {
        match (op, base, shape) {
            (SelectionOp::Replace, _, shape) => shape,
            (SelectionOp::Subtract, None, _) => None,
            (SelectionOp::Add, None, shape) => shape,
            (_, Some(base), None) => Some(base.clone()),
            (SelectionOp::Add, Some(base), Some(shape)) => base.union(&shape),
            (SelectionOp::Subtract, Some(base), Some(shape)) => base.difference(&shape),
        }
    }

    /// Combine this selection's coverage into `dst`, which spans `rect`.
    ///
    /// Row by row over the overlap only, so a small shape added to a large
    /// selection costs the small shape rather than the large one.
    fn blit_into(&self, dst: &mut [u8], rect: PixelRect, mut f: impl FnMut(u8, u8) -> u8) {
        let x0 = self.bounds.x.max(rect.x);
        let y0 = self.bounds.y.max(rect.y);
        let x1 = (self.bounds.x + self.bounds.width).min(rect.x + rect.width);
        let y1 = (self.bounds.y + self.bounds.height).min(rect.y + rect.height);
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        let span = (x1 - x0) as usize;
        for y in y0..y1 {
            let d = (y - rect.y) as usize * rect.width as usize + (x0 - rect.x) as usize;
            let s = (y - self.bounds.y) as usize * self.bounds.width as usize
                + (x0 - self.bounds.x) as usize;
            for i in 0..span {
                dst[d + i] = f(dst[d + i], self.coverage[s + i]);
            }
        }
    }
}

/// The smallest rectangle holding both.
fn union_rects(a: PixelRect, b: PixelRect) -> PixelRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let right = (a.x + a.width).max(b.x + b.width);
    let bottom = (a.y + a.height).max(b.y + b.height);
    PixelRect {
        x,
        y,
        width: right - x,
        height: bottom - y,
    }
}

/// Shrink `rect` to the rows and columns of `coverage` that hold anything, and
/// re-origin the mask onto it. `None` when nothing is covered at all.
fn trim(rect: PixelRect, coverage: Vec<u8>) -> Option<(PixelRect, Vec<u8>)> {
    let w = rect.width as usize;
    let mut min_x = w;
    let mut max_x = 0usize;
    let mut min_y = rect.height as usize;
    let mut max_y = 0usize;
    for (y, row) in coverage.chunks_exact(w).enumerate() {
        let first = row.iter().position(|c| *c != 0);
        let Some(first) = first else { continue };
        // `rposition` from the end rather than a second scan of the whole row:
        // an interior row of a large selection is covered edge to edge, and
        // both ends are then found in one step each.
        let last = row.iter().rposition(|c| *c != 0).unwrap_or(first);
        min_x = min_x.min(first);
        max_x = max_x.max(last);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    if min_x > max_x || min_y > max_y {
        return None;
    }
    let trimmed = PixelRect {
        x: rect.x + min_x as u32,
        y: rect.y + min_y as u32,
        width: (max_x - min_x + 1) as u32,
        height: (max_y - min_y + 1) as u32,
    };
    if trimmed == rect {
        return Some((rect, coverage));
    }
    let tw = trimmed.width as usize;
    let mut out = Vec::with_capacity(tw * trimmed.height as usize);
    for y in min_y..=max_y {
        out.extend_from_slice(&coverage[y * w + min_x..y * w + max_x + 1]);
    }
    Some((trimmed, out))
}

/// The winding number of `ring` about `point`, by the standard crossing count.
///
/// A ray is cast in +x. An edge counts once when it crosses the ray, signed by
/// whether it was going down or up. The half-open comparison (`<=` on one end,
/// `>` on the other) is what stops a vertex exactly on the ray being counted
/// twice — the classic off-by-one in this algorithm, and the one that makes a
/// rectangle's corner report as outside itself.
fn winding(ring: &[Vec2], point: Vec2) -> i32 {
    let mut w = 0;
    for i in 0..ring.len() {
        let a = ring[i];
        let b = ring[(i + 1) % ring.len()];
        if a.y <= point.y {
            if b.y > point.y && cross(a, b, point) > 0.0 {
                w += 1;
            }
        } else if b.y <= point.y && cross(a, b, point) < 0.0 {
            w -= 1;
        }
    }
    w
}

/// Which side of the line `a -> b` the point falls on. Positive is left.
fn cross(a: Vec2, b: Vec2, p: Vec2) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (p.x - a.x) * (b.y - a.y)
}

/// Fill `rings` into an 8-bit coverage mask over `rect`.
///
/// Scanline, nonzero winding, [`SUB_SCANLINES`] sub-rows per pixel row with
/// exact horizontal coverage — see the module docs for why the two axes are
/// treated differently.
fn rasterise(rings: &[Vec<Vec2>], rect: PixelRect) -> Vec<u8> {
    let width = rect.width as usize;
    let mut out = vec![0u8; width * rect.height as usize];
    // Both reused across every sub-scanline of every row: this runs once per
    // selection, but a lasso is thousands of segments over thousands of rows
    // and a fresh allocation per sub-row would be millions of them.
    let mut acc = vec![0.0f32; width];
    let mut crossings: Vec<(f32, i32)> = Vec::new();

    let weight = 1.0 / SUB_SCANLINES as f32;
    for row in 0..rect.height {
        acc.fill(0.0);
        for sub in 0..SUB_SCANLINES {
            let sy = rect.y as f32 + row as f32 + (sub as f32 + 0.5) / SUB_SCANLINES as f32;
            crossings.clear();
            for ring in rings {
                for i in 0..ring.len() {
                    let a = ring[i];
                    let b = ring[(i + 1) % ring.len()];
                    // Half-open in y, so a vertex shared by two edges is
                    // crossed exactly once and a horizontal edge not at all.
                    let (lo, hi, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
                    if sy < lo.y || sy >= hi.y {
                        continue;
                    }
                    let t = (sy - lo.y) / (hi.y - lo.y);
                    crossings.push((lo.x + t * (hi.x - lo.x), dir));
                }
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));

            let mut winding = 0;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding != 0 {
                    add_span(&mut acc, rect.x as f32, pair[0].0, pair[1].0, weight);
                }
            }
        }

        let base = row as usize * width;
        for (i, a) in acc.iter().enumerate() {
            out[base + i] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

/// Add the horizontal coverage of the document-space span `[x0, x1)` to `acc`,
/// whose first entry is the pixel at document x `origin`.
///
/// Exact: a span covering three tenths of a pixel adds three tenths.
fn add_span(acc: &mut [f32], origin: f32, x0: f32, x1: f32, weight: f32) {
    let a = (x0 - origin).max(0.0);
    let b = (x1 - origin).min(acc.len() as f32);
    if b <= a {
        return;
    }
    let first = a.floor() as usize;
    let last = (b.ceil() as usize).min(acc.len());
    for (i, cell) in acc.iter_mut().enumerate().take(last).skip(first) {
        let lo = a.max(i as f32);
        let hi = b.min(i as f32 + 1.0);
        if hi > lo {
            *cell += (hi - lo) * weight;
        }
    }
}

// ---------------------------------------------------------------------------
// Tracing an outline back out of a mask
// ---------------------------------------------------------------------------

/// A directed boundary edge, as stored in the two grids below.
const FORWARD: u8 = 1;
/// The same edge run the other way — see [`trace_rings`].
const BACKWARD: u8 = 2;

/// Trace the closed rings round the pixels of `coverage` that are at least
/// [`INSIDE`], in document space.
///
/// Only ever called after a boolean, and the module docs say what that costs.
/// It runs at pointer-up, once, and it is linear in the area — a full-canvas
/// selection on a 2048² document builds about 16 MB of grids and gives them
/// straight back, which is the same order as the rasterisation that produced
/// the mask a moment earlier. Nothing on the drawing path may reach it.
///
/// The method is boundary-edge walking, not marching squares over a 2×2 window:
/// it is the same information and it comes out already oriented. Every pixel
/// that is inside contributes one **directed** edge for each neighbour that is
/// outside, oriented so the inside is always on the *right* of the direction of
/// travel. Outer rings then come out wound the way [`Selection::rectangle`]
/// winds — the top edge running +x — and a ring round a hole comes out wound
/// the other way, so nonzero winding empties the hole with no special case and
/// [`Selection::contains`] keeps working on a traced outline.
///
/// Each vertex has at most two outgoing edges, and two only where two inside
/// pixels meet corner to corner across two outside ones. There the walk turns
/// as sharply as it can — the first candidate is a right turn — which keeps
/// diagonally touching regions apart rather than pinching them into one ring.
/// The choice is made against the *original* edges rather than the ones still
/// unwalked, so it pairs each arrival with exactly one departure and the rings
/// are the cycles of that pairing; a walk therefore ends by arriving back at
/// the edge it started from and can never wander onto another ring.
fn trace_rings(bounds: PixelRect, coverage: &[u8]) -> Vec<Vec<Vec2>> {
    let w = bounds.width as usize;
    let h = bounds.height as usize;
    let inside = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && coverage[y as usize * w + x as usize] >= INSIDE
    };

    // Horizontal edges live on the grid lines between pixel rows: `h + 1` rows
    // of `w`. Vertical ones on the lines between columns: `h` rows of `w + 1`.
    let mut hor = vec![0u8; (h + 1) * w];
    let mut ver = vec![0u8; h * (w + 1)];
    for gy in 0..=h {
        for gx in 0..w {
            hor[gy * w + gx] = match (
                inside(gx as isize, gy as isize - 1),
                inside(gx as isize, gy as isize),
            ) {
                // The top edge of an inside pixel, run +x: inside below, which
                // is the right-hand side when facing +x with y pointing down.
                (false, true) => FORWARD,
                // Its bottom edge, run -x, for the same reason mirrored.
                (true, false) => BACKWARD,
                _ => 0,
            };
        }
    }
    for gy in 0..h {
        for gx in 0..=w {
            ver[gy * (w + 1) + gx] = match (
                inside(gx as isize - 1, gy as isize),
                inside(gx as isize, gy as isize),
            ) {
                // The right edge of an inside pixel, run +y.
                (true, false) => FORWARD,
                // Its left edge, run -y.
                (false, true) => BACKWARD,
                _ => 0,
            };
        }
    }

    // An edge is named by its index into one grid or the other. Horizontal
    // indices come first so one number covers both.
    let split = hor.len();
    let dir_of = |e: usize| -> u8 {
        if e < split {
            // 0 is +x, 2 is -x.
            if hor[e] == FORWARD { 0 } else { 2 }
        } else if ver[e - split] == FORWARD {
            // 1 is +y, 3 is -y.
            1
        } else {
            3
        }
    };
    let ends_of = |e: usize| -> ((usize, usize), (usize, usize)) {
        if e < split {
            let (gx, gy) = (e % w, e / w);
            if hor[e] == FORWARD {
                ((gx, gy), (gx + 1, gy))
            } else {
                ((gx + 1, gy), (gx, gy))
            }
        } else {
            let i = e - split;
            let (gx, gy) = (i % (w + 1), i / (w + 1));
            if ver[i] == FORWARD {
                ((gx, gy), (gx, gy + 1))
            } else {
                ((gx, gy + 1), (gx, gy))
            }
        }
    };
    // The edge leaving `(gx, gy)` in direction `d`, if there is one.
    let leaving = |(gx, gy): (usize, usize), d: u8| -> Option<usize> {
        match d {
            0 if gx < w && hor[gy * w + gx] == FORWARD => Some(gy * w + gx),
            2 if gx > 0 && hor[gy * w + gx - 1] == BACKWARD => Some(gy * w + gx - 1),
            1 if gy < h && ver[gy * (w + 1) + gx] == FORWARD => Some(split + gy * (w + 1) + gx),
            3 if gy > 0 && ver[(gy - 1) * (w + 1) + gx] == BACKWARD => {
                Some(split + (gy - 1) * (w + 1) + gx)
            }
            _ => None,
        }
    };

    let mut walked = vec![false; hor.len() + ver.len()];
    let mut rings = Vec::new();
    let mut ring: Vec<Vec2> = Vec::new();
    for start in 0..walked.len() {
        let present = if start < split {
            hor[start] != 0
        } else {
            ver[start - split] != 0
        };
        if !present || walked[start] {
            continue;
        }
        ring.clear();
        let mut e = start;
        loop {
            walked[e] = true;
            let (from, to) = ends_of(e);
            ring.push(Vec2::new(
                bounds.x as f32 + from.0 as f32,
                bounds.y as f32 + from.1 as f32,
            ));
            let d = dir_of(e);
            // Right, straight on, left, back the way we came.
            let Some(next) = [1u8, 0, 3, 2]
                .into_iter()
                .find_map(|turn| leaving(to, (d + turn) % 4))
            else {
                break;
            };
            // Back at an edge already claimed — normally the one this ring
            // began at. Anything else would mean the pairing above was not a
            // pairing, and stopping is the safe reading either way.
            if walked[next] {
                break;
            }
            e = next;
        }
        // Only the corners are kept. A straight run along the edge of a large
        // selection is one segment however many pixels long it is, which is
        // what stops the outline being redrawn from thousands of points every
        // frame for the rest of the session.
        let corners = corners_only(&ring);
        if corners.len() >= 3 {
            rings.push(corners);
        }
    }
    rings
}

/// Drop the points of a closed ring that lie in the middle of a straight run.
fn corners_only(ring: &[Vec2]) -> Vec<Vec2> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let here = ring[i];
        let next = ring[(i + 1) % n];
        // Axis-aligned throughout, so "same direction" is a sign test and
        // needs no tolerance.
        let a = here - prev;
        let b = next - here;
        if a.x * b.y != a.y * b.x {
            out.push(here);
        }
    }
    out
}

/// A selection being drawn: the gesture, before it becomes a [`Selection`].
///
/// Lives here rather than in the interface because what each mode does with a
/// press, a move and a release is a rule, not a drawing — and a rule is
/// testable without a window. `panels.rs` and `dock.rs` keep the same division.
#[derive(Clone, Debug)]
pub struct SelectionDraft {
    mode: SelectionMode,
    /// What this gesture will do to the selection standing. Snapshotted when
    /// the gesture *begins* and never read again — see
    /// [`SelectionDraft::combining`].
    op: SelectionOp,
    /// Rectangle: the corner the drag started at. Lasso: every sampled point.
    /// Polygon: every vertex clicked so far.
    points: Vec<Vec2>,
    /// Where the pointer is now. For the rectangle this is the opposite
    /// corner; for the polygon it is the rubber-band end of the next edge.
    cursor: Vec2,
}

/// The smallest step, in document pixels, between two recorded lasso points.
///
/// A pointer at 1000 Hz over a canvas at 8x zoom reports hundreds of samples
/// per document pixel, and every one of them is an edge the rasteriser walks on
/// every sub-scanline it spans. Dropping the ones that say nothing costs
/// nothing visible and bounds the shape.
const LASSO_STEP: f32 = 1.0;

impl SelectionDraft {
    /// Begin at `at`, in document space.
    pub fn new(mode: SelectionMode, at: Vec2) -> Self {
        Self {
            mode,
            op: SelectionOp::Replace,
            points: vec![at],
            cursor: at,
        }
    }

    /// Make this gesture add to or subtract from the selection already there.
    ///
    /// A separate step rather than a fourth argument to [`SelectionDraft::new`]
    /// because [`SelectionOp::Replace`] is what a gesture is unless something
    /// says otherwise, and every existing caller means exactly that.
    ///
    /// **The modifier is read once, when the gesture begins.** A hand lets go
    /// of Shift halfway through a lasso and a polygon spans several clicks;
    /// reading it again at the end would let a key tapped mid-drag change what
    /// a finished gesture turns out to have meant. Snapshotting is also what
    /// every other paint application does, and it is the same rule
    /// `Editor::start_stroke` follows for the stroke's style.
    #[must_use]
    pub fn combining(mut self, op: SelectionOp) -> Self {
        self.op = op;
        self
    }

    pub fn mode(&self) -> SelectionMode {
        self.mode
    }

    pub fn op(&self) -> SelectionOp {
        self.op
    }

    /// A press, after the first. Returns true when the shape is now closed and
    /// the draft should be finished.
    ///
    /// Only the polygon has anything to do with this: the other two modes are
    /// one press, a drag and a release.
    ///
    /// `close_within` is in document pixels, and is how a click back on the
    /// first vertex closes the shape. It comes from the caller because it is a
    /// screen distance divided by the zoom — a fixed document distance would be
    /// impossible to hit at 10% and impossible to avoid at 800%.
    pub fn press(&mut self, at: Vec2, close_within: f32) -> bool {
        self.cursor = at;
        if self.mode != SelectionMode::Polygon {
            return false;
        }
        if self.points.len() >= 3
            && self
                .points
                .first()
                .is_some_and(|first| first.distance(at) <= close_within)
        {
            return true;
        }
        self.points.push(at);
        false
    }

    /// The pointer moved. For the lasso this may record a point.
    pub fn moved(&mut self, at: Vec2) {
        self.cursor = at;
        if self.mode == SelectionMode::Lasso
            && self
                .points
                .last()
                .is_none_or(|last| last.distance(at) >= LASSO_STEP)
        {
            self.points.push(at);
        }
    }

    /// A release. Returns true when the shape is complete.
    ///
    /// The polygon is the one mode a release does not finish: its gesture is a
    /// sequence of clicks, and ending it on the first button-up would make it
    /// a two-point line every time.
    pub fn release(&mut self, at: Vec2) -> bool {
        self.moved(at);
        self.mode != SelectionMode::Polygon
    }

    /// True once the draft describes something that could be selected.
    ///
    /// A polygon with two vertices is a line, and a rectangle dragged nowhere
    /// is a point; neither is a selection, and both are what a stray click
    /// produces.
    pub fn is_closable(&self) -> bool {
        match self.mode {
            SelectionMode::Rectangle => {
                let a = self.points[0];
                (a.x - self.cursor.x).abs() >= 1.0 && (a.y - self.cursor.y).abs() >= 1.0
            }
            SelectionMode::Lasso => self.points.len() >= 3,
            SelectionMode::Polygon => self.points.len() >= 3,
        }
    }

    /// Write the ring the draft currently describes into `out`, which is
    /// cleared first.
    ///
    /// Takes the caller's buffer rather than returning one because the outline
    /// is redrawn every frame of the drag, and this is the only thing in the
    /// selection path that runs per frame.
    pub fn outline_into(&self, out: &mut Vec<Vec2>) {
        out.clear();
        match self.mode {
            SelectionMode::Rectangle => {
                let a = self.points[0];
                let b = self.cursor;
                out.extend_from_slice(&[a, Vec2::new(b.x, a.y), b, Vec2::new(a.x, b.y)]);
            }
            SelectionMode::Lasso => out.extend_from_slice(&self.points),
            // The rubber band is part of what the user is looking at: without
            // it the shape appears to lag one click behind the pointer.
            SelectionMode::Polygon => {
                out.extend_from_slice(&self.points);
                out.push(self.cursor);
            }
        }
    }

    /// Turn the draft into a selection, or `None` if it encloses nothing.
    pub fn finish(&self, doc: UVec2) -> Option<Selection> {
        match self.mode {
            SelectionMode::Rectangle => Selection::rectangle(self.points[0], self.cursor, doc),
            SelectionMode::Lasso => Selection::polygon(&self.points, doc),
            SelectionMode::Polygon => Selection::polygon(&self.points, doc),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    const DOC: UVec2 = UVec2::splat(64);

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Selection {
        Selection::rectangle(vec2(x0, y0), vec2(x1, y1), DOC).expect("a rectangle")
    }

    #[test]
    fn a_whole_pixel_rectangle_is_exactly_covered() {
        // The commonest selection there is, and the one case where every
        // sub-scanline and every span end lands on an integer. Anything less
        // than 0 outside and 255 inside would mean the rasteriser's idea of
        // where a pixel is disagrees with the document's.
        let s = rect(10.0, 10.0, 20.0, 20.0);
        assert_eq!(
            s.bounds(),
            PixelRect {
                x: 10,
                y: 10,
                width: 10,
                height: 10
            }
        );
        assert_eq!(s.coverage_at(10, 10), 255);
        assert_eq!(s.coverage_at(19, 19), 255);
        assert_eq!(s.coverage_at(9, 15), 0);
        assert_eq!(s.coverage_at(20, 15), 0);
    }

    #[test]
    fn corners_given_in_any_order_select_the_same_box() {
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let b = rect(20.0, 20.0, 10.0, 10.0);
        assert_eq!(a.bounds(), b.bounds());
        assert_eq!(a.coverage(), b.coverage());
    }

    #[test]
    fn a_half_covered_pixel_is_half_selected() {
        // The edge falls down the middle of column 20, so that column is half
        // in. This is the whole reason coverage is a byte rather than a bit:
        // without it a selection edge is a staircase.
        let s = rect(10.0, 10.0, 20.5, 20.0);
        let half = s.coverage_at(20, 15);
        assert!(
            (120..=136).contains(&half),
            "expected ~128 for a half-covered pixel, got {half}"
        );
        assert_eq!(s.coverage_at(19, 15), 255);
    }

    #[test]
    fn a_triangle_ramps_across_its_diagonal() {
        // Coverage on the diagonal has to be somewhere between the two sides,
        // which only holds if the sub-scanlines and the span ends are both
        // doing their job. The hypotenuse runs x + y = 40, so pixel (19, 20)
        // straddles it and the two corners do not.
        let s = Selection::polygon(&[vec2(10.0, 10.0), vec2(30.0, 10.0), vec2(10.0, 30.0)], DOC)
            .expect("a triangle");
        assert_eq!(s.coverage_at(11, 11), 255, "well inside");
        assert_eq!(s.coverage_at(29, 29), 0, "well outside");
        let edge = s.coverage_at(19, 20);
        assert!(
            (1..=254).contains(&edge),
            "the diagonal should be partly covered, got {edge}"
        );
    }

    #[test]
    fn overlapping_rings_stay_selected_rather_than_cancelling() {
        // Nonzero winding, not even-odd. Two rings wound the same way, one
        // inside the other: nonzero fills the middle, even-odd punches a hole
        // in it. That difference is what a freehand lasso crossing its own path
        // runs into, and a hole where the artist drew a loop is not what they
        // asked for.
        let outer = vec![
            vec2(10.0, 10.0),
            vec2(40.0, 10.0),
            vec2(40.0, 40.0),
            vec2(10.0, 40.0),
        ];
        let inner = vec![
            vec2(20.0, 20.0),
            vec2(30.0, 20.0),
            vec2(30.0, 30.0),
            vec2(20.0, 30.0),
        ];
        let s = Selection::from_rings(vec![outer, inner], DOC).expect("two rings");
        assert_eq!(s.coverage_at(25, 25), 255, "the overlap must stay selected");
        assert!(s.contains(vec2(25.0, 25.0)), "and the outline agrees");
    }

    #[test]
    fn a_point_on_the_boundary_is_counted_once() {
        // The classic failure of a crossing count: a vertex exactly on the ray
        // counted by both of its edges, which reports the inside of a rectangle
        // as outside along one row.
        //
        // The rule is half-open, matching the mask: the top and left edges
        // belong to the selection and the bottom and right ones to whatever is
        // beyond it, so two selections sharing an edge do not both claim it.
        let s = rect(10.0, 10.0, 20.0, 20.0);
        assert!(s.contains(vec2(15.0, 10.0)), "the top edge is inside");
        assert!(s.contains(vec2(10.0, 15.0)), "and so is the left one");
        assert!(!s.contains(vec2(15.0, 20.0)), "the bottom edge is not");
        assert!(s.contains(vec2(15.0, 15.0)));
        assert!(!s.contains(vec2(15.0, 25.0)));
        assert!(!s.contains(vec2(5.0, 15.0)));
    }

    #[test]
    fn a_selection_off_the_canvas_is_no_selection() {
        assert!(Selection::rectangle(vec2(-40.0, -40.0), vec2(-10.0, -10.0), DOC).is_none());
        // And one thinner than a pixel encloses nothing, however long it is.
        assert!(Selection::rectangle(vec2(10.0, 10.0), vec2(10.0, 40.0), DOC).is_none());
    }

    #[test]
    fn a_selection_is_clipped_to_the_canvas() {
        // Nothing downstream may be handed a rectangle that runs off the
        // texture: `write_texture` and `read_layer_rect` both refuse one, and
        // the failure is a validation error that takes the process with it.
        let s = rect(-20.0, -20.0, 30.0, 30.0);
        let b = s.bounds();
        assert_eq!((b.x, b.y), (0, 0));
        assert_eq!((b.width, b.height), (30, 30));
        assert_eq!(s.coverage().len(), 900);
    }

    #[test]
    fn a_rectangle_draft_needs_a_drag_before_it_encloses_anything() {
        let mut draft = SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0));
        assert!(!draft.is_closable(), "a click is not a selection");
        assert!(draft.release(vec2(10.2, 10.2)));
        assert!(!draft.is_closable());
        draft.moved(vec2(30.0, 30.0));
        assert!(draft.is_closable());
        assert!(draft.finish(DOC).is_some());
    }

    #[test]
    fn a_polygon_closes_on_a_click_back_at_its_first_vertex() {
        let mut draft = SelectionDraft::new(SelectionMode::Polygon, vec2(10.0, 10.0));
        assert!(!draft.release(vec2(10.0, 10.0)), "a release is not a close");
        assert!(!draft.press(vec2(30.0, 10.0), 4.0));
        assert!(!draft.press(vec2(30.0, 30.0), 4.0));
        // Near the first vertex but not on it, which is what a real click is.
        assert!(draft.press(vec2(11.0, 12.0), 4.0));
        let s = draft.finish(DOC).expect("a triangle");
        assert_eq!(s.coverage_at(25, 15), 255);
    }

    #[test]
    fn a_polygon_does_not_close_before_it_is_a_shape() {
        // Two vertices and a click back on the start is a line, and closing on
        // it would leave the tool apparently dead: the selection would be
        // nothing and the draft would be gone.
        let mut draft = SelectionDraft::new(SelectionMode::Polygon, vec2(10.0, 10.0));
        assert!(!draft.press(vec2(30.0, 10.0), 4.0));
        assert!(!draft.press(vec2(10.5, 10.5), 4.0));
    }

    #[test]
    fn a_lasso_drops_samples_that_say_nothing() {
        // A pointer reports far faster than a document pixel changes. Every
        // recorded point is an edge the rasteriser walks on every sub-scanline
        // it spans, so the ones that repeat a position are dropped.
        let mut draft = SelectionDraft::new(SelectionMode::Lasso, vec2(10.0, 10.0));
        for _ in 0..100 {
            draft.moved(vec2(10.05, 10.05));
        }
        draft.moved(vec2(40.0, 10.0));
        draft.moved(vec2(40.0, 40.0));
        assert_eq!(draft.finish(DOC).map(|s| s.bounds().width), Some(30));
    }

    // --- combining --------------------------------------------------------

    /// How many pixels of `s` are at least half selected. The booleans are
    /// about *area*, so the tests below count it rather than sampling.
    fn covered(s: &Selection) -> usize {
        s.coverage().iter().filter(|c| **c >= 128).count()
    }

    #[test]
    fn adding_a_disjoint_shape_selects_both_and_grows_the_bounds() {
        // The point of Add: two areas with nothing between them are one
        // selection. The bounding rectangle has to grow to hold both, and the
        // mask has to be re-origined onto it — an off-by-one there puts every
        // selected pixel one across, on a mask that still looks plausible.
        let a = rect(4.0, 4.0, 10.0, 10.0);
        let b = rect(30.0, 40.0, 40.0, 50.0);
        let both = a.union(&b).expect("two areas");

        assert_eq!(
            both.bounds(),
            PixelRect {
                x: 4,
                y: 4,
                width: 36,
                height: 46
            }
        );
        assert_eq!(both.coverage().len(), 36 * 46);
        // Both shapes, in their own places, and nothing joining them.
        assert_eq!(both.coverage_at(5, 5), 255);
        assert_eq!(both.coverage_at(9, 9), 255);
        assert_eq!(both.coverage_at(31, 41), 255);
        assert_eq!(both.coverage_at(39, 49), 255);
        assert_eq!(both.coverage_at(20, 20), 0, "the gap stays out");
        assert_eq!(covered(&both), 36 + 100);
        // Two regions, so two rings — not one ring round the pair.
        assert_eq!(both.rings().len(), 2);
        assert!(both.contains(vec2(6.0, 6.0)));
        assert!(both.contains(vec2(35.0, 45.0)));
        assert!(!both.contains(vec2(20.0, 20.0)));
    }

    #[test]
    fn adding_a_touching_shape_merges_it_into_one_region() {
        // Two rectangles sharing an edge. The area is the sum, and the outline
        // is *one* ring: the seam between them is not a boundary of the union,
        // so tracing must not leave it in.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let b = rect(20.0, 10.0, 30.0, 20.0);
        let merged = a.union(&b).expect("one region");

        assert_eq!(
            merged.bounds(),
            PixelRect {
                x: 10,
                y: 10,
                width: 20,
                height: 10
            }
        );
        assert_eq!(covered(&merged), 200);
        assert_eq!(merged.coverage_at(19, 15), 255);
        assert_eq!(merged.coverage_at(20, 15), 255, "and across the join");
        assert_eq!(merged.rings().len(), 1, "one region, one outline");
        // A rectangle, so four corners once the straight runs are collapsed.
        assert_eq!(merged.rings()[0].len(), 4);
        assert!(merged.contains(vec2(25.0, 15.0)));
    }

    #[test]
    fn adding_a_shape_that_overlaps_does_not_double_the_overlap() {
        // `max`, not `a + b - ab`: adding a shape to itself is the identity.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let again = a.union(&a).expect("itself");
        assert_eq!(again.bounds(), a.bounds());
        assert_eq!(again.coverage(), a.coverage());
    }

    #[test]
    fn subtracting_half_a_rectangle_leaves_exactly_the_other_half() {
        // The plain statement of what Subtract is for. Half of a 20×20 box is
        // taken away and the remaining area, the remaining outline and the
        // trimmed bounds all have to agree that it went.
        let whole = rect(10.0, 10.0, 30.0, 30.0);
        let right_half = rect(20.0, 10.0, 30.0, 30.0);
        let left = whole.difference(&right_half).expect("the left half");

        assert_eq!(
            left.bounds(),
            PixelRect {
                x: 10,
                y: 10,
                width: 10,
                height: 20
            },
            "the bounds shrink onto what is left"
        );
        assert_eq!(left.coverage().len(), 200);
        assert_eq!(covered(&left), 200);
        assert_eq!(left.coverage_at(19, 20), 255);
        assert_eq!(left.coverage_at(20, 20), 0);
        assert!(left.contains(vec2(15.0, 20.0)));
        assert!(!left.contains(vec2(25.0, 20.0)));
    }

    #[test]
    fn subtracting_the_middle_leaves_a_hole_the_outline_knows_about() {
        // A hole is the case the winding of a traced ring decides. The inner
        // ring has to come out wound the opposite way to the outer one, or
        // nonzero winding fills the hole straight back in and `contains`
        // disagrees with the mask it was traced from.
        let whole = rect(10.0, 10.0, 40.0, 40.0);
        let middle = rect(20.0, 20.0, 30.0, 30.0);
        let ring = whole.difference(&middle).expect("a frame");

        assert_eq!(ring.bounds(), whole.bounds(), "the outside is unchanged");
        assert_eq!(covered(&ring), 900 - 100);
        assert_eq!(ring.coverage_at(25, 25), 0);
        assert_eq!(ring.coverage_at(15, 25), 255);
        assert_eq!(
            ring.rings().len(),
            2,
            "an outer ring and one round the hole"
        );
        assert!(!ring.contains(vec2(25.0, 25.0)), "the hole is not selected");
        assert!(ring.contains(vec2(15.0, 25.0)));
    }

    #[test]
    fn subtracting_everything_selects_nothing() {
        // Down to nothing is the same answer as never having had one, because
        // every caller treats an empty selection and no selection alike — and
        // a `Selection` with a zero-area mask would be a rectangle the renderer
        // must not be handed.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        assert!(a.difference(&a).is_none());
        let over = rect(0.0, 0.0, 64.0, 64.0);
        assert!(a.difference(&over).is_none());
    }

    #[test]
    fn the_empty_cases_of_a_combination_each_answer_differently() {
        let base = rect(10.0, 10.0, 20.0, 20.0);
        let shape = rect(30.0, 30.0, 40.0, 40.0);

        // A bare click deselects — but only without a modifier. A slip of the
        // hand while adding or subtracting must leave the work alone.
        assert!(Selection::combined(Some(&base), None, SelectionOp::Replace).is_none());
        let kept = Selection::combined(Some(&base), None, SelectionOp::Add).expect("kept");
        assert_eq!(kept.bounds(), base.bounds());
        let kept = Selection::combined(Some(&base), None, SelectionOp::Subtract).expect("kept");
        assert_eq!(kept.bounds(), base.bounds());

        // Adding to nothing is the shape itself, and it comes through
        // untouched — the same rings, not rings traced back out of a mask.
        let fresh = Selection::combined(None, Some(shape.clone()), SelectionOp::Add).expect("new");
        assert_eq!(fresh.rings(), shape.rings());

        // Subtracting from nothing is nothing: no selection means the whole
        // document, and taking a shape out of it would claim more than the
        // gesture did.
        assert!(Selection::combined(None, Some(shape.clone()), SelectionOp::Subtract).is_none());

        // Replace never looks at the base, and never re-traces.
        let replaced =
            Selection::combined(Some(&base), Some(shape.clone()), SelectionOp::Replace).unwrap();
        assert_eq!(replaced.rings(), shape.rings());
    }

    #[test]
    fn a_gesture_carries_the_operation_it_began_with() {
        // Snapshotted at the start: a modifier let go of halfway through a
        // lasso, or tapped between two clicks of a polygon, must not change
        // what the gesture turns out to have meant.
        let draft = SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0))
            .combining(SelectionOp::Subtract);
        assert_eq!(draft.op(), SelectionOp::Subtract);
        assert_eq!(
            SelectionDraft::new(SelectionMode::Lasso, vec2(0.0, 0.0)).op(),
            SelectionOp::Replace,
            "an unmodified gesture replaces"
        );
    }

    #[test]
    fn two_areas_touching_only_at_a_corner_keep_their_own_outlines() {
        // The one ambiguous case in the trace: two selected pixels meeting
        // diagonally across two unselected ones. The walk turns as sharply as
        // it can, so they stay two regions — pinching them into one ring would
        // put an outline through pixels that are not selected.
        let a = rect(10.0, 10.0, 20.0, 20.0);
        let b = rect(20.0, 20.0, 30.0, 30.0);
        let both = a.union(&b).expect("two squares corner to corner");
        assert_eq!(covered(&both), 200);
        assert_eq!(both.rings().len(), 2);
        assert!(
            !both.contains(vec2(25.0, 15.0)),
            "the empty quarters stay out"
        );
        assert!(!both.contains(vec2(15.0, 25.0)));
    }

    #[test]
    fn an_outline_is_written_into_the_callers_buffer() {
        // The one thing here that runs per frame. It must not allocate, which
        // is why it takes a buffer rather than returning one — and the buffer
        // has to be cleared, or the outline grows a tail of last frame's.
        let mut buf = vec![Vec2::ZERO; 9];
        let mut draft = SelectionDraft::new(SelectionMode::Rectangle, vec2(10.0, 10.0));
        draft.moved(vec2(30.0, 20.0));
        draft.outline_into(&mut buf);
        assert_eq!(
            buf,
            vec![
                vec2(10.0, 10.0),
                vec2(30.0, 10.0),
                vec2(30.0, 20.0),
                vec2(10.0, 20.0)
            ]
        );

        let mut poly = SelectionDraft::new(SelectionMode::Polygon, vec2(0.0, 0.0));
        poly.press(vec2(10.0, 0.0), 4.0);
        poly.moved(vec2(10.0, 10.0));
        poly.outline_into(&mut buf);
        assert_eq!(buf.len(), 3, "the rubber band is part of the outline");
    }
}
