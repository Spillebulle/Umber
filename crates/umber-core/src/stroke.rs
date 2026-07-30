//! Turning pointer motion into dabs.
//!
//! # Why dabs, and why a scratch layer
//!
//! A stroke is drawn as a dense row of overlapping stamps ("dabs"). If those
//! were composited straight onto the layer with normal alpha blending, every
//! overlap would darken — a semi-transparent stroke would come out blotchy and
//! far more opaque than requested, and it would darken again wherever the
//! stroke crossed itself.
//!
//! Instead dabs accumulate into a scratch (wet) layer with a `max` blend, so
//! coverage saturates at 1.0 no matter how many dabs land on a pixel. The
//! stroke's opacity is applied exactly once, when the scratch layer is
//! committed onto the real layer at pointer-up. This is why [`Brush::opacity`]
//! is deliberately *not* folded into [`Dab::coverage`].

use crate::brush::Brush;
use crate::geom::Rect;
use crate::input::InputPoint;
use bytemuck::{Pod, Zeroable};
use glam::Vec2;

/// One stamp, laid out for direct upload as GPU instance data.
///
/// Padded to 32 bytes: the fields total 20, and a power-of-two stride keeps
/// instance fetches from straddling cache lines.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Dab {
    /// Centre in document pixels.
    pub pos: [f32; 2],
    pub radius: f32,
    pub hardness: f32,
    /// Per-dab coverage, *excluding* stroke opacity.
    pub coverage: f32,
    pub _pad: [f32; 3],
}

impl Dab {
    fn new(pos: Vec2, radius: f32, hardness: f32, coverage: f32) -> Self {
        Self {
            pos: [pos.x, pos.y],
            radius,
            hardness,
            coverage,
            _pad: [0.0; 3],
        }
    }
}

/// Accumulates input samples and emits evenly spaced dabs.
///
/// Dabs are emitted into an internal buffer that the renderer drains each
/// frame, so a slow frame produces one large batch rather than dropping input.
#[derive(Debug)]
pub struct StrokeBuilder {
    brush: Brush,
    active: bool,
    /// Smoothed cursor position, the actual source of dab centres.
    smoothed: Vec2,
    /// Previous emitted-from sample.
    last: Option<InputPoint>,
    /// Distance still to travel before the next dab lands.
    residual: f32,
    pending: Vec<Dab>,
    bounds: Rect,
}

// Written out rather than derived: a derived `Default` would zero `bounds`,
// producing a valid-looking rect at the origin instead of the empty sentinel,
// and every first stroke would damage a region stretching back to (0, 0).
impl Default for StrokeBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl StrokeBuilder {
    pub fn new() -> Self {
        Self {
            brush: Brush::default(),
            active: false,
            smoothed: Vec2::ZERO,
            last: None,
            residual: 0.0,
            pending: Vec::with_capacity(1024),
            bounds: Rect::empty(),
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Bounding box of everything drawn in the current stroke.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Start a stroke, laying down one dab immediately so a tap makes a mark.
    pub fn begin(&mut self, brush: Brush, point: InputPoint) {
        self.brush = brush;
        self.active = true;
        self.smoothed = point.pos;
        self.residual = 0.0;
        self.bounds = Rect::empty();
        self.pending.clear();

        self.emit(point.pos, point.pressure);
        self.last = Some(InputPoint {
            pos: point.pos,
            ..point
        });
        // A tap has already consumed one dab's worth of travel.
        self.residual = self.brush.step_at(point.pressure);
    }

    /// Feed a new sample, emitting however many dabs the travel calls for.
    pub fn extend(&mut self, point: InputPoint) {
        if !self.active {
            return;
        }

        // Exponential smoothing. stabilization 0.0 leaves input untouched.
        let alpha = (1.0 - self.brush.stabilization).clamp(0.02, 1.0);
        self.smoothed += (point.pos - self.smoothed) * alpha;

        let Some(last) = self.last else {
            self.last = Some(point);
            return;
        };

        let from = last.pos;
        let to = self.smoothed;
        let seg = to - from;
        let len = seg.length();
        if !len.is_finite() || len < 1e-6 {
            return;
        }
        let dir = seg / len;

        // Walk the segment, dropping a dab every `step` document pixels. `step`
        // is recomputed per dab because pressure (and therefore size) varies
        // along the segment.
        let mut t = self.residual;
        while t <= len {
            let f = t / len;
            let pressure = last.pressure + (point.pressure - last.pressure) * f;
            self.emit(from + dir * t, pressure);
            t += self.brush.step_at(pressure);
        }
        self.residual = t - len;

        self.last = Some(InputPoint { pos: to, ..point });
    }

    /// Finish the stroke. The caller commits and clears the scratch layer.
    pub fn end(&mut self) {
        self.active = false;
        self.last = None;
        self.residual = 0.0;
    }

    /// Hand the accumulated dabs to the renderer, leaving the buffer empty but
    /// with its capacity intact.
    pub fn drain_pending(&mut self) -> std::vec::Drain<'_, Dab> {
        self.pending.drain(..)
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    fn emit(&mut self, pos: Vec2, pressure: f32) {
        let radius = self.brush.radius_at(pressure);
        let coverage = self.brush.coverage_at(pressure);
        self.pending
            .push(Dab::new(pos, radius, self.brush.hardness, coverage));
        self.bounds.union_circle(pos, radius);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Brush;
    use glam::vec2;

    fn unsmoothed(size: f32, spacing: f32) -> Brush {
        Brush {
            size,
            spacing,
            stabilization: 0.0,
            pressure_size: false,
            ..Default::default()
        }
    }

    #[test]
    fn a_tap_lays_down_exactly_one_dab() {
        let mut s = StrokeBuilder::new();
        s.begin(unsmoothed(20.0, 0.1), InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        assert_eq!(s.pending_len(), 1);
    }

    #[test]
    fn dabs_are_evenly_spaced_along_a_straight_line() {
        // 20px brush at 0.1 spacing => a dab every 2 document px.
        let mut s = StrokeBuilder::new();
        s.begin(unsmoothed(20.0, 0.1), InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.extend(InputPoint::new(vec2(100.0, 0.0), 1.0, 0.1));

        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert_eq!(dabs.len(), 51, "1 initial + 50 along the segment");
        for pair in dabs.windows(2) {
            let gap = pair[1].pos[0] - pair[0].pos[0];
            assert!((gap - 2.0).abs() < 1e-3, "uneven gap {gap}");
        }
    }

    #[test]
    fn spacing_is_continuous_across_samples() {
        // The residual must carry over, otherwise every input sample restarts
        // the spacing walk and dabs clump at sample boundaries.
        let mut s = StrokeBuilder::new();
        s.begin(unsmoothed(20.0, 0.1), InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        for i in 1..=10 {
            // 3px steps do not divide evenly into the 2px dab spacing.
            s.extend(InputPoint::new(
                vec2(i as f32 * 3.0, 0.0),
                1.0,
                i as f64 * 0.01,
            ));
        }
        let dabs: Vec<Dab> = s.drain_pending().collect();
        for pair in dabs.windows(2) {
            let gap = pair[1].pos[0] - pair[0].pos[0];
            assert!(
                (gap - 2.0).abs() < 1e-3,
                "clumping at sample boundary: {gap}"
            );
        }
    }

    #[test]
    fn bounds_cover_dab_radius_not_just_centres() {
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            InputPoint::new(vec2(50.0, 50.0), 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(60.0, 50.0), 1.0, 0.1));
        let b = s.bounds();
        assert!(
            b.min.x <= 40.0,
            "left edge {} should include radius",
            b.min.x
        );
        assert!(
            b.max.x >= 70.0,
            "right edge {} should include radius",
            b.max.x
        );
    }

    #[test]
    fn zero_length_motion_emits_nothing_extra() {
        let mut s = StrokeBuilder::new();
        s.begin(unsmoothed(20.0, 0.1), InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        let before = s.pending_len();
        s.extend(InputPoint::new(Vec2::ZERO, 1.0, 0.01));
        assert_eq!(s.pending_len(), before);
    }

    #[test]
    fn extend_without_begin_is_ignored() {
        let mut s = StrokeBuilder::new();
        s.extend(InputPoint::new(vec2(10.0, 10.0), 1.0, 0.0));
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn tiny_brush_at_zero_spacing_terminates() {
        // Guards the dab loop against an infinite walk.
        let mut s = StrokeBuilder::new();
        s.begin(unsmoothed(1.0, 0.0), InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.extend(InputPoint::new(vec2(50.0, 0.0), 1.0, 0.1));
        assert!(s.pending_len() > 0);
    }
}
