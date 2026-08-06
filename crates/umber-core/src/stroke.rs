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
use crate::color::Color;
use crate::damage::TileMask;
use crate::dynamics::{
    DabInput, DabInputs, Modulated, SLOW_SPEED_SLOWNESS, SPEED_GAMMA_LOG, SPEED_SLOWNESS,
    speed_input,
};
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

/// How far, in document pixels, a speed-driven offset may throw a dab off the
/// pointer.
///
/// `offset_by_speed` multiplies the smoothed velocity, and a velocity is a
/// division by a reported time interval. libmypaint carries a comment about
/// individual dabs landing "far far away" on Windows for exactly this reason:
/// two samples a microsecond apart report a speed of millions. The cap is
/// generous enough that no plausible flick reaches it and small enough that a
/// spike cannot damage — and therefore snapshot for undo — the whole canvas.
const MAX_SPEED_OFFSET: f32 = 512.0;

/// Time constant, in seconds, for the velocity that drives `offset_by_speed`.
///
/// libmypaint derives it from `offset_by_speed_slowness`, whose default of 1.0
/// gives `exp(0.01) - 1`. Only ten brushes in the pack use the offset at all
/// and none of them moves the slowness far, so it is a constant here rather
/// than a field and a slider nobody would ever touch.
const SPEED_OFFSET_SLOWNESS: f32 = 0.010_05;

/// A tiny xorshift, for scattering dabs.
///
/// Deliberately not the `rand` crate. This wants a few numbers per dab with no
/// statistical claims beyond "does not visibly repeat", and `umber-core` has
/// four dependencies; adding one for eight lines would be the wrong trade.
///
/// Not seeded from the clock, either: a stroke that scatters differently every
/// time it is replayed would make the renderer's output depend on when it ran,
/// and every pixel test in the suite would become flaky. The seed advances per
/// stroke instead, so two strokes differ but one stroke is reproducible.
#[derive(Debug, Clone, Copy)]
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        // Zero is a fixed point of xorshift — it would return zero forever, and
        // a "random" scatter of exactly zero is the bug that hides itself.
        Self(seed | 1)
    }

    fn next_u32(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }

    /// Uniform in `0.0..1.0`, which is the range MyPaint's `random` input has.
    fn unit(&mut self) -> f32 {
        self.next_u32() as f32 / u32::MAX as f32
    }

    /// Uniform in `-1.0..1.0`.
    fn signed(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    /// Roughly normal, mean 0, standard deviation 1.
    ///
    /// Three uniforms summed, which is the cheap central-limit trick: close
    /// enough to a bell for scattering paint, bounded at ±3 so a single dab can
    /// never fly to the far side of the canvas, and no `ln` or `sqrt` per dab.
    fn gaussian(&mut self) -> f32 {
        self.signed() + self.signed() + self.signed()
    }
}

/// One stamp, laid out for direct upload as GPU instance data.
///
/// 40 bytes. It was 32 — a power-of-two stride, so an instance fetch never
/// straddled a cache line — until dabs stopped being circles. Shape is worth
/// the eight bytes and then some: a frame of fast drawing is a few hundred
/// dabs, so the whole instance upload is single-digit kilobytes either way,
/// while a brush that cannot be anything but round is 64% of the library
/// misrepresented.
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Dab {
    /// Centre in document pixels.
    pub pos: [f32; 2],
    /// Semi-axis along [`Dab::angle`] — the **long** one whenever `aspect`
    /// exceeds 1.0.
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
    /// Long axis over short axis. 1.0 is a circle.
    pub aspect: f32,
    /// Direction of the long axis, in **radians**. Converted from the degrees
    /// `Brush` holds once here, rather than in the shader for every fragment.
    pub angle: f32,
}

impl Dab {
    /// A plain round dab. Kept for the callers — tests, mostly — that do not
    /// care about shape.
    pub fn round(pos: Vec2, radius: f32, hardness: f32, coverage: f32) -> Self {
        Self {
            pos: [pos.x, pos.y],
            radius,
            hardness,
            coverage,
            color: [0.0; 3],
            aspect: 1.0,
            angle: 0.0,
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
    /// Which cells of the canvas the dabs have reached. The bounding box says
    /// where the stroke *is*; this says what of it the stroke touched, and it
    /// is what the undo patch and the commit pass are both cut to.
    damage: TileMask,
    /// The palette colour this stroke started with, in linear RGB.
    paint: [f32; 3],
    /// Colour carried along by a smudging brush: what it has picked up, decayed
    /// towards each new canvas sample by `smudge_length`.
    ///
    /// `None` until the first sample arrives. A smudge brush that has picked
    /// nothing up yet paints its palette colour rather than black — starting it
    /// at zero would put a dark head on every smudged stroke.
    smudge: Option<[f32; 3]>,
    /// Scatter and radius jitter. Re-seeded per stroke, so one stroke redraws
    /// identically while two strokes differ.
    rng: Rng,
    /// How many strokes have been begun, which is the seed. Wrapping is fine —
    /// it only has to differ between neighbouring strokes.
    stroke_count: u32,
    /// Unit vector along the stroke, for brushes whose dab turns to follow it.
    /// Starts pointing along +x so the very first dab of a stroke, laid before
    /// any direction exists, is not stamped at a random angle.
    heading: Vec2,
    /// Pointer speed in document pixels per second, smoothed over
    /// [`SPEED_SLOWNESS`] and [`SLOW_SPEED_SLOWNESS`] respectively. Two
    /// separate filters because MyPaint's `speed1` and `speed2` are the same
    /// measurement at two time constants, and brushes use them for different
    /// things: the fast one for a flick, the slow one for the pace of a gesture.
    speed: f32,
    slow_speed: f32,
    /// Smoothed velocity *vector*, for [`Brush::speed_offset`]. Separate from
    /// `speed` because that one is a magnitude and this one has to point.
    velocity: Vec2,
    /// How far into the stroke we are, in the `0..1` the `Stroke` input reports
    /// — advanced by travel measured in dab radii, and wrapped.
    stroke_pos: f32,
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
            damage: TileMask::default(),
            paint: [0.0; 3],
            smudge: None,
            rng: Rng::new(1),
            stroke_count: 0,
            heading: Vec2::X,
            speed: 0.0,
            slow_speed: 0.0,
            velocity: Vec2::ZERO,
            stroke_pos: 0.0,
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
        self.brush.colours_dabs()
    }

    /// Whether this stroke's dabs accumulate coverage instead of saturating.
    ///
    /// Read off the brush snapshotted at [`Self::begin`] rather than off the
    /// live one, for the same reason the colour is: the dab pass picks a
    /// pipeline from this every frame, and a stroke that changed pipeline
    /// halfway would have its first half drawn under one rule and its second
    /// under the other.
    pub fn builds_up(&self) -> bool {
        self.brush.build_up
    }

    /// The grain this stroke bites through, as `(strength, tile size)`.
    ///
    /// `None` when the brush asks for none, which is the signal to the renderer
    /// to leave the grain binding at its 1×1 placeholder and multiply by one.
    pub fn grain(&self) -> Option<(f32, f32)> {
        self.brush.has_grain().then(|| {
            (
                self.brush.grain.clamp(0.0, 1.0),
                self.brush
                    .grain_scale
                    .clamp(Brush::MIN_GRAIN_SCALE, Brush::MAX_GRAIN_SCALE),
            )
        })
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

    /// The cells of the canvas this stroke has reached.
    ///
    /// Survives [`Self::end`] along with the bounds, because both are read
    /// after the stroke is over — the commit and the undo capture happen at
    /// pointer-up, and [`Self::begin`] is what clears them.
    pub fn damage(&self) -> &TileMask {
        &self.damage
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
        self.stroke_count = self.stroke_count.wrapping_add(1);
        // Mixed rather than used raw: consecutive small seeds give xorshift
        // visibly similar first outputs, and two strokes in a row scattering
        // the same way is exactly what scatter is supposed to avoid.
        self.rng = Rng::new(self.stroke_count.wrapping_mul(0x9E37_79B9));
        self.heading = Vec2::X;
        // A stroke starts from rest and at its own beginning. Carrying the
        // previous stroke's speed over would make the first dabs of a new mark
        // wear the last one's dynamics.
        self.speed = 0.0;
        self.slow_speed = 0.0;
        self.velocity = Vec2::ZERO;
        self.stroke_pos = 0.0;
        self.smoothed = point.pos;
        self.residual = 0.0;
        self.time_residual = 0.0;
        self.bounds = Rect::empty();
        self.damage.clear();
        self.pending.clear();

        self.emit(point.pos, point.pressure);
        self.last = Some(InputPoint {
            pos: point.pos,
            ..point
        });
        // A tap has already consumed one dab's worth of travel. `heading` is
        // still `Vec2::X` here — nothing has moved yet — which for a brush
        // that follows the stroke is the whole answer and for one that does
        // not is the best guess available. The first `extend` recomputes it
        // against the direction the hand actually went.
        self.residual = self
            .brush
            .step_at(point.pressure, self.brush.off_heading(self.heading));
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

        let elapsed = (point.time - last.time).max(0.0) as f32;
        // Speed and stroke position describe the travel that has just
        // happened, so they are advanced before this sample's dabs are laid
        // rather than after them — which is also the order libmypaint uses.
        self.advance_inputs(seg, len, elapsed);

        // A timed brush deposits paint for as long as the pen is down, whether
        // or not it has moved, so its dabs are counted before the early return
        // that a stationary pen would otherwise take.
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
        self.heading = dir;

        // Walk the segment, dropping a dab every `step` document pixels. `step`
        // is recomputed per dab because pressure (and therefore size) varies
        // along the segment.
        let mut t = self.residual;
        while t <= len {
            let f = t / len;
            let pressure = last.pressure + (point.pressure - last.pressure) * f;
            self.emit(from + dir * t, pressure);
            // Off `dir` rather than off `self.heading`: they are the same
            // number here, and this is the direction the walk below is
            // actually taking.
            t += self.brush.step_at(pressure, self.brush.off_heading(dir));
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

    /// Update the inputs that are derived from motion rather than reported.
    ///
    /// Every branch is guarded by the setting that reads it. Two exponential
    /// filters per pointer sample is not much, but pointer samples are the
    /// densest thing on the drawing path and a brush that reads neither speed
    /// nor stroke position — which is most of them — should pay for neither.
    fn advance_inputs(&mut self, seg: Vec2, len: f32, elapsed: f32) {
        if elapsed <= 0.0 || !len.is_finite() {
            return;
        }
        if self.brush.is_modulated() || self.brush.leads_with_speed() {
            let raw = len / elapsed;
            // libmypaint's own `1 - exp_decay(T, dt)`, so a brush tuned against
            // its filter behaves the same here at any report rate.
            self.speed += (raw - self.speed) * (1.0 - (-elapsed / SPEED_SLOWNESS).exp());
            self.slow_speed +=
                (raw - self.slow_speed) * (1.0 - (-elapsed / SLOW_SPEED_SLOWNESS).exp());
        }
        if self.brush.leads_with_speed() {
            let v = seg / elapsed;
            let fac = 1.0 - (-elapsed / SPEED_OFFSET_SLOWNESS).exp();
            self.velocity += (v - self.velocity) * fac;
        }
        if self.brush.uses_stroke_position() {
            // Measured in dab radii rather than pixels, so a brush scaled up
            // runs through its cycle over a proportionally longer mark instead
            // of finishing it in the first inch.
            let base_radius = (self.brush.size * 0.5).max(0.5);
            let span = self.brush.stroke_span.max(0.01);
            let wrap = 1.0 + self.brush.stroke_hold.max(0.0);
            let next = self.stroke_pos + len / base_radius / span;
            self.stroke_pos = if next < wrap {
                next
            } else if wrap > 10.9 {
                // MyPaint reads a hold time at its ceiling as "never wrap":
                // the input reaches 1 and stays there for the rest of the mark.
                1.0
            } else {
                next % wrap
            };
        }
    }

    /// The inputs a dab sees, in MyPaint's units.
    ///
    /// The random draw happens here and **only** when something reads it, which
    /// is the same discipline every other random feature follows: the RNG is a
    /// single stream, so an unconditional draw for a feature a brush does not
    /// use would reshuffle the numbers every other feature gets. One draw per
    /// dab shared by every entry, exactly as libmypaint's `random_input` is.
    fn dab_inputs(&mut self, pressure: f32) -> Modulated {
        if !self.brush.is_modulated() {
            return Modulated::NONE;
        }
        let random = if self.brush.modulations.uses(DabInput::Random) {
            self.rng.unit()
        } else {
            0.0
        };
        let inputs = DabInputs {
            pressure,
            speed: speed_input(self.speed, SPEED_GAMMA_LOG),
            slow_speed: speed_input(self.slow_speed, SPEED_GAMMA_LOG),
            stroke: self.stroke_pos.min(1.0),
            // Undirected, like MyPaint's: a line pulled left and the same line
            // pulled right are the same stroke as far as a brush is concerned.
            direction: self
                .heading
                .y
                .atan2(self.heading.x)
                .to_degrees()
                .rem_euclid(180.0),
            random,
        };
        self.brush.modulations.evaluate(&inputs)
    }

    fn emit(&mut self, pos: Vec2, pressure: f32) {
        let m = self.dab_inputs(pressure);

        let mut radius = self.brush.radius_at(pressure);
        // `radius_logarithmic` is a log, so a mapping on it is an offset in log
        // space and composes by multiplying the radius. Adding it in pixels
        // would make the same setting mean something different on a 4 px pen
        // and a 400 px wash.
        if m.size_log != 0.0 {
            radius = (radius * m.size_log.exp()).clamp(0.05, Brush::MAX_SIZE);
        }
        let coverage = (self.brush.coverage_at(pressure) * m.opacity).clamp(0.0, 1.0);

        // Jitter in log space, so the variation is symmetric about the nominal
        // radius and cannot produce a negative one however large the setting.
        if self.brush.radius_jitter > 0.0 {
            radius *= (self.rng.gaussian() * self.brush.radius_jitter).exp();
            radius = radius.clamp(0.05, Brush::MAX_SIZE);
        }

        // Scatter is stated in radii, so a big brush sprays wider than a small
        // one — which is what keeps a spray can looking like itself at any size.
        // Read through `scatter_at` rather than off the field: a pencil skips
        // across the tooth of the paper under a light hand and bites into it
        // under a heavy one, so the amount is pressure's to decide.
        let mut centre = pos;
        let scatter = (self.brush.scatter_at(pressure) + m.scatter).max(0.0);
        if scatter > 0.0 {
            let spread = radius * scatter;
            centre += Vec2::new(self.rng.gaussian(), self.rng.gaussian()) * spread;
        }

        // MyPaint's `offset_by_speed`: a *directed* lead along the smoothed
        // velocity, a tenth of a second's worth of it per unit. Reading it as
        // scatter is the obvious approximation and is wrong in kind — it turns
        // a brush that trails behind a fast flick into one that sprays.
        if self.brush.leads_with_speed() {
            let lead = self.velocity * self.brush.speed_offset * 0.1;
            let len = lead.length();
            centre += if len > MAX_SPEED_OFFSET {
                lead * (MAX_SPEED_OFFSET / len)
            } else {
                lead
            };
        }

        let mut angle = if self.brush.dab_angle_follows_stroke {
            // A rake keeps its bristles across the line of travel. `heading` is
            // whatever the last segment was, so the dab turns through a curve.
            self.heading.y.atan2(self.heading.x) + self.brush.dab_angle.to_radians()
        } else {
            self.brush.dab_angle.to_radians()
        };
        // Uniform rather than the gaussian used for position and radius: a
        // rotation that clusters around one heading is still a comb, and a
        // brush asking for 360° means "any way up", not "usually this way up".
        if self.brush.dab_angle_jitter > 0.0 {
            angle += self.rng.signed() * self.brush.dab_angle_jitter.to_radians() * 0.5;
        }
        angle += m.angle.to_radians();

        // Below 1.0 the long and short axes swap, so a modulation that dips
        // under a round dab would turn the ellipse a quarter turn rather than
        // flattening it back out.
        //
        // Bound to a name because the *damage* below is derived from it. It
        // used to be written inline and the box was computed from the nominal
        // `dab_ratio` instead — see the note there.
        let aspect = (self.brush.dab_ratio + m.ratio).max(1.0);

        self.pending.push(Dab {
            pos: [centre.x, centre.y],
            radius,
            hardness: (self.brush.hardness_at(pressure) + m.hardness).clamp(0.0, 1.0),
            coverage,
            color: self.dab_color(&m),
            aspect,
            angle,
        });

        // Bounded by the axis-aligned box of the *scattered* dab's quad, which
        // is exactly what the rasteriser can touch.
        //
        // The circumscribing circle is not enough, and this is the third time
        // an under-tight damaged rect has bitten this project: coverage left
        // outside it stays in the scratch, redraws as a live preview, and is
        // then baked in by the *next* stroke wearing that stroke's colour. A
        // round dab does fit inside its bounding square at any angle, so the
        // old bound held — but a **bitmap tip paints into the quad's corners**,
        // and a rotated quad's corners reach out to `radius * sqrt(2)`. Any
        // stamp brush with an angle or angle jitter was losing its edges.
        //
        // Tight for a square tip, conservative for a non-square one and for a
        // round dab, and for an unrotated ellipse it is *tighter* than the
        // circle was.
        //
        // The short semi-axis comes from `aspect` — the dab that was actually
        // pushed — and **not** from the nominal `self.brush.dab_ratio`. This
        // was the same under-tight rect a fourth time: `dab.wgsl`'s vertex
        // shader builds the quad as `radius / max(aspect, 1.0)`, so wherever a
        // `DabTarget::Ratio` modulation lands negative the real dab is *fatter*
        // than the nominal ratio describes and the box missed its edges — on y
        // at every angle and on x wherever the dab is turned.
        //
        // Reachable from the shipped library rather than hypothetical, and
        // `mypaint/dieterle/arrow-1` is the case worth knowing because the
        // obvious reading of it is wrong. Its `Ratio` curve is not a ramp but
        // `(0, 0.5, 1, 0, 0)` against a low of -9.0 and a `dab_ratio` of 10, so
        // the dab is round at the head of a mark, reaches 10:1 at the *midpoint*
        // of `stroke_span`, and is round again for the whole second half — and
        // `stroke_hold` at its 10.0 ceiling means the input never wraps, so a
        // stroke longer than its span sits at `aspect` 1.0 for the rest of its
        // length. That is the steady state, not the transient. The one place
        // the dab reaches its nominal size is the midpoint, which is exactly
        // where `aspect` is 10 and the error is zero, so quoting the brush's
        // 25.7 px `size` against a 2.6 px box would be describing two instants
        // that never coincide. Re-derive from the curve rather than from the
        // `size` field.
        //
        // Three things this deliberately does not touch:
        //
        //   - **`Brush::step_at` still reads the nominal `dab_ratio`**, and must.
        //     Spacing is decided by the nominal ratio and angle, never by
        //     `dab_angle_jitter` or a modulation, or a stroke's spacing would
        //     wander with the RNG. Damage is per dab and has to follow the dab;
        //     spacing is per stroke and must not. That bullet sits directly
        //     above this one in CLAUDE.md and is almost certainly how the defect
        //     arrived, so the two rules are opposite by design.
        //   - **`tip_scale` is still not in it**, and that stays right:
        //     `TipMask::aspect` divides both sides by the longer, so both
        //     components are at most 1 and the quad the shader builds can only
        //     be smaller than this box on that account.
        //   - **`widgets::preview_mark` was never wrong**, and reads
        //     `dab.aspect` as this now does. The brush list's rasteriser is the
        //     one duplicate of this geometry CLAUDE.md permits, and the canvas
        //     was the outlier rather than the reference.
        let (sin, cos) = angle.sin_cos();
        let short = radius / aspect;
        let half = Vec2::new(
            (radius * cos).abs() + (short * sin).abs(),
            (radius * sin).abs() + (short * cos).abs(),
        );
        self.bounds.union_box(centre, half);
        // The same box, on the cell grid. Both have to be fed from here and
        // from the same numbers: a cell mask that did not cover what the
        // bounding box covers is the under-tight damaged rect above, back
        // again and harder to see.
        self.damage.mark(centre, half);
    }

    /// The colour this dab deposits: the palette colour, pulled towards
    /// whatever the brush has picked up, then shifted by whatever the colour
    /// modulations ask for.
    ///
    /// Pickup first and the shift second, which is libmypaint's order: a brush
    /// that both blends and jitters is jittering the mixture, not jittering its
    /// own colour and then losing the jitter in the mix.
    fn dab_color(&self, m: &Modulated) -> [f32; 3] {
        let mixed = match (self.smudge, self.brush.smudges()) {
            (Some(held), true) => {
                let t = (self.brush.smudge + m.smudge).clamp(0.0, 1.0);
                [
                    self.paint[0] + (held[0] - self.paint[0]) * t,
                    self.paint[1] + (held[1] - self.paint[1]) * t,
                    self.paint[2] + (held[2] - self.paint[2]) * t,
                ]
            }
            _ => self.paint,
        };
        if !m.tints() {
            return mixed;
        }
        tint(mixed, m)
    }
}

/// Shift a linear-RGB colour in HSV, the way MyPaint's `change_color_*` do.
///
/// Over **sRGB** components, because [`Color::to_hsv`] is defined that way and
/// for the same reason: a hue rotation is a perceptual operation, and one run
/// over linear values bunches badly in the shadows. MyPaint delinearises with a
/// plain 2.2 power before doing the same thing; the exact transfer function is
/// the only difference, and it is smaller than the shift itself.
///
/// Saturation is *scaled* rather than offset — `s += s * v * amount` is
/// libmypaint's own form — so a grey stays grey however hard the brush jitters.
/// Hue wraps; value clamps.
fn tint(rgb: [f32; 3], m: &Modulated) -> [f32; 3] {
    let mut hsv = Color::new(rgb[0], rgb[1], rgb[2], 1.0).to_hsv();
    // MyPaint states hue in turns, so 0.5 is the opposite side of the wheel.
    //
    // Through `wrap_hue` rather than a bare `rem_euclid`, which is the one door
    // a hue comes through: `NaN.rem_euclid(360.0)` is NaN, and a tiny negative
    // rounds up to exactly `360.0`, which `to_color` reads as a sixth sextant
    // that does not exist. Writing the field by struct mutation bypasses
    // `Hsv::new`, which is where that wrapping otherwise happens.
    //
    // Nothing was ever painted magenta by this and no mesh was ever discarded:
    // `Hsv::to_color` wraps its own field before using it and caught both
    // cases. So this keeps the one-door rule true rather than repairing a
    // visible failure — a rule saved only by a downstream call is drift, and
    // that call is a guarantee somebody could refactor away next month without
    // ever learning it was load-bearing.
    hsv.h = crate::color::wrap_hue(hsv.h + m.hue * 360.0);
    hsv.s = (hsv.s + hsv.s * hsv.v * m.saturation).clamp(0.0, 1.0);
    hsv.v = (hsv.v + m.value).clamp(0.0, 1.0);
    let c = hsv.to_color(1.0);
    [c.r, c.g, c.b]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brush::Brush;
    use crate::curve::ResponseCurve;
    use crate::dynamics::{DabTarget, Modulation};
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

    fn scattered(scatter: f32, jitter: f32) -> Brush {
        Brush {
            size: 20.0,
            spacing: 0.1,
            stabilization: 0.0,
            pressure_size: false,
            scatter,
            radius_jitter: jitter,
            ..Default::default()
        }
    }

    #[test]
    fn an_unscattered_brush_lays_every_dab_exactly_on_the_line() {
        // The default path, and the one every pixel test in the suite depends
        // on: no scatter means no randomness at all, not "randomness with a
        // small amplitude".
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(100.0, 0.0), 1.0, 0.1));
        for dab in s.drain_pending() {
            assert_eq!(dab.pos[1], 0.0, "a dab left the line");
            assert_eq!(dab.aspect, 1.0);
        }
    }

    #[test]
    fn scatter_moves_dabs_off_the_line_in_proportion_to_the_radius() {
        // A spray can has to spray. Stated in radii, so a big brush sprays
        // wider than a small one and the brush looks like itself at any size.
        let mut s = StrokeBuilder::new();
        s.begin(
            scattered(1.0, 0.0),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert!(dabs.len() > 20);

        let off_line = dabs.iter().filter(|d| d.pos[1].abs() > 0.5).count();
        assert!(
            off_line > dabs.len() / 2,
            "only {off_line} of {} dabs scattered",
            dabs.len()
        );
        // Bounded: three summed uniforms cannot exceed 3 sigma, so no dab can
        // fly to the far side of the canvas.
        let radius = 10.0;
        for d in &dabs {
            assert!(d.pos[1].abs() < radius * 3.5, "dab flew to {:?}", d.pos);
        }
    }

    #[test]
    fn the_damaged_rect_covers_where_the_dabs_actually_landed() {
        // Scatter moves dabs off the path, and the committed rectangle is
        // computed from these bounds. Too tight and the edge of a spray is
        // never baked in: it redraws as a live preview and is then committed by
        // the *next* stroke, in that stroke's colour. Exactly the ghosting the
        // pending-tail bug produced.
        let mut s = StrokeBuilder::new();
        s.begin(
            scattered(2.0, 0.5),
            WHITE,
            InputPoint::new(vec2(500.0, 500.0), 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(600.0, 500.0), 1.0, 0.1));

        let bounds = s.bounds();
        for d in s.drain_pending() {
            let (x, y, r) = (d.pos[0], d.pos[1], d.radius);
            assert!(
                bounds.min.x <= x - r
                    && bounds.max.x >= x + r
                    && bounds.min.y <= y - r
                    && bounds.max.y >= y + r,
                "dab at {:?} r{r} escapes {bounds:?}",
                d.pos
            );
        }
    }

    #[test]
    fn radius_jitter_varies_the_size_without_ever_going_negative() {
        let mut s = StrokeBuilder::new();
        s.begin(
            scattered(0.0, 0.8),
            WHITE,
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();

        let mut varied = false;
        for d in &dabs {
            assert!(d.radius > 0.0, "non-positive radius {}", d.radius);
            if (d.radius - 10.0).abs() > 0.5 {
                varied = true;
            }
        }
        assert!(varied, "jitter produced no variation at all");
    }

    #[test]
    fn one_stroke_redraws_identically_but_two_strokes_differ() {
        // Seeded per stroke rather than from the clock. Without this every
        // pixel test that involves a scattering brush becomes flaky, and a
        // stroke would land differently each time it was replayed.
        let mut s = StrokeBuilder::new();
        let run = |s: &mut StrokeBuilder| {
            s.begin(
                scattered(1.5, 0.0),
                WHITE,
                InputPoint::new(Vec2::ZERO, 1.0, 0.0),
            );
            s.extend(InputPoint::new(vec2(100.0, 0.0), 1.0, 0.1));
            let out: Vec<[f32; 2]> = s.drain_pending().map(|d| d.pos).collect();
            s.end();
            out
        };
        let first = run(&mut s);
        let second = run(&mut s);
        assert_ne!(first, second, "two strokes scattered identically");

        // A fresh builder replays the sequence exactly.
        let mut t = StrokeBuilder::new();
        assert_eq!(run(&mut t), first, "the first stroke is not reproducible");
    }

    #[test]
    fn a_following_dab_turns_with_the_stroke() {
        let rake = Brush {
            size: 20.0,
            spacing: 0.5,
            stabilization: 0.0,
            pressure_size: false,
            dab_ratio: 5.0,
            dab_angle_follows_stroke: true,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(rake, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();

        s.extend(InputPoint::new(vec2(50.0, 0.0), 1.0, 0.1));
        let east = s.drain_pending().next().expect("a dab").angle;
        assert!(
            east.abs() < 1e-3,
            "travelling +x should be angle 0, got {east}"
        );

        s.extend(InputPoint::new(vec2(50.0, 50.0), 1.0, 0.2));
        let south = s.drain_pending().next().expect("a dab").angle;
        assert!(
            (south - std::f32::consts::FRAC_PI_2).abs() < 1e-3,
            "travelling +y should be a quarter turn, got {south}"
        );
    }

    #[test]
    fn a_fixed_angle_dab_holds_its_angle_through_a_turn() {
        // The nib, as against the rake above. This is what produces
        // calligraphic thick-and-thin, and it is the whole difference.
        let nib = Brush {
            size: 20.0,
            spacing: 0.5,
            stabilization: 0.0,
            pressure_size: false,
            dab_ratio: 5.0,
            dab_angle: 30.0,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(nib, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.extend(InputPoint::new(vec2(50.0, 0.0), 1.0, 0.1));
        s.extend(InputPoint::new(vec2(50.0, 50.0), 1.0, 0.2));
        let want = 30f32.to_radians();
        for d in s.drain_pending() {
            assert!(
                (d.angle - want).abs() < 1e-4,
                "angle drifted to {}",
                d.angle
            );
        }
    }

    #[test]
    fn angle_jitter_turns_every_dab_a_different_way() {
        // A long dab stamped repeatedly at one angle is a comb, not a brush.
        // This is what a watercolour fringe, a charcoal and a grain brush all
        // are, and it is the single most common shape mapping in the pack.
        let grain = Brush {
            size: 20.0,
            spacing: 0.5,
            stabilization: 0.0,
            pressure_size: false,
            dab_ratio: 8.0,
            dab_angle_jitter: 360.0,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(grain, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.extend(InputPoint::new(vec2(400.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert!(dabs.len() > 20);

        // Bounded by half the stated width, and actually spread across it —
        // a jitter that never leaves a few degrees of centre would still comb.
        let limit = 180f32.to_radians() + 1e-4;
        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for d in &dabs {
            assert!(d.angle.abs() <= limit, "angle {} escaped", d.angle);
            lo = lo.min(d.angle);
            hi = hi.max(d.angle);
        }
        assert!(
            hi - lo > 180f32.to_radians(),
            "jitter only spanned {} degrees",
            (hi - lo).to_degrees()
        );
    }

    #[test]
    fn angle_jitter_is_measured_from_the_brush_angle_and_the_heading() {
        // Jitter is an offset, not a replacement: a rake with a little wobble
        // must still lie across the line of travel.
        let rake = Brush {
            size: 20.0,
            spacing: 0.5,
            stabilization: 0.0,
            pressure_size: false,
            dab_ratio: 6.0,
            dab_angle_follows_stroke: true,
            dab_angle_jitter: 20.0,
            ..Default::default()
        };
        let mut s = StrokeBuilder::new();
        s.begin(rake, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();
        s.extend(InputPoint::new(vec2(0.0, 200.0), 1.0, 0.1));
        let quarter = std::f32::consts::FRAC_PI_2;
        for d in s.drain_pending() {
            let off = (d.angle - quarter).abs();
            assert!(
                off <= 10f32.to_radians() + 1e-4,
                "a 20° wobble moved the dab {} degrees off the heading",
                off.to_degrees()
            );
        }
    }

    #[test]
    fn pressure_can_scatter_a_brush_that_is_otherwise_a_clean_line() {
        // 16 of the shipped brushes state no constant scatter and put the whole
        // of it on pressure. Reading only the constant made every one of them a
        // perfectly smooth line.
        let pencil = Brush {
            size: 20.0,
            spacing: 0.2,
            stabilization: 0.0,
            pressure_size: false,
            scatter: 2.0,
            pressure_scatter: true,
            min_scatter_ratio: 0.0,
            ..Default::default()
        };
        assert_eq!(pencil.scatter_at(0.0), 0.0, "a feather touch is clean");
        assert!((pencil.scatter_at(1.0) - 2.0).abs() < 1e-5);

        let mut s = StrokeBuilder::new();
        s.begin(pencil, WHITE, InputPoint::new(Vec2::ZERO, 0.0, 0.0));
        s.extend(InputPoint::new(vec2(100.0, 0.0), 0.0, 0.1));
        for d in s.drain_pending() {
            assert_eq!(d.pos[1], 0.0, "zero pressure must not scatter");
        }

        s.end();
        s.clear_pending();
        s.begin(pencil, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        let off_line = dabs.iter().filter(|d| d.pos[1].abs() > 0.5).count();
        assert!(off_line > dabs.len() / 2, "full pressure did not scatter");
    }

    #[test]
    fn pressure_softens_the_edge_when_the_brush_asks_it_to() {
        // `Dab::hardness` was already per-dab; it just always carried the same
        // value. 69 of the 196 shipped brushes want it to vary.
        let pencil = Brush {
            size: 20.0,
            spacing: 0.2,
            stabilization: 0.0,
            pressure_size: false,
            hardness: 0.9,
            pressure_hardness: true,
            min_hardness_ratio: 0.25,
            ..Default::default()
        };
        assert!((pencil.hardness_at(1.0) - 0.9).abs() < 1e-5);
        assert!((pencil.hardness_at(0.0) - 0.225).abs() < 1e-5);

        let mut s = StrokeBuilder::new();
        s.begin(pencil, WHITE, InputPoint::new(Vec2::ZERO, 0.2, 0.0));
        s.extend(InputPoint::new(vec2(100.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        let first = dabs.first().expect("a dab").hardness;
        let last = dabs.last().expect("a dab").hardness;
        assert!(
            last > first + 0.2,
            "hardness did not follow pressure: {first} to {last}"
        );
        assert!(dabs.iter().all(|d| (0.0..=1.0).contains(&d.hardness)));
    }

    #[test]
    fn a_brush_with_no_new_dynamics_emits_exactly_what_it_used_to() {
        // The default path. Every pixel test in the suite depends on hardness
        // being the flat setting and the RNG being untouched when nothing asks
        // for randomness — an extra draw here would reshuffle every scatter.
        let mut s = StrokeBuilder::new();
        s.begin(
            unsmoothed(20.0, 0.1),
            WHITE,
            InputPoint::new(Vec2::ZERO, 0.3, 0.0),
        );
        s.extend(InputPoint::new(vec2(100.0, 0.0), 0.7, 0.1));
        let want = Brush::default().hardness;
        for d in s.drain_pending() {
            assert_eq!(d.hardness, want);
            assert_eq!(d.angle, 0.0);
            assert_eq!(d.pos[1], 0.0);
        }
    }

    fn modulated(entries: &[Modulation]) -> Brush {
        Brush {
            modulations: entries.iter().copied().collect(),
            ..unsmoothed(20.0, 0.2)
        }
    }

    fn entry(target: DabTarget, input: DabInput, low: f32, high: f32) -> Modulation {
        Modulation {
            target,
            input,
            low,
            high,
            curve: ResponseCurve::LINEAR,
        }
    }

    /// The RNG invariant, extended to the new random input. A brush that reads
    /// `random` draws once per dab; a brush that does not must not draw at all,
    /// or every scatter in the suite shifts. The failure this guards is silent:
    /// nothing errors, the marks simply move.
    #[test]
    fn a_modulation_that_reads_no_randomness_leaves_the_rng_alone() {
        let scatter_only = scattered(1.5, 0.0);
        let plus_speed = Brush {
            modulations: [entry(DabTarget::Hardness, DabInput::Speed, -0.2, 0.2)]
                .into_iter()
                .collect(),
            ..scatter_only
        };

        let run = |brush: Brush| {
            let mut s = StrokeBuilder::new();
            s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
            s.extend(InputPoint::new(vec2(100.0, 0.0), 1.0, 0.1));
            s.drain_pending().map(|d| d.pos).collect::<Vec<_>>()
        };
        assert_eq!(
            run(scatter_only),
            run(plus_speed),
            "a non-random modulation moved the scatter stream"
        );

        // And one that *does* read randomness takes exactly one draw per dab,
        // which is what shifts it.
        let plus_random = Brush {
            modulations: [entry(DabTarget::Hardness, DabInput::Random, 0.0, 0.3)]
                .into_iter()
                .collect(),
            ..scatter_only
        };
        assert_ne!(run(scatter_only), run(plus_random));
    }

    /// Speed is measured, filtered and fed in on MyPaint's own log scale. The
    /// direction of the effect is the thing worth pinning: a brush that thins
    /// when flicked must not thicken.
    #[test]
    fn a_speed_modulation_thins_a_fast_stroke_and_not_a_slow_one() {
        let brush = modulated(&[entry(DabTarget::Size, DabInput::Speed, 0.0, -0.7)]);
        let radius_at_speed = |pixels_per_second: f32| {
            let mut s = StrokeBuilder::new();
            s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
            s.drain_pending();
            // Several samples, because the filter has a 40 ms time constant
            // and one report would only move it part of the way.
            for i in 1..=20 {
                let t = i as f64 * 0.01;
                s.extend(InputPoint::new(
                    vec2(pixels_per_second * t as f32, 0.0),
                    1.0,
                    t,
                ));
            }
            s.drain_pending().next_back().expect("a dab").radius
        };
        let slow = radius_at_speed(20.0);
        let fast = radius_at_speed(2000.0);
        assert!(fast < slow * 0.8, "slow {slow}, fast {fast}");
        // And it must stay a brush rather than vanishing.
        assert!(fast > 0.5);
    }

    /// The stroke input is a ramp measured in dab radii, so the same brush
    /// scaled up runs through its cycle over a proportionally longer mark. Two
    /// things have to hold: it moves, and it wraps.
    #[test]
    fn the_stroke_input_ramps_over_its_span_and_then_wraps() {
        let brush = Brush {
            // 10 radii of travel — a 20 px brush has a radius of 10, so a
            // full cycle is 100 document pixels.
            stroke_span: 10.0,
            // Kept clear of the 0..1 clamp on hardness, so the ramp shows as a
            // ramp rather than as a plateau.
            ..modulated(&[entry(DabTarget::Hardness, DabInput::Stroke, 0.0, 0.4)])
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();

        // Ten samples of 20 px, so two full cycles of the 100 px ramp.
        let mut hardness = Vec::new();
        for i in 1..=10 {
            let x = i as f32 * 20.0;
            s.extend(InputPoint::new(vec2(x, 0.0), 1.0, i as f64 * 0.05));
            hardness.push(s.drain_pending().next_back().expect("a dab").hardness);
        }
        // Rising over the first cycle...
        assert!(hardness[0] < hardness[1], "{hardness:?}");
        assert!(hardness[1] < hardness[2], "{hardness:?}");
        // ...then back to the bottom once past 100 px of travel...
        assert!(hardness[4] < hardness[0], "did not wrap: {hardness:?}");
        // ...and the second cycle repeats the first exactly.
        assert_eq!(&hardness[0..5], &hardness[5..10], "{hardness:?}");
    }

    /// `offset_by_speed` throws the dab *along* the direction of travel, which
    /// is what makes a brush trail. Scatter would have been the easy reading
    /// and the wrong one.
    #[test]
    fn a_speed_offset_leads_the_dab_along_the_line_of_travel() {
        let brush = Brush {
            speed_offset: 1.0,
            ..unsmoothed(20.0, 0.5)
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();
        // 4000 px/s along +x for long enough for the 10 ms filter to settle.
        for i in 1..=10 {
            let t = i as f64 * 0.01;
            s.extend(InputPoint::new(vec2(4000.0 * t as f32, 0.0), 1.0, t));
        }
        let dabs: Vec<Dab> = s.drain_pending().collect();
        let last = dabs.last().expect("a dab");
        // A tenth of a second at 4000 px/s is 400 px of lead — ahead of the
        // pointer, and exactly on the line rather than beside it.
        assert!(last.pos[0] > 4000.0 * 0.1 + 100.0, "{:?}", last.pos);
        assert_eq!(last.pos[1], 0.0, "a lead is not a spray");
        // The damaged rect has to cover where the dab actually went, or the
        // lead is never committed and ghosts onto the next stroke.
        let bounds = s.bounds();
        for d in &dabs {
            assert!(
                bounds.min.x <= d.pos[0] - d.radius && bounds.max.x >= d.pos[0] + d.radius,
                "dab at {:?} escapes {bounds:?}",
                d.pos
            );
        }
    }

    /// A speed spike — two samples a microsecond apart, which Windows does
    /// produce — must not fling a dab across the canvas and damage the whole
    /// layer with it.
    #[test]
    fn a_velocity_spike_cannot_throw_a_dab_off_the_canvas() {
        let brush = Brush {
            speed_offset: 10.0,
            ..unsmoothed(20.0, 0.5)
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.drain_pending();
        s.extend(InputPoint::new(vec2(50.0, 0.0), 1.0, 0.000_001));
        for d in s.drain_pending() {
            assert!(
                d.pos[0].abs() < MAX_SPEED_OFFSET + 100.0,
                "dab flew to {:?}",
                d.pos
            );
        }
    }

    /// Colour dynamics ride the per-dab colour path smudging already built.
    /// Hue is stated in turns, and a half-turn from white is still white — so
    /// the test uses a colour with a hue to rotate.
    #[test]
    fn a_colour_modulation_tints_each_dab_differently() {
        let brush = modulated(&[entry(DabTarget::Value, DabInput::Random, -0.4, 0.4)]);
        assert!(brush.colours_dabs(), "the stroke needs its colour scratch");

        let mut s = StrokeBuilder::new();
        s.begin(
            brush,
            [0.5, 0.2, 0.2],
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert!(dabs.len() > 20);

        let mut lo = f32::MAX;
        let mut hi = f32::MIN;
        for d in &dabs {
            assert!(
                d.color.iter().all(|c| (0.0..=1.0).contains(c)),
                "{:?}",
                d.color
            );
            lo = lo.min(d.color[0]);
            hi = hi.max(d.color[0]);
        }
        assert!(hi - lo > 0.1, "brightness did not vary: {lo}..{hi}");
    }

    /// Saturation is scaled rather than offset, which is libmypaint's own form
    /// and the reason a grey stays grey. A brush that turned neutrals coloured
    /// would be unusable for shading.
    #[test]
    fn a_saturation_modulation_leaves_a_grey_grey() {
        let brush = modulated(&[entry(DabTarget::Saturation, DabInput::Random, -1.0, 1.0)]);
        let mut s = StrokeBuilder::new();
        s.begin(
            brush,
            [0.3, 0.3, 0.3],
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        for d in s.drain_pending() {
            assert!((d.color[0] - d.color[1]).abs() < 1e-3, "{:?}", d.color);
            assert!((d.color[1] - d.color[2]).abs() < 1e-3, "{:?}", d.color);
        }
    }

    /// Ellipticity per dab. 46 brushes vary it and it costs nothing to render:
    /// `Dab::aspect` was already an instance field.
    #[test]
    fn a_ratio_modulation_reshapes_every_dab() {
        let brush = Brush {
            dab_ratio: 3.0,
            ..modulated(&[entry(DabTarget::Ratio, DabInput::Random, -2.0, 2.0)])
        };
        let mut s = StrokeBuilder::new();
        s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        let aspects: Vec<f32> = s.drain_pending().map(|d| d.aspect).collect();
        let lo = aspects.iter().copied().fold(f32::MAX, f32::min);
        let hi = aspects.iter().copied().fold(f32::MIN, f32::max);
        assert!(hi - lo > 1.0, "the dab did not change shape: {lo}..{hi}");
        // Never inside out: below 1.0 the long and short axes swap and the
        // ellipse would turn a quarter turn rather than flattening back out.
        assert!(lo >= 1.0, "aspect went below a circle: {lo}");
    }

    /// The quad `dab.wgsl`'s vertex shader builds for one dab, built the same
    /// way it does: the box `(±radius, ±short)` with `short = radius /
    /// max(aspect, 1)`, rotated by the dab's angle about its centre.
    ///
    /// `tip_scale` is left out because it is `(1, 1)` for a tipless stroke, and
    /// because its components are at most 1 in any case — it can only ever make
    /// the quad smaller, never larger than the box being checked against.
    ///
    /// This reads the *shader's* construction rather than the emitter's, which
    /// is the point: it is the specification the damaged box has to satisfy, so
    /// a test written against it cannot pass by agreeing with a mistake.
    ///
    /// Callers allow a thousandth of a pixel against it rather than demanding
    /// equality, and that slack is load-bearing twice. Here, because this
    /// reaches the extent by rotating four corners where the emitter reaches it
    /// as `|r cos| + |short sin|` — the same number in exact arithmetic, not
    /// necessarily the same float. And on a device, because WGSL specifies `/`
    /// to 2.5 ULP rather than correctly rounded, so the shader's own `short`
    /// need not be bit-identical to this one even given identical operands.
    fn dab_quad(d: &Dab) -> [Vec2; 4] {
        let short = d.radius / d.aspect.max(1.0);
        let (sin, cos) = d.angle.sin_cos();
        let pos = vec2(d.pos[0], d.pos[1]);
        let corners = [
            vec2(-1.0, -1.0),
            vec2(1.0, -1.0),
            vec2(-1.0, 1.0),
            vec2(1.0, 1.0),
        ];
        corners.map(|c| {
            let s = vec2(c.x * d.radius, c.y * short);
            pos + vec2(s.x * cos - s.y * sin, s.x * sin + s.y * cos)
        })
    }

    /// The axis-aligned box of a dab's quad.
    fn quad_box(d: &Dab) -> (Vec2, Vec2) {
        let q = dab_quad(d);
        (
            q.iter().copied().reduce(Vec2::min).unwrap(),
            q.iter().copied().reduce(Vec2::max).unwrap(),
        )
    }

    /// A chisel whose ratio modulation can round it right out.
    ///
    /// `dab_ratio` 10.0 against a `Ratio` mapping reaching -9.0 is not invented:
    /// it is `mypaint/tanda/charcoal-04` and `mypaint/dieterle/arrow-1`, both
    /// shipped. `aspect` then reaches 1.0 — a round dab — while the nominal
    /// ratio still says the short axis is a tenth of the long one. Turned by
    /// 30° so the box is wrong on x as well as on y.
    fn flattening_chisel() -> Brush {
        Brush {
            dab_ratio: 10.0,
            dab_angle: 30.0,
            ..modulated(&[entry(DabTarget::Ratio, DabInput::Random, -9.0, 0.0)])
        }
    }

    /// The damaged box has to describe the dab that was *emitted*, not the one
    /// the nominal settings describe.
    ///
    /// It did not: the box took its short semi-axis from `Brush::dab_ratio`
    /// while the dab carried a modulated `aspect`, so a mapping that dipped the
    /// ellipticity — 30 of the shipped presets can — left the real quad taller
    /// than the rectangle recorded for it. That is the under-tight damaged rect
    /// this project has been bitten by three times before: the coverage outside
    /// it stays in the scratch, redraws as a live preview so the stroke appears
    /// to hang, and is then baked in by the *next* stroke wearing that stroke's
    /// colour. It also puts pixels outside the undo patch, since the commit is
    /// scissored to the same cells the patch was captured from.
    #[test]
    fn the_damaged_box_covers_a_dab_a_ratio_modulation_fattened() {
        let mut s = StrokeBuilder::new();
        s.begin(
            flattening_chisel(),
            WHITE,
            InputPoint::new(vec2(200.0, 200.0), 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(300.0, 240.0), 1.0, 0.1));

        let bounds = s.bounds();
        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert!(
            dabs.len() > 4,
            "too few dabs to say anything: {}",
            dabs.len()
        );
        // Or the test would pass on a stroke whose modulation never bit.
        let roundest = dabs.iter().map(|d| d.aspect).fold(f32::MAX, f32::min);
        assert!(
            roundest < 2.0,
            "no dab was rounded out, so nothing was tested: narrowest aspect {roundest}"
        );

        for d in &dabs {
            for c in dab_quad(d) {
                assert!(
                    c.x >= bounds.min.x - 1e-3
                        && c.x <= bounds.max.x + 1e-3
                        && c.y >= bounds.min.y - 1e-3
                        && c.y <= bounds.max.y + 1e-3,
                    "a corner the shader rasterises is outside the damaged box: \
                     corner {c}, box {bounds:?}, aspect {}, radius {}",
                    d.aspect,
                    d.radius
                );
            }
        }
    }

    /// Feed both or neither: the cell mask is what the undo patch and the
    /// commit are cut to, so a mask that did not cover what the box covers is
    /// the same bug back again and much harder to see.
    #[test]
    fn the_cell_mask_covers_a_dab_a_ratio_modulation_fattened() {
        const CANVAS: u32 = 512;

        let mut s = StrokeBuilder::new();
        s.begin(
            flattening_chisel(),
            WHITE,
            InputPoint::new(vec2(200.0, 200.0), 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(300.0, 240.0), 1.0, 0.1));

        // The rect a patch would be cut to, exactly as the commit builds it.
        let rect = s
            .bounds()
            .to_pixels_clamped(glam::UVec2::splat(CANVAS))
            .expect("the stroke damaged nothing");
        let pieces = s.damage().pieces(rect);
        let dabs: Vec<Dab> = s.drain_pending().collect();

        let mut covered = vec![false; (CANVAS * CANVAS) as usize];
        for p in &pieces {
            for y in p.y..p.y + p.height {
                for x in p.x..p.x + p.width {
                    covered[(y * CANVAS + x) as usize] = true;
                }
            }
        }

        for d in &dabs {
            let (lo, hi) = quad_box(d);
            // Pulled in by a thousandth of a pixel before rounding out to whole
            // ones. `quad_box` reaches the extent by rotating four corners where
            // the emitter reaches it as `|r cos| + |short sin|`: the same number
            // in exact arithmetic and not necessarily the same float, so an edge
            // that lands within an ULP of an integer could otherwise name a
            // pixel column the emitter's own rounding put on the other side.
            // That is a spurious failure about f32, not about the damage rule —
            // the sibling test above allows the same 1e-3 for the same reason.
            let x0 = (lo.x + 1e-3).floor().max(0.0) as u32;
            let y0 = (lo.y + 1e-3).floor().max(0.0) as u32;
            let x1 = ((hi.x - 1e-3).ceil().max(0.0) as u32).min(CANVAS);
            let y1 = ((hi.y - 1e-3).ceil().max(0.0) as u32).min(CANVAS);
            for y in y0..y1 {
                for x in x0..x1 {
                    assert!(
                        covered[(y * CANVAS + x) as usize],
                        "pixel ({x}, {y}) is under the dab's quad and in no damage \
                         piece: aspect {}, radius {}",
                        d.aspect,
                        d.radius
                    );
                }
            }
        }
    }

    /// The other half of the rule, and the reason there is no fudge factor in
    /// the box: it is the quad's axis-aligned bound *exactly*, not a widened
    /// one. The tiling argument rests on the pixels a patch keeps being a
    /// subset of what the box held, so a box grown "to be safe" would make
    /// every small mark cost more than it used to.
    ///
    /// A brush with no ratio modulation is where the fix is an identity — the
    /// dab's `aspect` is `dab_ratio.max(1.0)` by construction — so this also
    /// pins that the change costs the rest of the library nothing.
    #[test]
    fn the_damaged_box_is_the_quad_exactly_and_no_larger() {
        let plain = Brush {
            dab_ratio: 6.0,
            dab_angle: 40.0,
            ..unsmoothed(20.0, 0.2)
        };
        // **Both**, and the modulated one is the case that matters. Checking
        // only `plain` would leave the whole fix untested here, because with no
        // ratio modulation the new arithmetic is bit-identical to the old — a
        // fudge written as `if m.ratio != 0.0 { half *= 1.05 }` would satisfy
        // it and satisfy `the_box_never_widens_...` too.
        for brush in [plain, flattening_chisel()] {
            let mut s = StrokeBuilder::new();
            // A tap, so the box belongs to one dab and equality is meaningful.
            s.begin(brush, WHITE, InputPoint::new(vec2(200.0, 200.0), 1.0, 0.0));
            let bounds = s.bounds();
            let dabs: Vec<Dab> = s.drain_pending().collect();
            assert_eq!(dabs.len(), 1);

            let (lo, hi) = quad_box(&dabs[0]);
            assert!(
                (bounds.min - lo).abs().max_element() < 1e-3
                    && (bounds.max - hi).abs().max_element() < 1e-3,
                "box {bounds:?} is not the quad's bound {lo}..{hi} (aspect {})",
                dabs[0].aspect
            );
        }
    }

    /// The box the superseded arithmetic recorded: the short semi-axis taken
    /// off the brush's *nominal* ratio rather than off the dab that was
    /// emitted. Written out because what is being pinned below is a relation
    /// between the two rules, so the old one has to be somewhere to compare to.
    fn nominal_box(dabs: &[Dab], dab_ratio: f32) -> Rect {
        let mut r = Rect::empty();
        for d in dabs {
            let (sin, cos) = d.angle.sin_cos();
            let short = d.radius / dab_ratio.max(1.0);
            r.union_box(
                vec2(d.pos[0], d.pos[1]),
                vec2(
                    (d.radius * cos).abs() + (short * sin).abs(),
                    (d.radius * sin).abs() + (short * cos).abs(),
                ),
            );
        }
        r
    }

    /// What the fix costs a brush that never flattened: nothing, and for some
    /// of them less than nothing. Pinned rather than merely argued, because a
    /// documented-and-unenforced invariant is how this defect arrived.
    ///
    /// Three cases, and between them the reason the short axis must not be
    /// "simplified" back onto the nominal ratio later:
    ///
    /// * **No `Ratio` modulation.** `m.ratio` is 0, so `(dab_ratio +
    ///   0.0).max(1.0)` is the identical expression to `dab_ratio.max(1.0)` and
    ///   the box is byte for byte what it was. That is 216 of the 258 shipped
    ///   presets and every hand-written brush.
    /// * **A positive one.** The real dab is *narrower* than nominal, so the
    ///   new box is strictly tighter and the undo patch strictly smaller.
    /// * **A negative one.** The box widens, to what the shader was already
    ///   painting. That is the bug, and the only case that costs anything.
    #[test]
    fn the_box_never_widens_for_a_brush_that_does_not_flatten() {
        let run = |brush: Brush| {
            let mut s = StrokeBuilder::new();
            s.begin(brush, WHITE, InputPoint::new(vec2(200.0, 200.0), 1.0, 0.0));
            s.extend(InputPoint::new(vec2(300.0, 240.0), 1.0, 0.1));
            let got = s.bounds();
            let dabs: Vec<Dab> = s.drain_pending().collect();
            (got, nominal_box(&dabs, brush.dab_ratio), dabs)
        };

        // Nothing modulates the ratio: identical, and not merely contained.
        let plain = Brush {
            dab_ratio: 8.0,
            dab_angle: 25.0,
            ..unsmoothed(20.0, 0.2)
        };
        let (got, was, _) = run(plain);
        assert_eq!(got, was, "an unmodulated brush's damaged box moved");

        // Positive only, so every dab is narrower than its nominal ratio says.
        let narrowing = Brush {
            dab_ratio: 4.0,
            dab_angle: 25.0,
            ..modulated(&[entry(DabTarget::Ratio, DabInput::Random, 0.0, 6.0)])
        };
        let (got, was, dabs) = run(narrowing);
        assert!(
            dabs.iter().any(|d| d.aspect > 4.5),
            "the modulation never bit, so nothing was tested"
        );
        assert!(
            got.min.x >= was.min.x
                && got.min.y >= was.min.y
                && got.max.x <= was.max.x
                && got.max.y <= was.max.y,
            "a positive ratio modulation widened the box: {got:?} against {was:?}"
        );
        assert_ne!(got, was, "the box should be strictly tighter, not equal");
    }

    /// An end-to-end check that the dab colour path — smudge mix, `tint`,
    /// `Hsv::to_color` — lands a real colour in the instance buffer, where a
    /// non-finite component would be a stamp the rasteriser draws as anything
    /// at all.
    ///
    /// **No assertion on `tint`'s output can distinguish the bare `rem_euclid`
    /// this replaced, and that is proved rather than assumed.** `wrap_hue`
    /// sends non-finite input to `0.0` and a wrapped value of exactly `360.0`
    /// to `0.0`, so for finite input it lands in `[0, 360)` and is idempotent —
    /// hence `wrap_hue(x.rem_euclid(360.0)) == wrap_hue(x)` for *every* `x`,
    /// the NaN and infinity cases included, since both routes reach `0.0`.
    /// `tint` returns a colour rather than a hue and `Hsv::to_color` opens with
    /// `wrap_hue(self.h)`, reading the field nowhere else, so the two spellings
    /// are observationally identical. Confirmed by mutation, not only by the
    /// argument: with `tint` reverted to `rem_euclid`, a hue driven to a tiny
    /// negative and a hue driven to NaN both still come out of `tint` as
    /// exactly `[1.0, 0.0, 0.0]`.
    ///
    /// The fact worth carrying away is the one that argument yields:
    /// **removing `Hsv::to_color`'s own `wrap_hue` would break `tint`.**
    /// Somebody tidying `to_color` has no other way to learn that `tint` was
    /// standing on it — which is exactly why `tint` should not have been
    /// standing on it, and now does not. The door itself is tested where the
    /// door is, in `color.rs`; this says the path through it is joined up.
    #[test]
    fn a_hue_modulation_still_lands_a_colour_on_the_dab() {
        // Fifty turns either way: far past anything a brush asks for, so the
        // wrap is exercised rather than skirted.
        let brush = modulated(&[entry(DabTarget::Hue, DabInput::Random, -50.0, 50.0)]);
        let mut s = StrokeBuilder::new();
        s.begin(
            brush,
            [0.8, 0.2, 0.1],
            InputPoint::new(Vec2::ZERO, 1.0, 0.0),
        );
        s.extend(InputPoint::new(vec2(200.0, 0.0), 1.0, 0.1));
        let dabs: Vec<Dab> = s.drain_pending().collect();
        assert!(dabs.len() > 4);
        for d in &dabs {
            for c in d.color {
                assert!(
                    c.is_finite() && (0.0..=1.0).contains(&c),
                    "dab colour {:?} is not a colour",
                    d.color
                );
            }
        }
    }

    /// One stroke still has to redraw identically with the new random input in
    /// play, or every pixel test involving one of these brushes goes flaky.
    #[test]
    fn a_modulated_stroke_is_still_reproducible() {
        let brush = modulated(&[entry(DabTarget::Ratio, DabInput::Random, -1.0, 1.0)]);
        let run = |s: &mut StrokeBuilder| {
            s.begin(brush, WHITE, InputPoint::new(Vec2::ZERO, 1.0, 0.0));
            s.extend(InputPoint::new(vec2(100.0, 0.0), 1.0, 0.1));
            let out: Vec<f32> = s.drain_pending().map(|d| d.aspect).collect();
            s.end();
            out
        };
        let mut a = StrokeBuilder::new();
        let mut b = StrokeBuilder::new();
        assert_eq!(run(&mut a), run(&mut b));
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
