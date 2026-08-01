//! Brush parameters.

use serde::{Deserialize, Serialize};

use crate::curve::ResponseCurve;

/// What a stroke does to the layer underneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrushMode {
    /// Composite the stroke colour over the layer.
    Paint,
    /// Subtract the stroke's coverage from the layer's alpha.
    Erase,
}

/// A round brush.
///
/// Sizes are in **document** pixels, so brush appearance is independent of
/// zoom — painting at 12% zoom lays down exactly the pixels you would get at
/// 100%.
///
/// Every field carries `#[serde(default)]` through the container attribute, so
/// a hand-written preset in the brush library may name only the parameters it
/// cares about and pick up the defaults for the rest. That also means a library
/// written by an older Umber still loads when a field is added here.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Brush {
    /// Diameter at full pressure.
    pub size: f32,
    /// Diameter at zero pressure, as a fraction of `size`.
    pub min_size_ratio: f32,
    /// `0.0` is a fully soft falloff, `1.0` a hard (still antialiased) edge.
    pub hardness: f32,
    /// Opacity of the finished stroke, applied once on commit rather than per
    /// dab — see [`crate::stroke`] for why that distinction matters.
    pub opacity: f32,
    /// Distance between dabs as a fraction of the current diameter. Smaller is
    /// smoother and more expensive; 0.1 is a good default.
    pub spacing: f32,
    pub pressure_size: bool,
    pub pressure_opacity: bool,
    /// Shapes how pressure drives size.
    pub size_curve: ResponseCurve,
    /// Shapes how pressure drives per-dab coverage.
    pub opacity_curve: ResponseCurve,
    /// Input smoothing, `0.0` (raw) to just under `1.0` (very heavy).
    pub stabilization: f32,
    pub mode: BrushMode,
    /// How much of each dab's colour is picked up off the canvas rather than
    /// taken from the palette. `0.0` is an ordinary brush; `1.0` deposits only
    /// what it found, which is a pure blender.
    ///
    /// Non-zero is what makes a stroke *per-dab coloured*, and that costs a
    /// second scratch target — see [`crate::stroke::StrokeBuilder`]. Zero is
    /// therefore not merely the default but the fast path, and the overwhelming
    /// majority of brushes stay on it.
    pub smudge: f32,
    /// How long picked-up colour survives, `0.0` (replaced at every sample) to
    /// just under `1.0` (a very long smear). MyPaint's `smudge_length`.
    pub smudge_length: f32,
    /// Radius of the canvas patch a dab averages when it picks colour up, as a
    /// multiple of the dab radius. MyPaint's `smudge_radius_log`, already
    /// exponentiated.
    pub smudge_radius: f32,
    /// Dabs deposited per second while the pen is down, *in addition* to the
    /// distance-driven ones. `0.0` — the default — is a purely distance-driven
    /// brush, which is what almost every brush wants.
    ///
    /// A handful of MyPaint brushes set only this and no distance term: they
    /// are airbrushes, and they are supposed to keep depositing paint while the
    /// pen is held still. Without it they import as a solid line.
    pub dabs_per_second: f32,
    /// Long axis divided by short axis. `1.0` is a circle; `4.0` is a chisel
    /// four times as long as it is wide.
    ///
    /// [`Brush::size`] always describes the **long** axis, so raising this
    /// narrows the dab rather than growing it — which is what makes a flat
    /// brush cover the same ground as the round one it was derived from.
    pub dab_ratio: f32,
    /// Direction of the long axis in degrees, measured from the document's
    /// +x axis. Meaningless while `dab_ratio` is 1.0.
    pub dab_angle: f32,
    /// Whether the long axis turns to follow the stroke.
    ///
    /// The difference between a rake and a nib, and it is not cosmetic: a
    /// broad-nib pen holds one angle whatever direction you pull it, which is
    /// what produces calligraphic thick-and-thin, while a fan or rake brush
    /// keeps its bristles across the direction of travel. Both are common and
    /// neither looks remotely like the other.
    pub dab_angle_follows_stroke: bool,
    /// Random offset applied to each dab's position, in multiples of the dab
    /// radius, as a standard deviation. `0.0` lays dabs exactly on the stroke.
    ///
    /// This is what makes a spray can spray and a charcoal stick catch on the
    /// tooth of the paper. Without it those brushes are smooth lines.
    pub scatter: f32,
    /// Random variation in each dab's radius, as a standard deviation applied
    /// in **log** space — so `0.7` means "typically within a factor of two",
    /// symmetrically, and no amount of it can produce a negative radius.
    pub radius_jitter: f32,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            size: 24.0,
            min_size_ratio: 0.08,
            hardness: 0.55,
            opacity: 1.0,
            spacing: 0.1,
            pressure_size: true,
            pressure_opacity: false,
            size_curve: ResponseCurve::LINEAR,
            opacity_curve: ResponseCurve::LINEAR,
            stabilization: 0.35,
            mode: BrushMode::Paint,
            smudge: 0.0,
            smudge_length: 0.5,
            smudge_radius: 1.0,
            dabs_per_second: 0.0,
            dab_ratio: 1.0,
            dab_angle: 0.0,
            dab_angle_follows_stroke: false,
            scatter: 0.0,
            radius_jitter: 0.0,
        }
    }
}

impl Brush {
    pub const MIN_SIZE: f32 = 1.0;
    pub const MAX_SIZE: f32 = 2000.0;

    /// Dab radius for a given pressure, in document pixels.
    pub fn radius_at(&self, pressure: f32) -> f32 {
        let p = self.size_curve.sample(pressure.clamp(0.0, 1.0));
        let scale = if self.pressure_size {
            self.min_size_ratio + (1.0 - self.min_size_ratio) * p
        } else {
            1.0
        };
        (self.size * scale * 0.5).max(0.5)
    }

    /// Per-dab coverage for a given pressure.
    ///
    /// Note this deliberately excludes [`Brush::opacity`]: dabs accumulate with
    /// a `max` blend, so stroke opacity has to be applied once afterwards.
    pub fn coverage_at(&self, pressure: f32) -> f32 {
        if self.pressure_opacity {
            self.opacity_curve.sample(pressure.clamp(0.0, 1.0))
        } else {
            1.0
        }
    }

    /// Distance to the next dab, in document pixels.
    pub fn step_at(&self, pressure: f32) -> f32 {
        (self.radius_at(pressure) * 2.0 * self.spacing).max(0.25)
    }

    /// Whether this brush picks colour up off the canvas.
    ///
    /// The threshold is not zero: a smudge of a few thousandths is a rounding
    /// artefact of the import, and turning the whole per-dab colour path on for
    /// it would cost a scratch target to render something indistinguishable.
    pub fn smudges(&self) -> bool {
        self.smudge > 0.004
    }

    /// Whether the brush keeps depositing paint while the pen is stationary.
    pub fn is_timed(&self) -> bool {
        self.dabs_per_second > 0.0
    }

    /// Whether the dab is anything other than a plain circle laid on the line.
    ///
    /// Only useful for describing a brush — the dab path costs the same either
    /// way, since shape is two more floats on an instance that was going to be
    /// uploaded regardless.
    pub fn is_shaped(&self) -> bool {
        self.dab_ratio > 1.01 || self.scatter > 0.0 || self.radius_jitter > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_scales_radius_between_min_and_full() {
        let b = Brush {
            size: 100.0,
            min_size_ratio: 0.1,
            pressure_size: true,
            ..Default::default()
        };
        assert!((b.radius_at(1.0) - 50.0).abs() < 1e-4);
        assert!((b.radius_at(0.0) - 5.0).abs() < 1e-4);
    }

    #[test]
    fn step_never_reaches_zero() {
        // A zero step would spin the dab loop forever.
        let b = Brush {
            size: 1.0,
            spacing: 0.0,
            ..Default::default()
        };
        assert!(b.step_at(0.0) > 0.0);
    }
}
