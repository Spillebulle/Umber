//! Response curves — how an input like pen pressure maps to an output like
//! brush size.
//!
//! A linear response is rarely what a painter wants: pens vary in how hard they
//! are to press, and a curve is how you compensate without changing your hand.
//!
//! The curve is a fixed number of evenly spaced samples rather than free
//! control points. That keeps it `Copy` (so [`crate::Brush`] stays `Copy`),
//! makes sampling a lerp with no search, and gives the editor a fixed set of
//! handles to drag — you can only move a point up and down, never past its
//! neighbours, so the curve can never become non-monotonic in x.

/// Maps `0.0..=1.0` to `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResponseCurve {
    /// Output at inputs 0, 0.25, 0.5, 0.75, 1.
    pub points: [f32; Self::N],
}

impl Default for ResponseCurve {
    fn default() -> Self {
        Self::LINEAR
    }
}

impl ResponseCurve {
    pub const N: usize = 5;

    pub const LINEAR: Self = Self {
        points: [0.0, 0.25, 0.5, 0.75, 1.0],
    };
    /// Slow to build up, then rises quickly — a light hand stays thin.
    pub const EASE_IN: Self = Self {
        points: [0.0, 0.06, 0.25, 0.56, 1.0],
    };
    /// Reaches full quickly — for pens that are stiff to press.
    pub const EASE_OUT: Self = Self {
        points: [0.0, 0.44, 0.75, 0.94, 1.0],
    };
    /// Nearly on/off, for a marker-like response.
    pub const HARD: Self = Self {
        points: [0.0, 0.02, 0.5, 0.98, 1.0],
    };

    pub const PRESETS: [(&'static str, Self); 4] = [
        ("Linear", Self::LINEAR),
        ("Ease in", Self::EASE_IN),
        ("Ease out", Self::EASE_OUT),
        ("Hard", Self::HARD),
    ];

    /// Input position of point `i`, in `0.0..=1.0`.
    pub fn x_of(i: usize) -> f32 {
        i as f32 / (Self::N - 1) as f32
    }

    /// Evaluate the curve, interpolating linearly between samples.
    pub fn sample(&self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        let scaled = t * (Self::N - 1) as f32;
        let i = (scaled.floor() as usize).min(Self::N - 2);
        let f = scaled - i as f32;
        (self.points[i] + (self.points[i + 1] - self.points[i]) * f).clamp(0.0, 1.0)
    }

    pub fn set(&mut self, i: usize, value: f32) {
        if let Some(p) = self.points.get_mut(i) {
            *p = value.clamp(0.0, 1.0);
        }
    }

    /// Name of the matching preset, if the curve is still one of them.
    pub fn preset_name(&self) -> Option<&'static str> {
        Self::PRESETS
            .iter()
            .find(|(_, c)| {
                c.points
                    .iter()
                    .zip(self.points.iter())
                    .all(|(a, b)| (a - b).abs() < 1e-4)
            })
            .map(|(name, _)| *name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_curve_is_the_identity() {
        let c = ResponseCurve::LINEAR;
        for i in 0..=20 {
            let t = i as f32 / 20.0;
            assert!((c.sample(t) - t).abs() < 1e-4, "at {t} got {}", c.sample(t));
        }
    }

    #[test]
    fn the_ends_are_pinned_for_every_preset() {
        // A curve that does not reach 1.0 means full pressure never gives full
        // size, which reads as a broken pen rather than a deliberate curve.
        for (name, c) in ResponseCurve::PRESETS {
            assert!(c.sample(0.0).abs() < 1e-4, "{name} does not start at 0");
            assert!(
                (c.sample(1.0) - 1.0).abs() < 1e-4,
                "{name} does not end at 1"
            );
        }
    }

    #[test]
    fn presets_are_monotonic() {
        // Pressing harder must never produce a smaller result.
        for (name, c) in ResponseCurve::PRESETS {
            for pair in c.points.windows(2) {
                assert!(pair[1] >= pair[0], "{name} dips: {:?}", c.points);
            }
        }
    }

    #[test]
    fn ease_in_starts_below_linear() {
        let c = ResponseCurve::EASE_IN;
        assert!(c.sample(0.25) < 0.25, "got {}", c.sample(0.25));
    }

    #[test]
    fn ease_out_starts_above_linear() {
        let c = ResponseCurve::EASE_OUT;
        assert!(c.sample(0.25) > 0.25, "got {}", c.sample(0.25));
    }

    #[test]
    fn sampling_is_clamped_at_both_ends() {
        let c = ResponseCurve::EASE_IN;
        assert_eq!(c.sample(-5.0), c.sample(0.0));
        assert_eq!(c.sample(5.0), c.sample(1.0));
    }

    #[test]
    fn edits_are_clamped_and_out_of_range_indices_ignored() {
        let mut c = ResponseCurve::LINEAR;
        c.set(2, 4.0);
        assert_eq!(c.points[2], 1.0);
        c.set(99, 0.5);
        assert_eq!(c.points.len(), ResponseCurve::N);
    }

    #[test]
    fn preset_names_resolve_and_stop_after_editing() {
        let mut c = ResponseCurve::EASE_IN;
        assert_eq!(c.preset_name(), Some("Ease in"));
        c.set(2, 0.9);
        assert_eq!(c.preset_name(), None);
    }
}
