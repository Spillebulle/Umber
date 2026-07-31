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

/// Ceiling on how many timed dabs one input sample may owe.
///
/// A stall — a slow frame, a breakpoint, a laptop lid — makes `elapsed` huge,
/// and an airbrush at 80 dabs a second would answer with tens of thousands of
/// stamps and a visible hang. Paint stops accumulating during a freeze, which
/// is the lesser wrong: nobody can tell how much airbrush landed while the
/// application was not responding, but everybody notices the hang.
const MAX_TIMED_DABS_PER_SAMPLE: u32 = 64;

/// One stamp, laid out for direct upload as GPU instance data.
///
/// Exactly 32 bytes, which is both a power-of-two stride — instance fetches
/// never straddle a cache line — and, as it happens, precisely what the fields
/// need once the colour is included. Per-dab colour therefore costs no instance
/// bandwidth at all; it went into what used to be padding.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Dab {
    /// Centre in document pixels.
    pub pos: [f32; 2],
    pub radius: f32,
    pub hardness: f32,
    /// Per-dab coverage, *excluding* stroke opacity.
    pub coverage: f32,
    /// Linear RGB actually deposited by this dab.
    ///
    /// Equal to the stroke colour for every ordinary brush, in which case the
    /// dab pass never reads it and the composite takes the colour from its
    /// uniform instead. It differs only along a smudging stroke, which is the
    /// entire reason it exists.
    pub color: [f32; 3],
}

impl Dab {
    fn new(pos: Vec2, radius: f32, hardness: f32, coverage: f32, color: [f32; 3]) -> Self {
        Self {
            pos: [pos.x, pos.y],
            radius,
            hardness,
            coverage,
            color,
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
    /// Fraction of a timed dab's interval already elapsed. Carried across
    /// samples for the same reason `residual` is: resetting it per sample would
    /// make the deposit rate depend on how often the pointer reports.
    time_residual: f32,
    pending: Vec<Dab>,
    bounds: Rect,
    /// The palette colour this stroke started with, in linear RGB.
    paint: [f32; 3],
    /// Colour carried along by a smudging brush: what it has picked up, decayed
    /// towards each new canvas sample by `smudge_length`.
    ///
    /// `None` until the first sample arrives. A smudge brush that has picked
    /// nothing up yet paints its palette colour rather than black — starting it
    /// at zero would put a dark head on every smudged stroke.
    smudge: Option<[f32; 3]>,
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
            time_residual: 0.0,
            pending: Vec::with_capacity(1024),
            bounds: Rect::empty(),
            paint: [0.0; 3],
            smudge: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Whether this stroke needs the per-dab colour path.
    ///
    /// The renderer asks once per frame to decide which dab pipeline to use, so
    /// an ordinary stroke never allocates or writes the colour scratch.
    pub fn is_coloured(&self) -> bool {
        self.brush.smudges()
    }

    /// Where the next canvas sample should be taken, and how wide.
    ///
    /// `None` when the brush does not smudge, which is the common case and the
    /// signal to the renderer not to run a probe at all.
    pub fn probe(&self) -> Option<(Vec2, f32)> {
        if !self.active || !self.brush.smudges() {
            return None;
        }
        let radius = self.brush.radius_at(self.last.map_or(1.0, |p| p.pressure));
        Some((self.smoothed, (radius * self.brush.smudge_radius).max(0.5)))
    }

    /// Feed back what the canvas holds under the brush.
    ///
    /// Arrives a frame or two late by construction — the read is asynchronous,
    /// because a blocking one belongs nowhere near the drawing loop. That lag is
    /// not a defect here: a smudge is a trailing average of what the brush has
    /// passed over, and `smudge_length` already delays it far more than the
    /// readback does.
    ///
    /// `sample` is linear RGBA with **straight** alpha — the form the composite
    /// pass produces on its export path, which is what the probe reuses.
    ///
    /// Alpha is how much paint was actually found there, and it scales how far
    /// the carried colour moves: bare canvas contributes nothing rather than
    /// contributing black, and thin paint pulls less than solid. Without that,
    /// smudging off the edge of a painting would drag a dark smear back onto it.
    pub fn absorb(&mut self, sample: [f32; 4]) {
        if !self.brush.smudges() {
            return;
        }
        let alpha = sample[3].clamp(0.0, 1.0);
        if alpha <= 0.0 {
            return;
        }
        let found = [sample[0], sample[1], sample[2]];
        let keep = self.brush.smudge_length.clamp(0.0, 0.99);
        let take = (1.0 - keep) * alpha;
        self.smudge = Some(match self.smudge {
            None => found,
            Some(held) => [
                held[0] + (found[0] - held[0]) * take,
                held[1] + (found[1] - held[1]) * take,
                held[2] + (found[2] - held[2]) * take,
            ],
        });
    }

    /// Bounding box of everything drawn in the current stroke.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Start a stroke, laying down one dab immediately so a tap makes a mark.
    ///
    /// `paint` is the palette colour in linear RGB. It is snapshotted here for
    /// the same reason the brush is: changing the colour mid-stroke must not
    /// alter the half already painted.
    pub fn begin(&mut self, brush: Brush, paint: [f32; 3], point: InputPoint) {
        self.brush = brush;
        self.paint = paint;
        self.smudge = None;
        self.active = true;
        self.smoothed = point.pos;
        self.residual = 0.0;
        self.time_residual = 0.0;
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

        // A timed brush deposits paint for as long as the pen is down, whether
        // or not it has moved, so its dabs are counted before the early return
        // that a stationary pen would otherwise take.
        let elapsed = (point.time - last.time).max(0.0) as f32;
        if self.brush.is_timed() && elapsed > 0.0 {
            self.emit_timed(elapsed, point.pressure, from, seg);
        }

        if !len.is_finite() || len < 1e-6 {
            // The position is unchanged, but the clock is not: record the new
            // sample so the next interval measures from here rather than
            // depositing the same milliseconds twice.
            self.last = Some(InputPoint { pos: to, ..point });
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

    /// Drop dabs that have not been handed to the renderer.
    ///
    /// Only for abandoning a stroke. On a normal finish the pending dabs must
    /// be *flushed*, not dropped — they are the tail of the stroke.
    pub fn clear_pending(&mut self) {
        self.pending.clear();
    }

    /// Lay down the dabs a stationary or slow-moving timed brush owes.
    ///
    /// Spread along whatever travel there was, so an airbrush dragged slowly
    /// deposits along the path rather than piling every dab on the last sample.
    fn emit_timed(&mut self, elapsed: f32, pressure: f32, from: Vec2, seg: Vec2) {
        let owed = elapsed * self.brush.dabs_per_second + self.time_residual;
        // Bounded so that a frame lost to a stall — or a debugger pause — cannot
        // turn into thousands of dabs and a hang while they are drawn.
        let count = (owed as u32).min(MAX_TIMED_DABS_PER_SAMPLE);
        self.time_residual = (owed - count as f32).clamp(0.0, 1.0);
        for i in 0..count {
            let f = (i + 1) as f32 / count as f32;
            self.emit(from + seg * f, pressure);
        }
    }

    fn emit(&mut self, pos: Vec2, pressure: f32) {
        let radius = self.brush.radius_at(pressure);
        let coverage = self.brush.coverage_at(pressure);
        self.pending.push(Dab::new(
            pos,
            radius,
            self.brush.hardness,
            coverage,
            self.dab_color(),
        ));
        self.bounds.union_circle(pos, radius);
    }

    /// The colour this dab deposits: the palette colour, pulled towards
    /// whatever the brush has picked up.
    fn dab_color(&self) -> [f32; 3] {
        let (Some(held), true) = (self.smudge, self.brush.smudges()) else {
            return self.paint;
        };
        let t = self.brush.smudge.clamp(0.0, 1.0);
        [
            self.paint[0] + (held[0] - self.paint[0]) * t,
            self.paint[1] + (held[1] - self.paint[1]) * t,
            self.paint[2] + (held[2] - self.paint[2]) * t,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Brush;
    use glam::vec2;

    const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

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
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        assert_eq!(s.pending_len(), 1);
    }

    #[test]
    fn dabs_are_evenly_spaced_along_a_straight_line() {
        // 20px brush at 0.1 spacing => a dab every 2 document px.
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
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
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
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
            WHITE,
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
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        let before = s.pending_len();
        s.extend(InputPoint::new(Vec2::ZERO, 1.0, 0.01));
        assert_eq!(s.pending_len(), before);
    }

    #[test]
    fn ending_a_stroke_keeps_its_tail_pending() {
        // The renderer drains pending dabs once per frame, but pointer events
        // arrive far more often than frames. Whatever is still pending when the
        // stroke ends is its tail, and it must survive `end()` so the caller
        // can flush it into the scratch texture before committing.
        //
        // Dropping it here was a real bug: the tail stayed as stale coverage in
        // the scratch, reappeared as a live preview (the stroke appeared to
        // hang), and was then baked in by the *next* stroke's commit, wearing
        // that stroke's colour.
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(40.0, 0.0), 1.0, 0.05));
        let tail = s.pending_len();
        assert!(tail > 1, "expected a tail to have accumulated");

        s.end();
        assert_eq!(
            s.pending_len(),
            tail,
            "end() must not discard the tail of the stroke"
        );
    }

    #[test]
    fn clear_pending_discards_the_tail() {
        // The abandon path — a gesture that turned out to be a pinch, not a
        // stroke — is the one case where dropping is correct.
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(40.0, 0.0), 1.0, 0.05));
        s.end();
        s.clear_pending();
        assert_eq!(s.pending_len(), 0);
    }

    #[test]
    fn extend_without_begin_is_ignored() {
        let mut s = StrokeBuilder::new();
        s.extend(InputPoint::new(vec2(10.0, 10.0), 1.0, 0.0));
        assert_eq!(s.pending_len(), 0);
    }

    fn smudger(smudge: f32, length: f32) -> Brush {
        Brush {
            size: 20.0,
            spacing: 0.1,
            stabilization: 0.0,
            pressure_size: false,
            smudge,
            smudge_length: length,
            ..Default::default()
        }
    }

    #[test]
    fn an_ordinary_brush_paints_its_palette_colour_and_nothing_else() {
        // The fast path. Every dab carries the stroke colour, so the renderer
        // can ignore the colour scratch entirely — which is what keeps the
        // common case at one target and one blend.
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(40.0, 0.0), 1.0, 0.05));
        assert!(!s.is_coloured());
        assert!(s.probe().is_none(), "no smudge, so nothing to sample");
        for dab in s.drain_pending() {
            assert_eq!(dab.color, WHITE);
        }
    }

    #[test]
    fn a_full_smudge_deposits_what_it_picked_up() {
        let mut s = StrokeBuilder::new();
        // smudge 1.0 = a pure blender: the palette colour never appears.
        // smudge_length 0.0 = it takes each sample whole, which makes the
        // arithmetic here exact rather than a decayed average.
        s.begin(
            smudger(1.0, 0.0),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        assert!(s.is_coloured());
        s.drain_pending();

        // Opaque red under the brush.
        s.absorb([1.0, 0.0, 0.0, 1.0]);
        s.extend(InputPoint::new(vec2(20.0, 0.0), 1.0, 0.05));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert!(!dabs.is_empty());
        for dab in dabs {
            assert_eq!(dab.color, [1.0, 0.0, 0.0], "should paint what it found");
        }
    }

    #[test]
    fn a_smudge_before_its_first_sample_paints_the_palette_not_black() {
        // The lag is real: the first dabs of a stroke land before any readback
        // has come home. Treating "nothing picked up yet" as black would put a
        // dark head on every smudged stroke.
        let mut s = StrokeBuilder::new();
        s.begin(
            smudger(1.0, 0.0),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        let first: Vec<Dab> = s.drain_pending().collect();
        assert_eq!(first[0].color, WHITE);
    }

    #[test]
    fn transparent_canvas_is_not_picked_up_as_black() {
        // Premultiplied RGBA: fully transparent reads as all zeroes, which
        // un-premultiplied would be a division by zero and looks like black.
        // Smudging off the edge of a painting must not drag a dark smear back.
        let mut s = StrokeBuilder::new();
        s.begin(
            smudger(1.0, 0.0),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.drain_pending();
        s.absorb([0.0, 0.0, 0.0, 0.0]);
        s.extend(InputPoint::new(vec2(20.0, 0.0), 1.0, 0.05));
        for dab in s.drain_pending() {
            assert_eq!(dab.color, WHITE, "nothing there, so nothing picked up");
        }
    }

    #[test]
    fn thin_paint_is_picked_up_less_than_solid_paint() {
        // Straight alpha, so the colour found is red whatever the coverage —
        // but half-covered canvas should pull the carried colour only half as
        // far. Ignoring alpha would make a brush crossing a soft edge grab as
        // much colour as one crossing a solid mark.
        let mut s = StrokeBuilder::new();
        s.begin(
            smudger(1.0, 0.0),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.drain_pending();
        s.absorb([0.0, 0.0, 0.0, 1.0]); // solid black: the first sample is whole
        s.absorb([1.0, 1.0, 1.0, 0.5]); // half-covered white: moves half way
        s.extend(InputPoint::new(vec2(20.0, 0.0), 1.0, 0.05));
        let dab = s.drain_pending().next().expect("a dab");
        assert!((dab.color[0] - 0.5).abs() < 1e-5, "{:?}", dab.color);
    }

    #[test]
    fn smudge_length_decays_towards_each_new_sample() {
        let mut s = StrokeBuilder::new();
        s.begin(
            smudger(1.0, 0.5),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.drain_pending();
        s.absorb([0.0, 0.0, 0.0, 1.0]); // first sample is taken whole: black
        s.absorb([1.0, 1.0, 1.0, 1.0]); // then half-way back towards white
        s.extend(InputPoint::new(vec2(20.0, 0.0), 1.0, 0.05));
        let dab = s.drain_pending().next().expect("a dab");
        assert!(
            (dab.color[0] - 0.5).abs() < 1e-5,
            "expected a half-decayed smear, got {:?}",
            dab.color
        );
    }

    #[test]
    fn a_timed_brush_keeps_painting_while_the_pen_is_still() {
        // The airbrush case, and the reason two MyPaint brushes could not be
        // imported at all: with no distance term they emitted one dab and then
        // nothing, which reads as a solid line rather than a spray.
        let brush = Brush {
            size: 20.0,
            spacing: 0.1,
            stabilization: 0.0,
            pressure_size: false,
            dabs_per_second: 40.0,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();

        // Quarter of a second without moving: 40/s should owe about ten dabs.
        s.extend(InputPoint::new(Vec2::ZERO, 1.0, 0.25));
        let n = s.pending_len();
        assert!((9..=11).contains(&n), "expected ~10 timed dabs, got {n}");
    }

    #[test]
    fn a_stall_cannot_turn_into_a_flood_of_timed_dabs() {
        // A minute of wall clock between two samples — a breakpoint, a lid —
        // must not become 4800 stamps and a hang.
        let brush = Brush {
            size: 20.0,
            dabs_per_second: 80.0,
            stabilization: 0.0,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();
        s.extend(InputPoint::new(Vec2::ZERO, 1.0, 60.0));
        assert!(
            s.pending_len() as u32 <= MAX_TIMED_DABS_PER_SAMPLE,
            "a stall produced {} dabs",
            s.pending_len()
        );
    }

    #[test]
    fn a_stationary_timed_brush_does_not_bank_the_same_seconds_twice() {
        // The early return for zero-length motion used to skip recording the
        // sample, so every stationary report measured its interval from the
        // last *move* and deposited the whole wait again.
        let brush = Brush {
            size: 20.0,
            dabs_per_second: 40.0,
            stabilization: 0.0,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();
        for i in 1..=10 {
            s.extend(InputPoint::new(Vec2::ZERO, 1.0, i as f64 * 0.025));
        }
        // A quarter second in ten reports is still a quarter second of paint.
        let n = s.pending_len();
        assert!((9..=11).contains(&n), "expected ~10 timed dabs, got {n}");
    }

    #[test]
    fn tiny_brush_at_zero_spacing_terminates() {
        // Guards the dab loop against an infinite walk.
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(1.0, 0.0),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(50.0, 0.0), 1.0, 0.1));
        assert!(s.pending_len() > 0);
    }
}
