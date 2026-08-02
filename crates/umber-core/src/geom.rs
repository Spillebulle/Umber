//! Small geometry helpers shared by the engine and renderer.

use glam::Vec2;

/// How far a screen-space drag of `delta` travelled *towards "more"*, in
/// pixels, where right and up are more and left and down are less.
///
/// Two of Umber's gestures ask the same question of a drag — the zoom tool's
/// and the Alt-held brush resize — and neither is naturally one-dimensional:
/// the hand goes where it goes. Screen y is down-positive, which is why the two
/// axes are *subtracted*, so the "more" direction is the diagonal `(1, -1)` and
/// a drag has to be resolved onto it somehow.
///
/// Adding the axes outright is the obvious way and is wrong: a 45° drag of 100
/// pixels each way is 141 pixels of hand movement and would be worth 200, so
/// the same gesture asks for half again as much for being made diagonally.
/// Projecting onto the diagonal is the other obvious way and only moves the
/// problem — it is the same expression times a constant, so a diagonal still
/// outruns an axis, and it slows the horizontal drag both these gestures have
/// always been by 30%.
///
/// So: **the distance the hand travelled, weighted by how far the drag leans
/// towards "more" rather than "less"**. `(dx - dy) / (|dx| + |dy|)` is that
/// lean, running from +1 for a drag purely towards more to -1 for one purely
/// towards less and passing through 0 on the neutral diagonal, where a drag
/// along `(1, 1)` asks for nothing. Every drag is then worth at most its own
/// length, a pure right drag is worth exactly what it was worth before either
/// gesture took the vertical axis in, and a diagonal is worth its own 141
/// pixels rather than 200.
///
/// What a pixel of it *buys* is the caller's, and the two callers differ by
/// three orders of magnitude — a zoom doubles in 90 and a brush in 100, but one
/// is spent on a rate raised to a power and the other on a doubling. Sharing
/// the shape is what stops the reasoning above being written twice; sharing a
/// rate would make one gesture's feel hostage to the other's.
pub fn drag_towards_more(delta: Vec2) -> f32 {
    let lean = delta.x.abs() + delta.y.abs();
    // A drag that did not move has no direction to lean in, and the division
    // below would be 0/0.
    if lean <= f32::EPSILON {
        return 0.0;
    }
    delta.length() * (delta.x - delta.y) / lean
}

/// Which way a mirror faces.
///
/// Named for the direction the picture *moves*, which is how every application
/// with the command words it: a horizontal flip swaps left and right.
///
/// A flip is its own inverse on both axes, and that is the property the whole
/// of the undo design rests on — see [`crate::history::EditKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlipAxis {
    /// Left to right, about the canvas's vertical centre line.
    Horizontal,
    /// Top to bottom, about its horizontal centre line.
    Vertical,
}

impl FlipAxis {
    /// Mirror a point of a `size`-wide document.
    ///
    /// The mirror of the *continuous* canvas, not of a texel index: the span
    /// `0 ..= w` maps onto itself, so the pixel covering `x .. x + 1` lands on
    /// the one covering `w - x - 1 .. w - x`. That is the same permutation the
    /// flip pass performs with integers, stated in the space the selection's
    /// rings live in.
    pub fn mirror(self, point: Vec2, size: Vec2) -> Vec2 {
        match self {
            Self::Horizontal => Vec2::new(size.x - point.x, point.y),
            Self::Vertical => Vec2::new(point.x, size.y - point.y),
        }
    }
}

/// An axis-aligned rectangle in document space (float pixels).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub min: Vec2,
    pub max: Vec2,
}

impl Rect {
    pub fn new(min: Vec2, max: Vec2) -> Self {
        Self { min, max }
    }

    /// An empty rect that absorbs the first thing unioned into it.
    pub fn empty() -> Self {
        Self {
            min: Vec2::splat(f32::INFINITY),
            max: Vec2::splat(f32::NEG_INFINITY),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.min.x > self.max.x || self.min.y > self.max.y
    }

    /// Grow to include a circle — the shape a round dab actually covers.
    pub fn union_circle(&mut self, center: Vec2, radius: f32) {
        self.min = self.min.min(center - radius);
        self.max = self.max.max(center + radius);
    }

    /// Grow to include an axis-aligned box, given as a centre and half-extents.
    ///
    /// What a *dab* needs, as opposed to a circle: a dab is a quad, and a
    /// bitmap tip paints right into its corners.
    pub fn union_box(&mut self, center: Vec2, half: Vec2) {
        self.min = self.min.min(center - half);
        self.max = self.max.max(center + half);
    }

    pub fn union(&mut self, other: &Rect) {
        if other.is_empty() {
            return;
        }
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    /// The part of this rectangle that is also inside `other`.
    ///
    /// `None` when they do not meet at all. A rectangle that only *touches*
    /// another along an edge is still an answer — it has a position, which is
    /// all [`crate::overlay`] asks of it — where two that miss each other have
    /// nothing to say.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let min = self.min.max(other.min);
        let max = self.max.min(other.max);
        (min.x <= max.x && min.y <= max.y).then_some(Rect { min, max })
    }

    pub fn center(&self) -> Vec2 {
        (self.min + self.max) * 0.5
    }

    pub fn size(&self) -> Vec2 {
        self.max - self.min
    }

    /// Snap outwards to whole pixels and clamp to a document of `size`.
    ///
    /// Returns `None` when the result would be zero-area, which lets callers
    /// skip work entirely for strokes that fell outside the canvas.
    pub fn to_pixels_clamped(&self, size: glam::UVec2) -> Option<PixelRect> {
        if self.is_empty() {
            return None;
        }
        let min_x = self.min.x.floor().max(0.0) as u32;
        let min_y = self.min.y.floor().max(0.0) as u32;
        let max_x = (self.max.x.ceil().max(0.0) as u32).min(size.x);
        let max_y = (self.max.y.ceil().max(0.0) as u32).min(size.y);
        if max_x <= min_x || max_y <= min_y {
            return None;
        }
        Some(PixelRect {
            x: min_x,
            y: min_y,
            width: max_x - min_x,
            height: max_y - min_y,
        })
    }
}

/// An integer rectangle in texture space, used for damage tracking and undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub fn area(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::{UVec2, vec2};

    #[test]
    fn empty_rect_absorbs_first_circle() {
        let mut r = Rect::empty();
        assert!(r.is_empty());
        r.union_circle(vec2(10.0, 10.0), 2.0);
        assert_eq!(r.min, vec2(8.0, 8.0));
        assert_eq!(r.max, vec2(12.0, 12.0));
    }

    #[test]
    fn pixel_clamp_trims_to_document() {
        let mut r = Rect::empty();
        r.union_circle(vec2(1.0, 1.0), 8.0);
        let p = r.to_pixels_clamped(UVec2::new(64, 64)).unwrap();
        assert_eq!(p.x, 0);
        assert_eq!(p.y, 0);
        assert_eq!(p.width, 9);
    }

    #[test]
    fn fully_offscreen_rect_yields_nothing() {
        let mut r = Rect::empty();
        r.union_circle(vec2(-100.0, -100.0), 4.0);
        assert!(r.to_pixels_clamped(UVec2::new(64, 64)).is_none());
    }

    /// The shape both drag gestures are built on, in its own units — so a
    /// change to it is caught here rather than as two puzzling failures in
    /// `Camera` and `Brush`.
    #[test]
    fn a_drag_leans_towards_more_and_is_worth_at_most_its_own_length() {
        use super::drag_towards_more as more;

        // Right and up are more, left and down are less, and a drag that did
        // not move asks for nothing.
        assert!(more(vec2(20.0, 0.0)) > 0.0);
        assert!(more(vec2(0.0, -20.0)) > 0.0, "screen y is down-positive");
        assert!(more(vec2(-20.0, 0.0)) < 0.0);
        assert!(more(vec2(0.0, 20.0)) < 0.0);
        assert_eq!(more(Vec2::ZERO), 0.0);

        // Neither axis is the "real" one with the other bolted on.
        assert!((more(vec2(17.0, 0.0)) - more(vec2(0.0, -17.0))).abs() < 1e-4);

        // The neutral diagonal is exactly between the two answers, so it must
        // not pick one.
        assert_eq!(more(vec2(30.0, 30.0)), 0.0);
        assert_eq!(more(vec2(-30.0, -30.0)), 0.0);

        // A pure axis drag is worth exactly its length — which is what it was
        // worth before either gesture read the second axis — and no direction
        // is worth more. That is the whole reason the axes are weighted rather
        // than added.
        assert!((more(vec2(50.0, 0.0)) - 50.0).abs() < 1e-3);
        for step in 0..64 {
            let angle = step as f32 * std::f32::consts::TAU / 64.0;
            let along = more(vec2(angle.cos(), angle.sin()) * 50.0);
            assert!(along.abs() <= 50.0 + 1e-3, "{angle} gave {along}");
        }
    }
}
