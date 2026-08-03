//! Brush parameters.

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::curve::ResponseCurve;
use crate::dynamics::{DabInput, DabTarget, Modulations};
use crate::layer::BlendMode;

/// What a stroke does to the layer underneath it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrushMode {
    /// Composite the stroke colour over the layer.
    Paint,
    /// Subtract the stroke's coverage from the layer's alpha.
    Erase,
}

/// Which of the shipped paper textures a brush bites into.
///
/// An enum rather than a name, because [`Brush`] is `Copy` and a `String` would
/// end that — the same constraint that makes [`ResponseCurve`] a fixed array of
/// samples rather than a `Vec` of control points. Umber ships three papers and
/// nothing reads a fourth from disk, so a closed set is not a limitation the
/// user can feel; when a user's own paper becomes a feature, this grows a
/// variant that names one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrainPattern {
    /// Hot-pressed paper: a fine, even tooth. What a pencil catches on.
    #[default]
    Tooth,
    /// Cotton canvas: a woven grid under a slow blotch.
    Canvas,
    /// Cold-pressed rough: coarse hollows a dry brush skips across entirely.
    Grit,
}

impl GrainPattern {
    /// Every pattern, in the order the editor lists them.
    pub const ALL: [GrainPattern; 3] = [Self::Tooth, Self::Canvas, Self::Grit];

    /// The key into [`crate::tip::patterns`].
    pub fn key(self) -> &'static str {
        match self {
            Self::Tooth => "tooth",
            Self::Canvas => "canvas",
            Self::Grit => "grit",
        }
    }

    /// What the brush editor calls it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Tooth => "Tooth",
            Self::Canvas => "Canvas",
            Self::Grit => "Rough",
        }
    }
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
    /// How the finished stroke combines with the layer it lands on.
    ///
    /// The same [`BlendMode`] a layer carries, deliberately: it is evaluated by
    /// the same shared WGSL function, so a brush set to Multiply and a layer set
    /// to Multiply are the same arithmetic on the same linear premultiplied
    /// numbers. Two enums, or a second implementation of the maths, would
    /// eventually disagree about one of the five.
    ///
    /// It applies at the **composite and commit step**, not in the dab pass.
    /// The scratch still holds nothing but coverage in `0..1`, so the `max`
    /// still saturates, opacity is still applied exactly once, and a selection
    /// still clips by the one multiply it always did — none of the dab pass's
    /// invariants can be reached from here. What changes is only how the
    /// finished coverage is combined with what is underneath it, which is a
    /// question the dab pass cannot answer anyway because it never sees the
    /// layer.
    ///
    /// Meaningless while [`Brush::mode`] is [`BrushMode::Erase`] — an eraser
    /// removes coverage and deposits no colour for a mode to combine — and
    /// [`Brush::blend_applies`] is the one place that is decided.
    pub blend: BlendMode,
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
    /// Whether overlapping dabs within one stroke **accumulate** instead of
    /// saturating at the strongest of them.
    ///
    /// Off — the default — is the wet-layer scheme the whole renderer is built
    /// around: coverage takes a `max`, so a stroke crossing itself is no more
    /// opaque than one that does not, and stroke opacity is applied once at
    /// commit. That is right for a brush whose dab is a solid disc, where
    /// overlap is an artefact of how a line is drawn rather than something the
    /// artist asked for.
    ///
    /// On, coverage composites: `a = a + cov(1 - a)`. That is wrong for a disc
    /// and it is the *entire mark* for a sparse texture stamp. GIMP and Krita
    /// composite every dab, so a photographic stamp whose brightest texel is
    /// 0.49 builds to solid along a stroke; under a `max` it can never exceed
    /// 0.49 however many times it is stamped, which is a stroke half as strong
    /// as the author's. See `docs/brush-sources.md` for the measurement.
    ///
    /// It is a **blend-state** change and nothing else — the dab shader is
    /// untouched, so the two paths cannot drift into stamping different shapes.
    pub build_up: bool,
    /// How strongly a tiling grain texture bites into dab coverage, `0.0`
    /// (none) to `1.0` (the grain's dark texels erase the dab entirely).
    ///
    /// Zero is the exact identity: coverage is multiplied by
    /// `mix(1.0, grain, strength)`, so a brush that does not ask for grain
    /// pays a multiply by one and nothing else. This is what makes a pencil
    /// catch on the tooth of the paper — the grain is fixed to the *document*,
    /// not to the dab, so the same texel is hit every time the brush passes
    /// over it and a second stroke lands in the same pits as the first.
    pub grain: f32,
    /// Size of one tile of the grain texture, in document pixels.
    ///
    /// Document pixels rather than a multiple of the brush size, for the same
    /// reason the grain is anchored to the document: paper does not get coarser
    /// when you pick up a bigger pencil.
    pub grain_scale: f32,
    /// Which shipped paper the grain comes from. Meaningless while
    /// [`Brush::grain`] is zero.
    pub grain_pattern: GrainPattern,
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
            blend: BlendMode::Normal,
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
            build_up: false,
            grain: 0.0,
            grain_scale: 256.0,
            grain_pattern: GrainPattern::Tooth,
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
    /// Screen pixels a resize drag covers to double the brush — see
    /// [`Brush::size_after_drag`].
    ///
    /// The whole range, 1 px to 2000, is eleven doublings and so about 1100
    /// pixels of drag: a wide sweep for a journey nobody makes, while the step
    /// between two sizes anybody actually chooses between is a flick of the
    /// wrist. Keyboard stepping is 1.15× a press, which is this rate over 20
    /// pixels — so the two agree about what "a bit bigger" means.
    pub const RESIZE_DOUBLE_PX: f32 = 100.0;
    /// Bounds on [`Brush::grain_scale`]. Below the lower one a paper texture is
    /// finer than the pixels it is sampled at and reads as noise; above the
    /// upper one a single tile is bigger than most canvases and the tiling
    /// stops being a texture at all.
    pub const MIN_GRAIN_SCALE: f32 = 16.0;
    pub const MAX_GRAIN_SCALE: f32 = 2048.0;

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

    /// Whether the grain texture is doing anything.
    ///
    /// The threshold is not zero for the same reason [`Brush::smudges`]'s is
    /// not: a strength of a few thousandths would cost a texture binding and a
    /// second sampler to render something nobody can see.
    pub fn has_grain(&self) -> bool {
        self.grain > 0.004
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

    /// Whether [`Brush::blend`] means anything for this brush.
    ///
    /// It does not for an eraser, and that is not a gap to be filled later. A
    /// blend mode is a rule for combining a *colour* with what is underneath
    /// it, and an eraser deposits none: it is a different blend state rather
    /// than a different shader output — `src_factor: Zero`, so the layer's
    /// alpha is scaled down — and there is nothing in that for Multiply to be a
    /// mode of. So the control is not drawn for one, in the same spirit as
    /// [`Brush::dab_has_angle`] and for the same reason: a live control that
    /// does nothing is worse than one that is visibly absent.
    ///
    /// Read at the editor's one gate as well as by the control, so a brush
    /// carrying a mode from before it was switched to erasing cannot smuggle
    /// one into a stroke.
    pub fn blend_applies(&self) -> bool {
        self.mode == BrushMode::Paint
    }

    /// The blend mode this brush would actually paint with.
    ///
    /// [`BlendMode::Normal`] wherever [`Brush::blend_applies`] is false, so
    /// nothing downstream ever has to hold the pair of them in mind at once.
    pub fn effective_blend(&self) -> BlendMode {
        if self.blend_applies() {
            self.blend
        } else {
            BlendMode::Normal
        }
    }

    /// Whether the dab's angle is worth showing the user.
    ///
    /// A circle has no angle, so an Angle slider on a round brush is a control
    /// that does nothing — which is worse than one that is visibly disabled.
    pub fn dab_has_angle(&self) -> bool {
        self.dab_ratio > 1.01 || self.modulations.drives(DabTarget::Ratio)
    }

    /// The size a resize drag of `delta` screen pixels asks for, having started
    /// from `from`.
    ///
    /// Measured from where the drag began rather than stepped per event, so
    /// dragging back to the middle gives the size back exactly. Stepping would
    /// accumulate a rounding error at 500 events a second, and — worse —
    /// would make the size depend on how the hand got there.
    ///
    /// Logarithmic, because size is: the difference between a 3-pixel liner
    /// and a 6-pixel one is the whole character of the brush, and the same six
    /// pixels added to a 300-pixel wash is nothing. So the drag doubles rather
    /// than adds, at [`Brush::RESIZE_DOUBLE_PX`] pixels a doubling.
    ///
    /// Right and up are bigger, left and down smaller — the same directions the
    /// zoom tool's drag calls "more", and resolved onto one signed distance by
    /// the same [`crate::geom::drag_towards_more`]. Only the rate differs, and
    /// deliberately: a brush and a zoom are not the same scale.
    pub fn size_after_drag(from: f32, delta: Vec2) -> f32 {
        let along = crate::geom::drag_towards_more(delta);
        (from * (along / Self::RESIZE_DOUBLE_PX).exp2()).clamp(Self::MIN_SIZE, Self::MAX_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

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
    fn a_resize_drag_doubles_to_the_right_and_halves_to_the_left() {
        let d = Brush::RESIZE_DOUBLE_PX;
        assert!((Brush::size_after_drag(40.0, vec2(d, 0.0)) - 80.0).abs() < 1e-3);
        assert!((Brush::size_after_drag(40.0, vec2(-d, 0.0)) - 20.0).abs() < 1e-3);
        assert_eq!(Brush::size_after_drag(40.0, Vec2::ZERO), 40.0);
    }

    /// The vertical axis is worth exactly what the horizontal one is, and up is
    /// the bigger direction — screen y being down-positive.
    #[test]
    fn a_resize_drag_reads_both_axes() {
        let d = Brush::RESIZE_DOUBLE_PX;
        assert!((Brush::size_after_drag(40.0, vec2(0.0, -d)) - 80.0).abs() < 1e-3);
        assert!((Brush::size_after_drag(40.0, vec2(0.0, d)) - 20.0).abs() < 1e-3);
        // Down-right and up-left sit exactly between bigger and smaller, and a
        // gesture between two answers must not pick one.
        assert_eq!(Brush::size_after_drag(40.0, vec2(30.0, 30.0)), 40.0);
        assert_eq!(Brush::size_after_drag(40.0, vec2(-30.0, -30.0)), 40.0);
    }

    /// No direction is a fast lane: a diagonal is worth the distance the hand
    /// travelled and not the sum of its axes, and the pure horizontal drag this
    /// gesture used to be is still the bound. Nothing got faster by the
    /// vertical axis being taken in.
    #[test]
    fn no_resize_drag_is_worth_more_than_the_distance_the_hand_moved() {
        let diagonal = Brush::size_after_drag(40.0, vec2(40.0, -40.0));
        let same_distance = Brush::size_after_drag(40.0, vec2(40.0 * 2f32.sqrt(), 0.0));
        assert!(
            (diagonal - same_distance).abs() < 1e-3,
            "{diagonal} vs {same_distance}"
        );

        let bound = Brush::size_after_drag(40.0, vec2(50.0, 0.0));
        for step in 0..64 {
            let angle = step as f32 * std::f32::consts::TAU / 64.0;
            let size = Brush::size_after_drag(40.0, vec2(angle.cos(), angle.sin()) * 50.0);
            assert!(size <= bound + 1e-3, "{angle} gave {size}");
            assert!(size >= 40.0 * 40.0 / bound - 1e-3, "{angle} gave {size}");
        }
    }

    #[test]
    fn a_resize_drag_is_worth_the_same_wherever_it_starts() {
        // The point of doubling rather than adding: the same flick of the
        // wrist is a useful change to a liner and to a wash.
        let small = Brush::size_after_drag(4.0, vec2(25.0, 0.0)) / 4.0;
        let large = Brush::size_after_drag(400.0, vec2(25.0, 0.0)) / 400.0;
        assert!((small - large).abs() < 1e-4, "{small} vs {large}");
    }

    #[test]
    fn dragging_back_gives_the_size_back() {
        // What measuring from where the drag began buys, and why the size is
        // not stepped per pointer event: at 500 events a second, an accumulated
        // size would come home a different brush.
        //
        // Out along a diagonal and back, so the round trip is pinned for a hand
        // that wandered on both axes rather than only for one that ran along a
        // rail.
        let mut size = 63.0;
        for step in 0..200 {
            size = Brush::size_after_drag(63.0, vec2(step as f32 * 0.5, step as f32 * -0.3));
        }
        for step in (0..200).rev() {
            size = Brush::size_after_drag(63.0, vec2(step as f32 * 0.5, step as f32 * -0.3));
        }
        assert_eq!(size, 63.0);
    }

    #[test]
    fn a_resize_drag_cannot_leave_the_brush_outside_its_limits() {
        // A drag has no end, so the clamp is the only thing between it and a
        // brush the engine will not paint.
        assert_eq!(
            Brush::size_after_drag(1000.0, vec2(5000.0, -5000.0)),
            Brush::MAX_SIZE
        );
        assert_eq!(
            Brush::size_after_drag(1.5, vec2(-5000.0, 5000.0)),
            Brush::MIN_SIZE
        );
    }

    /// An eraser has no colour to combine with anything, so whatever mode the
    /// brush is carrying must not reach the stroke. Decided once, here, rather
    /// than at each of the places that read the field.
    #[test]
    fn an_eraser_has_no_blend_mode() {
        let mut b = Brush {
            blend: BlendMode::Multiply,
            ..Default::default()
        };
        assert!(b.blend_applies());
        assert_eq!(b.effective_blend(), BlendMode::Multiply);

        b.mode = BrushMode::Erase;
        assert!(!b.blend_applies());
        assert_eq!(b.effective_blend(), BlendMode::Normal);
        assert_eq!(
            b.blend,
            BlendMode::Multiply,
            "the field is not cleared — switching back to painting restores it"
        );
    }

    /// A library written before brushes had a blend mode still loads, and the
    /// brush it names paints exactly as it did: `#[serde(default)]` on the
    /// container is what carries that, and this is what stops a later field
    /// being added without it.
    #[test]
    fn a_preset_written_before_blend_modes_still_loads_as_normal() {
        let b: Brush = ron::from_str("(size: 40.0, hardness: 0.9)").unwrap();
        assert_eq!(b.size, 40.0);
        assert_eq!(b.blend, BlendMode::Normal);

        let round: Brush = ron::from_str(
            &ron::ser::to_string(&Brush {
                blend: BlendMode::Screen,
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(round.blend, BlendMode::Screen);
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
