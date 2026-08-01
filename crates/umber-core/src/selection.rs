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
//! # What is not here
//!
//! No boolean operations (add to / subtract from a selection), no feather, no
//! "select by colour". Each is a real feature and none is drawn in the
//! interface, which is the rule this file lives by as much as any other.

use crate::geom::{PixelRect, Rect};
use glam::{UVec2, Vec2};

/// Sub-scanlines per pixel row. See the module docs.
const SUB_SCANLINES: u32 = 4;

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

/// A selection being drawn: the gesture, before it becomes a [`Selection`].
///
/// Lives here rather than in the interface because what each mode does with a
/// press, a move and a release is a rule, not a drawing — and a rule is
/// testable without a window. `panels.rs` and `dock.rs` keep the same division.
#[derive(Clone, Debug)]
pub struct SelectionDraft {
    mode: SelectionMode,
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
            points: vec![at],
            cursor: at,
        }
    }

    pub fn mode(&self) -> SelectionMode {
        self.mode
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
