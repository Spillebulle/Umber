//! Brush parameters.

use serde::{Deserialize, Serialize};

use crate::curve::ResponseCurve;
use crate::dynamics::{DabInput, DabTarget, Modulations};

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
    /// Whether pressure drives the edge falloff.
    ///
    /// The most widely used dynamic in the MyPaint pack after size and
    /// opacity — 69 of its 196 brushes map hardness onto pressure — and the
    /// reason a pencil's light strokes are feathery rather than merely thin.
    pub pressure_hardness: bool,
    /// Shapes how pressure drives size.
    pub size_curve: ResponseCurve,
    /// Shapes how pressure drives per-dab coverage.
    pub opacity_curve: ResponseCurve,
    /// Shapes how pressure drives hardness.
    pub hardness_curve: ResponseCurve,
    /// Hardness at zero pressure, as a fraction of `hardness`. Mirrors
    /// [`Brush::min_size_ratio`], and for the same reason: a curve that reaches
    /// zero would mean a completely diffuse dab at a feather touch, which no
    /// real brush does.
    pub min_hardness_ratio: f32,
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
    /// Random rotation added to each dab, in degrees, as the **full width** of
    /// a uniform range: `360.0` is a dab that may point anywhere, `0.0` one
    /// that holds its angle exactly.
    ///
    /// The third state of an elliptical dab, alongside the nib and the rake
    /// above, and it is what a watercolour fringe, a charcoal and a grain
    /// brush all are. Without it a long dab repeated down a stroke is a comb:
    /// every stamp lies the same way, so the mark reads as machined ruling
    /// rather than as a loaded brush. 31 of the shipped 196 ask for it.
    pub dab_angle_jitter: f32,
    /// Random offset applied to each dab's position, in multiples of the dab
    /// radius, as a standard deviation. `0.0` lays dabs exactly on the stroke.
    ///
    /// This is what makes a spray can spray and a charcoal stick catch on the
    /// tooth of the paper. Without it those brushes are smooth lines.
    pub scatter: f32,
    /// Whether pressure drives scatter.
    ///
    /// Usually *inversely*: a pencil bitten into the paper lays a solid line
    /// while a light one skips across the tooth. 38 of the shipped brushes map
    /// it, and 16 of those state no constant scatter at all — before this they
    /// imported as perfectly smooth lines wearing the name of something
    /// granular.
    pub pressure_scatter: bool,
    /// Scatter at zero pressure, as a fraction of `scatter`. Unlike the size
    /// and hardness ratios this may legitimately be `0.0`: a brush that
    /// scatters only when pressed is an ordinary thing to want.
    pub min_scatter_ratio: f32,
    /// Shapes how pressure drives scatter.
    pub scatter_curve: ResponseCurve,
    /// Random variation in each dab's radius, as a standard deviation applied
    /// in **log** space — so `0.7` means "typically within a factor of two",
    /// symmetrically, and no amount of it can produce a negative radius.
    pub radius_jitter: f32,
    /// How far each dab leads the pointer along its own direction of travel,
    /// in seconds of the current speed. MyPaint's `offset_by_speed`.
    ///
    /// Not scatter: the offset is *directed*, so a fast flick throws the mark
    /// ahead of (or, negative, behind) the cursor while a slow one sits on it.
    /// That is a trailing brush, and reading it as a spray — which is the
    /// obvious-looking approximation — turns a smear into confetti.
    pub speed_offset: f32,
    /// How far the pointer travels, in dab radii, before the `Stroke` input
    /// reaches 1. MyPaint's `exp(stroke_duration_logarithmic)`.
    ///
    /// Only consulted when something reads [`DabInput::Stroke`]. It is stated
    /// in radii rather than pixels for the same reason scatter is: a brush
    /// scaled up should behave like itself, not run through its cycle sooner.
    pub stroke_span: f32,
    /// Extra travel, as a multiple of [`Brush::stroke_span`], that the `Stroke`
    /// input sits at 1 before wrapping back to 0. MyPaint's `stroke_holdtime`;
    /// its own ceiling of 10 means "never wrap".
    pub stroke_hold: f32,
    /// Inputs other than pressure, routed onto whatever they drive.
    ///
    /// Empty for every hand-written preset and for most imports, which is the
    /// fast path: [`crate::stroke::StrokeBuilder`] skips the whole evaluation
    /// and does not touch the RNG. See [`crate::dynamics`] for why this is a
    /// small fixed table rather than a curve per input per target.
    pub modulations: Modulations,
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
            pressure_hardness: false,
            size_curve: ResponseCurve::LINEAR,
            opacity_curve: ResponseCurve::LINEAR,
            hardness_curve: ResponseCurve::LINEAR,
            min_hardness_ratio: 0.5,
            stabilization: 0.35,
            mode: BrushMode::Paint,
            smudge: 0.0,
            smudge_length: 0.5,
            smudge_radius: 1.0,
            dabs_per_second: 0.0,
            dab_ratio: 1.0,
            dab_angle: 0.0,
            dab_angle_follows_stroke: false,
            dab_angle_jitter: 0.0,
            scatter: 0.0,
            pressure_scatter: false,
            min_scatter_ratio: 0.0,
            scatter_curve: ResponseCurve::LINEAR,
            radius_jitter: 0.0,
            speed_offset: 0.0,
            // MyPaint's own default, exp(4): about 55 radii of travel for a
            // full cycle of the stroke input.
            stroke_span: 54.598_15,
            stroke_hold: 0.0,
            modulations: Modulations::EMPTY,
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

    /// Edge falloff for a given pressure.
    ///
    /// Scaled between `min_hardness_ratio * hardness` and `hardness` rather
    /// than between zero and `hardness`, so the softest end of the curve is
    /// still a brush rather than a cloud.
    pub fn hardness_at(&self, pressure: f32) -> f32 {
        if !self.pressure_hardness {
            return self.hardness;
        }
        let p = self.hardness_curve.sample(pressure.clamp(0.0, 1.0));
        let ratio = self.min_hardness_ratio.clamp(0.0, 1.0);
        (self.hardness * (ratio + (1.0 - ratio) * p)).clamp(0.0, 1.0)
    }

    /// Scatter for a given pressure, in multiples of the dab radius.
    ///
    /// The floor is zero, not a fraction of `scatter`: "no scatter at all
    /// below a light touch" is the shape of a real pencil and the ratio has to
    /// be able to say it.
    pub fn scatter_at(&self, pressure: f32) -> f32 {
        if !self.pressure_scatter {
            return self.scatter;
        }
        let p = self.scatter_curve.sample(pressure.clamp(0.0, 1.0));
        let ratio = self.min_scatter_ratio.clamp(0.0, 1.0);
        (self.scatter * (ratio + (1.0 - ratio) * p)).max(0.0)
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
    ///
    /// A brush whose pickup is stated *entirely* as a modulation still smudges
    /// even though the field reads zero — 42 of the shipped 196 put the whole
    /// of it on pressure, and reading the field alone made every one of them a
    /// brush that deposits flat paint however hard you lean on it.
    pub fn smudges(&self) -> bool {
        self.smudge > 0.004 || self.modulations.drives(DabTarget::Smudge)
    }

    /// Whether individual dabs may carry a colour of their own — either picked
    /// up off the canvas or shifted by a colour modulation.
    ///
    /// This is what decides whether the stroke needs its second scratch target,
    /// so it must stay false for the overwhelming majority of brushes.
    pub fn colours_dabs(&self) -> bool {
        self.smudges() || self.modulations.tints()
    }

    /// Whether anything but pressure varies this brush along a stroke.
    pub fn is_modulated(&self) -> bool {
        !self.modulations.is_empty()
    }

    /// Whether the dab's position depends on how fast the pointer is moving.
    pub fn leads_with_speed(&self) -> bool {
        self.speed_offset.abs() > 1e-4
    }

    /// Whether anything reads the `Stroke` input, and therefore whether
    /// [`Brush::stroke_span`] means anything for this brush.
    pub fn uses_stroke_position(&self) -> bool {
        self.modulations.uses(DabInput::Stroke)
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
        self.dab_ratio > 1.01
            || self.scatter > 0.0
            || self.radius_jitter > 0.0
            || self.modulations.drives(DabTarget::Ratio)
            || self.modulations.drives(DabTarget::Scatter)
    }

    /// Whether the dab's angle is worth showing the user.
    ///
    /// A circle has no angle, so an Angle slider on a round brush is a control
    /// that does nothing — which is worse than one that is visibly disabled.
    pub fn dab_has_angle(&self) -> bool {
        self.dab_ratio > 1.01 || self.modulations.drives(DabTarget::Ratio)
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
