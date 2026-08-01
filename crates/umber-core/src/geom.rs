//! Small geometry helpers shared by the engine and renderer.

use glam::Vec2;

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
}
