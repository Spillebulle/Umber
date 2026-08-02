//! The shape of the stroke a brush preview draws.
//!
//! The brush library is 201 presets deep and the sample beside each name is how
//! you choose between them, so the sample has to be a *stroke*: something with
//! a direction that changes, a hand that presses and lifts, and a crossing —
//! because a rake, a chisel and a brush whose angle follows the stroke are
//! identical on a straight line and unmistakable the moment the line turns.
//!
//! # Why this is a curve and not a traced picture
//!
//! The obvious alternative is to draw the line by hand and ship the traced
//! points. A parametric curve wins on every count that matters here: it is
//! exact, it is the same on every row without an asset to keep in step with
//! anything, it can be resampled at whatever density a preview wants, and — the
//! reason it is in `umber-core` rather than beside the widget — the properties
//! the preview depends on are *testable without a window*. A preview whose
//! tangent was undefined somewhere would make an angle-following brush flicker
//! at that point; a path that strayed outside the box it was fitted to would
//! clip. Both are assertions about a rule, and rules live here. The same
//! argument as [`crate::camera::ScrollSpan`] and [`crate::clipboard::Clip`].
//!
//! # The shape
//!
//! One continuous sweep from left to right, riding on a shallow arc so the
//! tails are a drawn line rather than a ruled one, with one full turn of a
//! circle folded into the middle. The turn is deliberately faster than the
//! sweep — [`LOOP_RADIUS`] against the width of its own window — so the path
//! travels *backwards* through the middle of it. That is what makes the stroke
//! cross itself rather than merely bulge, and it is the whole point: a loop
//! shows the brush at every heading in one row.
//!
//! Everything is normalised into the unit box at the end, so a caller fits it
//! to whatever rectangle it has. The fit is the caller's to make non-uniform —
//! a library row is far wider than it is tall, and a flattened loop still
//! crosses.

use glam::Vec2;

/// How far the sweep rises and falls across its whole length, as a fraction of
/// that length. Small: this is the difference between a drawn line and a ruled
/// one, not a wave.
const SAG: f32 = 0.055;

/// Radius of the loop, in the same units — so the loop is a little over a
/// quarter of the stroke's length across.
const LOOP_RADIUS: f32 = 0.13;

/// Where along the stroke the turn is made. Centred a shade before the middle,
/// because the exit tail carries the lift and wants the longer run.
const LOOP_FROM: f32 = 0.28;
const LOOP_TO: f32 = 0.68;

/// How much of the stroke the hand spends pressing down, and lifting off.
///
/// Not equal, and that is the observation: a pen lands quickly and leaves
/// slowly, so the taper at the end of a mark is longer than the one at its
/// start.
const PRESS: f32 = 0.16;
const LIFT: f32 = 0.34;

/// What the pen reports at the two ends.
///
/// Light, but not nothing: a dab at zero pressure is no dab at all, and a
/// preview whose first and last stamps are missing has a stroke that starts
/// somewhere other than where the path does.
const TOUCH: f32 = 0.05;

/// One sample of the preview stroke.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PreviewPoint {
    /// Where, in the unit box: both axes in `0..=1`, y down like every other
    /// position in Umber.
    pub pos: Vec2,
    /// How hard the hand is pressing here, `0..=1`.
    pub pressure: f32,
}

/// How many points of the curve are measured for each one handed back.
///
/// The samples are placed at equal *distances* rather than at equal steps of
/// the parameter, which needs the curve measured before it can be walked. Equal
/// distances are worth the scan twice over: the hand that drew this moves at a
/// steady pace rather than sprinting through the turn, and a stroke sampled
/// evenly in space is what a pointer actually reports — so the stabiliser, the
/// speed inputs and the spacing all see something plausible.
const OVERSAMPLE: usize = 16;

/// The preview stroke, sampled at even distances along itself.
///
/// The points are normalised into the unit box *as returned* rather than by an
/// analytic bound on the curve — so however many samples are asked for, the
/// path fills the box and leaves none of it, which is what a caller fitting it
/// to a rectangle is entitled to assume.
///
/// `samples` below two is raised to two: a stroke needs somewhere to come from
/// and somewhere to go.
pub fn stroke(samples: usize) -> Vec<PreviewPoint> {
    let samples = samples.max(2);

    // The curve, scanned densely and squashed into the unit box. Normalising
    // before measuring is deliberate: the box is not square, so a distance
    // measured on the raw curve is not the distance the preview will draw.
    let fine = samples * OVERSAMPLE;
    let mut scan: Vec<(f32, Vec2)> = (0..=fine)
        .map(|i| {
            let u = i as f32 / fine as f32;
            (u, raw(u))
        })
        .collect();
    let (mut min, mut max) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for (_, pos) in &scan {
        min = min.min(*pos);
        max = max.max(*pos);
    }
    // A span of zero on an axis is not reachable from the curve above, but a
    // division by one would be a NaN in every position rather than a squashed
    // picture — so it is answered here instead of downstream.
    let span = (max - min).max(Vec2::splat(f32::MIN_POSITIVE));
    for (_, pos) in &mut scan {
        *pos = ((*pos - min) / span).clamp(Vec2::ZERO, Vec2::ONE);
    }

    // Distance travelled by each scanned point, then one output point per equal
    // share of the total.
    let mut travelled = Vec::with_capacity(scan.len());
    let mut total = 0.0;
    travelled.push(0.0);
    for pair in scan.windows(2) {
        total += (pair[1].1 - pair[0].1).length();
        travelled.push(total);
    }

    let mut walked = 0usize;
    (0..samples)
        .map(|i| {
            let target = total * i as f32 / (samples - 1) as f32;
            while walked + 2 < scan.len() && travelled[walked + 1] < target {
                walked += 1;
            }
            let step = travelled[walked + 1] - travelled[walked];
            let f = if step > 0.0 {
                ((target - travelled[walked]) / step).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let (from, to) = (scan[walked], scan[walked + 1]);
            PreviewPoint {
                pos: from.1.lerp(to.1, f),
                pressure: pressure(from.0 + (to.0 - from.0) * f),
            }
        })
        .collect()
}

/// The curve itself, before normalisation, over `u` in `0..=1`.
fn raw(u: f32) -> Vec2 {
    // The turn, as an angle that runs from nothing to one whole revolution
    // across the loop's window and holds still either side of it.
    let theta = std::f32::consts::TAU * ramp(u);
    Vec2::new(
        // The sweep, plus the turn's own travel. `sin` is zero at both ends of
        // the revolution, so the loop returns the path to the line it left.
        u + LOOP_RADIUS * theta.sin(),
        // The shallow arc the whole stroke rides on — entry low, exit high —
        // and the turn, which reaches a diameter above the line at its top and
        // nothing at either end of itself.
        SAG * (std::f32::consts::PI * u).cos() + LOOP_RADIUS * (theta.cos() - 1.0),
    )
}

/// How far through the turn the path is at `u`.
///
/// Smoothstep rather than a linear ramp: a linear one would start and stop
/// turning instantly, which is a corner in the path — and a corner is exactly
/// the undefined tangent an angle-following brush would flicker at.
fn ramp(u: f32) -> f32 {
    let t = ((u - LOOP_FROM) / (LOOP_TO - LOOP_FROM)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The pressure profile: light at the entry, heavy through the middle, lifting
/// away at the exit.
fn pressure(u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0);
    let rise = ease(u / PRESS);
    let fall = ease((1.0 - u) / LIFT);
    // Nobody holds one weight through a stroke. A gentle swell, heaviest around
    // the middle, so a brush whose size follows pressure shows a mark that
    // breathes rather than one with two tapers and a bar between them.
    let swell = 0.85 + 0.15 * (std::f32::consts::PI * u).sin();
    (TOUCH + (1.0 - TOUCH) * rise * fall * swell).clamp(0.0, 1.0)
}

/// Smoothstep on `0..=1`, with anything outside held at the ends.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// How densely the preview is actually sampled by the library rows, so the
    /// assertions below are made about the path the interface draws.
    const N: usize = 96;

    /// Do two segments of the path cross? Written out here rather than taken
    /// from `geom`, because what is being tested is that the stroke *is* a
    /// loop, and a test that shared its arithmetic with the thing it tests
    /// would pass on a shape that had none.
    fn crosses(a: (Vec2, Vec2), b: (Vec2, Vec2)) -> bool {
        let r = a.1 - a.0;
        let s = b.1 - b.0;
        let denom = r.x * s.y - r.y * s.x;
        if denom.abs() < 1e-9 {
            return false;
        }
        let d = b.0 - a.0;
        let t = (d.x * s.y - d.y * s.x) / denom;
        let v = (d.x * r.y - d.y * r.x) / denom;
        (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&v)
    }

    /// The whole reason the path is not a straight line: it crosses itself, so
    /// one row shows the brush at every heading.
    #[test]
    fn the_preview_stroke_closes_a_loop() {
        let path = stroke(N);
        let mut crossings = 0;
        for i in 0..path.len() - 1 {
            // Neighbouring segments share an endpoint and so always "cross".
            for j in i + 2..path.len() - 1 {
                if crosses(
                    (path[i].pos, path[i + 1].pos),
                    (path[j].pos, path[j + 1].pos),
                ) {
                    crossings += 1;
                }
            }
        }
        assert_eq!(crossings, 1, "expected exactly one self-crossing");
    }

    /// And it is one stroke rather than two marks: no jump anywhere along it,
    /// at any density.
    ///
    /// Stated against the *mean* step rather than an absolute figure, so it
    /// keeps its meaning however finely the path is sampled — and tightly,
    /// because the samples are placed at equal distances and a step that was
    /// half again as long as its neighbours would mean the walk had lost the
    /// curve rather than that the curve had a corner in it.
    #[test]
    fn the_preview_stroke_is_continuous() {
        for samples in [16, N, 512] {
            let path = stroke(samples);
            let steps: Vec<f32> = path
                .windows(2)
                .map(|pair| (pair[1].pos - pair[0].pos).length())
                .collect();
            let mean = steps.iter().sum::<f32>() / steps.len() as f32;
            let longest = steps.iter().copied().fold(0.0f32, f32::max);
            assert!(
                longest < mean * 1.5,
                "a gap of {longest} against a mean step of {mean} at {samples} samples"
            );
        }
    }

    /// A caller fits the path to a rectangle and expects it to fill that
    /// rectangle and stay in it. Both halves: nothing outside, and both ends of
    /// both axes reached — a path that stayed inside by being tiny would pass
    /// the first assertion and draw a preview in the middle of an empty row.
    #[test]
    fn the_preview_stroke_fills_its_unit_box_and_leaves_none_of_it() {
        let path = stroke(N);
        let (mut min, mut max) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
        for point in &path {
            assert!(
                point.pos.x >= 0.0 && point.pos.x <= 1.0,
                "x escaped: {}",
                point.pos.x
            );
            assert!(
                point.pos.y >= 0.0 && point.pos.y <= 1.0,
                "y escaped: {}",
                point.pos.y
            );
            min = min.min(point.pos);
            max = max.max(point.pos);
        }
        // Three of the four extremes are the ends of the stroke and land
        // exactly; the fourth is the top of the loop, which a walk at even
        // distances passes within half a step of rather than landing on.
        assert!(min.abs_diff_eq(Vec2::ZERO, 5e-3), "{min} is not the corner");
        assert!(max.abs_diff_eq(Vec2::ONE, 5e-3), "{max} is not the corner");
    }

    /// Every point of the stroke has a heading, and the heading turns smoothly.
    ///
    /// A dab that follows the stroke takes its angle from the last segment, so
    /// a zero-length step would leave it with no direction at all and a corner
    /// would make it flick a quarter turn between two neighbouring stamps. Both
    /// read as a preview that flickers, and both are properties of the curve
    /// rather than of the widget.
    #[test]
    fn the_preview_stroke_has_a_heading_everywhere() {
        let path = stroke(N);
        let mut headings = Vec::new();
        for pair in path.windows(2) {
            let step = pair[1].pos - pair[0].pos;
            assert!(
                step.length() > 1e-5,
                "two samples landed on top of each other at {}",
                pair[0].pos
            );
            headings.push(step.normalize());
        }
        for turn in headings.windows(2) {
            // The dot product of two unit vectors is the cosine of the angle
            // between them; a corner would take it towards -1.
            let cos = turn[0].dot(turn[1]).clamp(-1.0, 1.0);
            assert!(
                cos > 0.5,
                "the heading turned {} degrees between two samples",
                cos.acos().to_degrees()
            );
        }
    }

    /// The hand presses and lifts. Light at both ends, heavy through the
    /// middle, and never outside what a device can report.
    #[test]
    fn the_pressure_profile_starts_and_ends_light() {
        let path = stroke(N);
        let first = path.first().expect("a first sample").pressure;
        let last = path.last().expect("a last sample").pressure;
        assert!(first < 0.2, "the stroke landed at {first}");
        assert!(last < 0.2, "the stroke left at {last}");

        let peak = path
            .iter()
            .map(|point| point.pressure)
            .fold(0.0f32, f32::max);
        assert!(peak > 0.9, "the stroke never bore down: peak {peak}");
        for point in &path {
            assert!(
                (0.0..=1.0).contains(&point.pressure),
                "pressure {} is not a reading",
                point.pressure
            );
        }
        // And the lift is the longer of the two, which is what makes the exit
        // taper read as a hand leaving the paper rather than as a symmetry.
        let at = |u: f32| pressure(u);
        assert!(
            at(1.0 - PRESS) < at(PRESS),
            "the exit is no lighter than the entry at the same distance in"
        );
    }

    /// Asking for a degenerate count answers with a stroke rather than a panic
    /// or a division by zero.
    #[test]
    fn a_stroke_of_no_samples_is_still_a_stroke() {
        for samples in [0, 1, 2] {
            let path = stroke(samples);
            assert!(path.len() >= 2);
            for point in &path {
                assert!(point.pos.is_finite(), "{} is not a position", point.pos);
            }
        }
    }
}
