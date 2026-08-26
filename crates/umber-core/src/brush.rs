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
    /// Distance between dabs as a fraction of how wide the dab is **along the
    /// stroke**. Smaller is smoother and more expensive; 0.1 is a good default.
    ///
    /// Along the stroke rather than across the long axis, which for a round dab
    /// is the same number and for a chisel is not — see [`Brush::step_at`],
    /// which has the argument and the brush that proved it.
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
    /// What one dab lays down, as a fraction of the mark the stroke builds to.
    ///
    /// Photoshop's Flow and Krita's build-up painting mode. Below `1.0` a dab
    /// carries less than the finished mark, so the stroke *arrives* at that
    /// mark over several dabs instead of reaching it with the first — and a
    /// stroke that crosses itself goes over the same pixels twice and comes out
    /// darker there, which is the thing a painter reaches for this to get.
    ///
    /// **It is not [`Brush::opacity`] and the two are different numbers on
    /// purpose.** Opacity caps the *finished stroke* and is applied exactly
    /// once, at commit, over coverage the dab pass has already saturated —
    /// folding it into a dab is the compounding bug the whole wet-layer scheme
    /// exists to prevent, and [`crate::stroke`] says so at the line that would
    /// do it. Flow is the opposite end: it is a statement about one dab, it
    /// never reaches the commit, and the scratch still holds coverage in `0..1`
    /// so composite and commit are untouched. Halving opacity halves the
    /// finished stroke everywhere; halving flow leaves a well-travelled stroke
    /// at full strength and thins only its thin ends and its first few dabs.
    ///
    /// **`1.0` is the exact identity and every shipped preset depends on it.**
    /// Nothing multiplies by it on the `max` path — [`Brush::builds`] answers
    /// false, so the conversion is not reached at all.
    ///
    /// **Below `1.0` it selects the accumulating blend**, through
    /// [`Brush::builds`]. Under the `max` a uniform per-dab scale is not flow at
    /// all: `max` is idempotent, so every dab writing `flow` caps the stroke at
    /// `flow` and the mark comes out uniformly fainter and just as flat, with
    /// crossing still doing nothing. There is nothing to build with until the
    /// blend composites.
    pub flow: f32,
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
            flow: 1.0,
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
    /// The heaviest smoothing anything may ask for.
    ///
    /// A **user-interface** bound and not a mathematical one, and saying which
    /// matters: `StrokeBuilder`'s filter is `(1.0 - stabilization)` clamped to
    /// at least 0.02, so even a stabilisation of 1.0 still converges on the
    /// pointer — slowly enough to feel broken, which is what this is for and is
    /// all it is for.
    ///
    /// It is a constant because three places had typed 0.95 by hand: the brush
    /// editor's Tip section, the tool options strip's rail, and the MyPaint
    /// importer's ceiling. Three copies is how a rail comes to disagree with
    /// what an import can produce, and an imported brush above the rail's top
    /// is one whose setting cannot be put back where it was found.
    pub const MAX_STABILIZATION: f32 = 0.95;

    /// The lightest flow the rails offer, and a bound on the *value* as well.
    ///
    /// A user-interface bound in the sense [`Brush::MAX_STABILIZATION`] is one,
    /// and a pixel bound underneath it. At a mark of 1.0 a dab carries
    /// `MIN_FLOW` straight into an `R8Unorm` scratch, so 0.01 is 2.55 levels of
    /// 255: faint, and still a mark. The next decade down is not — 0.001 is a
    /// quarter of a level, which the store rounds to nothing, and a *constant*
    /// increment under half a level never moves the accumulator however many
    /// dabs land on it. That is the invisible-stroke defect
    /// [`crate::tip::SCRATCH_LEVEL`] exists to bound, and a rail that reaches it
    /// is a control whose bottom end paints nothing at all.
    pub const MIN_FLOW: f32 = 0.01;

    /// [`Brush::flow`], bounded.
    ///
    /// The field is public and a hand-written preset may name anything, so the
    /// bound is applied where it is *read* rather than trusted at the rail —
    /// the same arrangement `grain` and `stabilization` keep.
    pub fn flow(&self) -> f32 {
        self.flow.clamp(Self::MIN_FLOW, 1.0)
    }

    /// Whether this stroke's dabs **accumulate** coverage instead of saturating
    /// at the strongest of them.
    ///
    /// The one statement of the question, asked by [`crate::stroke::Stroke`] for
    /// the pipeline the renderer picks and by the conversion in
    /// [`crate::stroke::StrokeBuilder`] that decides what a dab carries. One
    /// function rather than two readings of two fields, because those two have
    /// to agree for every frame of a stroke: a dab converted for a blend it is
    /// not then drawn under is a mark at the wrong strength, and which way it is
    /// wrong depends on which of the two was consulted.
    ///
    /// [`Brush::build_up`] is the author saying the dab is not solid — a sparse
    /// stamp, a grain — and [`Brush::flow`] below 1.0 is the author asking for
    /// less than the mark per dab. Either needs the accumulating blend and
    /// neither implies the other, so this is an `||` and not a single field.
    pub fn builds(&self) -> bool {
        self.build_up || self.flow() < 1.0
    }

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

    /// Coverage the **mark** reaches at a given pressure.
    ///
    /// Note this deliberately excludes [`Brush::opacity`]: dabs accumulate with
    /// a `max` blend, so stroke opacity has to be applied once afterwards.
    ///
    /// The mark rather than one dab, and under the `max` those are the same
    /// number — which is why this reads as "per-dab coverage" everywhere the
    /// dab pass is being described. Under [`Brush::build_up`] they part company,
    /// and it is [`crate::stroke::StrokeBuilder`] that converts: this stays the
    /// figure the brush editor's curve draws and the figure the stroke arrives
    /// at. See [`crate::tip::per_dab_for_stroke`] for what happens to a
    /// pressure-opacity ramp when the two are confused.
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

    /// Distance to the next dab, in document pixels, for a dab whose long axis
    /// makes an angle of `off_heading` radians with the direction of travel.
    ///
    /// **Spacing is a fraction of how far the dab reaches *along the stroke*,
    /// not of its long axis**, and the difference is the whole of this
    /// function. A round dab reaches the same distance whichever way it is
    /// walked, so every brush that has ever been round is untouched. A chisel
    /// is not. A
    /// marker nib held across the line — [`Brush::dab_angle`] at 90° with
    /// [`Brush::dab_angle_follows_stroke`] — travels on its *short* axis, so
    /// measuring the step against the long one steps further than the dab
    /// reaches and lays the mark down as a row of separate ellipses with gaps
    /// between them.
    ///
    /// That is not a hypothetical and it is not an import bug: MyPaint states
    /// its spacing against the same long radius and Ramón Miranda's "Marker"
    /// therefore asks for a step of 14.1 px on a dab 10.4 px wide, which is
    /// what the brush had always done here. Twelve shipped presets did it —
    /// every marker, the calligraphy pen, the palette knife — and the reading
    /// that fixes them is the one every painter would assume: a spacing of 10%
    /// means each dab lands a tenth of a mark past the last one, whatever shape
    /// the mark is.
    ///
    /// **The reading is the ellipse's *radius* in the direction of travel**,
    /// `1 / sqrt((cos Δ / a)² + (sin Δ / b)²)`, with `b = a / dab_ratio`. Not
    /// its shadow, `sqrt((a cos Δ)² + (b sin Δ)²)`, which is what this used
    /// first and is a different number wherever the stroke runs at an angle to
    /// the nib: for the shipped calligraphy pen at 46° off its own axis they
    /// are 8.3 px and 2.9 px, and the pen combed at the larger one. The shadow
    /// is how much ground the dab covers *measured across* the direction of
    /// travel; what decides whether two dabs merge is how far the ellipse
    /// actually reaches *along* it. The two agree at 0° and at 90°, which is
    /// why the markers were fixed by either and the calligraphy pen by only
    /// one of them.
    ///
    /// Nominal rather than per-dab: [`Brush::dab_angle_jitter`] and a
    /// `dynamics` modulation both move a single dab's angle, and letting either
    /// decide the *step* would make the spacing of a stroke wander with the RNG.
    pub fn step_at(&self, pressure: f32, off_heading: f32) -> f32 {
        // Floored in **document pixels**, so on a dab a pixel or two across the
        // floor rather than the spacing decides the step. Anything reasoning
        // about how deep the dabs pile up has to know that: at a radius of 1 a
        // spacing of 2% asks for a step of 0.04 px and gets 0.25, which is six
        // times fewer dabs over a point than the spacing suggests.
        // `tip::stack_depth` takes the step and the reach for exactly this
        // reason rather than recomputing either.
        (self.reach_at(pressure, off_heading) * 2.0 * self.spacing).max(0.25)
    }

    /// How far the dab reaches from its own centre in the direction of travel,
    /// in document pixels.
    ///
    /// The ellipse's **radius** in that direction, `1 / sqrt((cos Δ / a)² +
    /// (sin Δ / b)²)`, which is what [`Brush::step_at`] measures the spacing
    /// against and what the fragment shader's `length(local) <= 1` is a test on.
    /// Factored out so nothing else has to restate it: it is also the length one
    /// unit of the dab's own frame comes to, which is what turns a step in pixels
    /// into a step in the units a falloff is written in.
    pub fn reach_at(&self, pressure: f32, off_heading: f32) -> f32 {
        let long = self.radius_at(pressure).max(1e-4);
        let short = (long / self.dab_ratio.max(1.0)).max(1e-4);
        let (sin, cos) = off_heading.sin_cos();
        // `a == b` reduces this to `a` exactly, which is what keeps every round
        // brush byte for byte what it was.
        ((cos / long).powi(2) + (sin / short).powi(2))
            .sqrt()
            .recip()
    }

    /// How far the dab's long axis sits from the direction of travel, in
    /// radians, for a stroke that is going anywhere at all.
    ///
    /// Constant for a brush that follows the stroke — that is what following
    /// means — and, for one that does not, a function of which way the hand is
    /// moving. Both are exactly what [`crate::stroke::StrokeBuilder`] computes
    /// for the dab's own angle, minus the two per-dab terms that must not reach
    /// the spacing.
    pub fn off_heading(&self, heading: Vec2) -> f32 {
        if self.dab_angle_follows_stroke {
            return self.dab_angle.to_radians();
        }
        if heading.length_squared() < 1e-12 {
            // No direction to be off. The long axis is as good an answer as
            // any and is the one the round case gives.
            return 0.0;
        }
        self.dab_angle.to_radians() - heading.y.atan2(heading.x)
    }

    /// [`Brush::smudge`] for an application that states the pickup and the
    /// **paint-deposit rate** as two separate numbers.
    ///
    /// Krita's colour-smudge engine is the case this exists for: `SmudgeRate`
    /// is how much of the canvas a dab lifts and `ColorRate` is how much fresh
    /// paint it lays down, and the two are independent knobs. Umber has one,
    /// and it is not a missing feature — a dab deposits
    /// `lerp(palette, picked-up, smudge)`, so **`1 - smudge` already *is* a
    /// paint-deposit rate**. What a two-knob application says that one number
    /// cannot is the pair's *magnitude*: how faintly the dab lands at all,
    /// rather than what colour it is.
    ///
    /// The mix is the pickup's share of the two, `p / (p + d)`. **That is a
    /// heuristic and not anybody's arithmetic**, and the honest reason is the
    /// units: in Krita the two are not commensurable at all. `p` is the dab's
    /// own opacity and `d` is a colour-mix weight that is then *squared* —
    /// `KisColorSmudgeStrategyBase` composites the paint over the already
    /// smudged dab at `d² × opacity`. A ratio of two quantities in different
    /// units is a heuristic by construction. What it does promise is monotone
    /// in both, scale-free, and exact at both ends — a deposit of zero is a
    /// pure blender and a pickup of zero an ordinary brush — which is as much
    /// as one number can say about two.
    ///
    /// Reproducing the composite faithfully is the obvious alternative. Written
    /// out it is `p(1 − d²·op) / (p(1 − d²·op) + d²·op)`, and it is rejected
    /// for two reasons rather than the one that first suggested itself. It
    /// needs the stroke opacity, which is a *third* number and not this
    /// function's to know. And it is a reading at full pressure: a colour rate
    /// is usually stated as a pressure curve with the recorded value its peak,
    /// so at `op = 1` it answers "no pickup at all" for every curve that
    /// reaches the top — the value at one end of a stroke, offered as the
    /// constant for the whole of it. The two do not merely differ near that
    /// end: at `p = 0.38`, `d = 0.84`, `op = 1` the composite is 0.137 and this
    /// is 0.311.
    ///
    /// Two zeroes mean a dab that puts nothing anywhere, which is not a state
    /// to carry into a mix: it answers `0.0`, the ordinary brush, rather than a
    /// division by zero. That case is reachable rather than hypothetical — a
    /// pickup rate of zero with the deposit switched off is exactly it — and
    /// answering the ordinary brush is a choice: the source paints nothing at
    /// all there, and Umber has no way to say "nothing" in a colour.
    ///
    /// **A rate that is not finite is read as absent**, i.e. as zero, because
    /// these come from `str::parse` over somebody's file and `"nan"` and
    /// `"inf"` both parse. It is stated because the direction is not obvious:
    /// an infinite *pickup* therefore reads as an ordinary brush rather than as
    /// a pure blender. Both readings are arbitrary for a value that cannot mean
    /// anything; what matters is that the dab pass is never handed a NaN, which
    /// would spread to every channel of every dab's colour.
    pub fn smudge_from_rates(pickup: f32, deposit: f32) -> f32 {
        let pickup = if pickup.is_finite() {
            pickup.max(0.0)
        } else {
            0.0
        };
        let deposit = if deposit.is_finite() {
            deposit.max(0.0)
        } else {
            0.0
        };
        let total = pickup + deposit;
        if total <= 0.0 {
            return 0.0;
        }
        (pickup / total).clamp(0.0, 1.0)
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

    /// The two ends are the whole point: a brush that lays down no paint is a
    /// pure blender, and one that lifts nothing is an ordinary brush. Anything
    /// between is the pickup's share.
    #[test]
    fn a_pair_of_rates_becomes_the_mix_between_them() {
        assert_eq!(Brush::smudge_from_rates(1.0, 0.0), 1.0);
        assert_eq!(Brush::smudge_from_rates(0.0, 1.0), 0.0);
        assert_eq!(Brush::smudge_from_rates(1.0, 1.0), 0.5);
        // Krita's "Paint Round Dry": lifts 0.38, lays down 0.84 — mostly paint
        // with a hint of what is under it. Reading the pickup alone made it a
        // brush that deposits barely a third of its own colour.
        let dry = Brush::smudge_from_rates(0.38, 0.84);
        assert!((dry - 0.311_47).abs() < 1e-4, "{dry}");
        assert!(dry < 0.38);
    }

    /// Scale-free, because only the ratio is expressible: the same brush stated
    /// out of one and out of ten has to arrive the same brush.
    #[test]
    fn only_the_ratio_of_two_rates_survives() {
        for scale in [0.01f32, 0.5, 1.0, 10.0] {
            assert!((Brush::smudge_from_rates(0.7 * scale, 0.3 * scale) - 0.7).abs() < 1e-5);
        }
    }

    /// A dab that neither lifts nor deposits is not a mix, and dividing by the
    /// sum of two zeroes would put a NaN into the colour of every dab.
    #[test]
    fn two_rates_of_nothing_are_an_ordinary_brush() {
        assert_eq!(Brush::smudge_from_rates(0.0, 0.0), 0.0);
        assert_eq!(Brush::smudge_from_rates(f32::NAN, f32::NAN), 0.0);
        assert_eq!(Brush::smudge_from_rates(-1.0, -1.0), 0.0);
        // A file that states one of them as nonsense still has to give a
        // number the dab pass can use, and the *direction* is pinned rather
        // than merely its finiteness: a non-finite rate is read as absent, so
        // an infinite pickup is an ordinary brush and not a pure blender.
        assert_eq!(Brush::smudge_from_rates(f32::INFINITY, 1.0), 0.0);
        assert_eq!(Brush::smudge_from_rates(1.0, f32::NAN), 1.0);
        assert_eq!(Brush::smudge_from_rates(f32::NEG_INFINITY, 0.0), 0.0);
    }

    #[test]
    fn step_never_reaches_zero() {
        // A zero step would spin the dab loop forever.
        let b = Brush {
            size: 1.0,
            spacing: 0.0,
            ..Default::default()
        };
        assert!(b.step_at(0.0, std::f32::consts::FRAC_PI_2) > 0.0);
    }

    /// **A chisel is spaced by the width it actually travels on**, which is the
    /// bug that made every marker in the library paint as a row of separate
    /// nib marks with gaps between them.
    ///
    /// The figures are Ramón Miranda's "Marker": a 56.5 px long axis at 5.46:1,
    /// held across the line, at a spacing of 0.25. The old rule measured
    /// against the long axis and asked for a 14.1 px step on a dab 10.35 px
    /// wide — a mark, a gap, a mark. It is now a quarter of the 10.35, and the
    /// stroke closes.
    #[test]
    fn a_chisel_is_spaced_by_the_width_it_travels_on() {
        let marker = Brush {
            size: 56.48749,
            dab_ratio: 5.46,
            dab_angle: 90.0,
            dab_angle_follows_stroke: true,
            spacing: 0.25,
            pressure_size: false,
            ..Default::default()
        };
        // Across the nib, where the ellipse's radius and its shadow agree.
        let across = marker.off_heading(Vec2::X);
        let short = marker.size / marker.dab_ratio;
        assert!(
            (marker.step_at(1.0, across) - short * 0.25).abs() < 1e-3,
            "got {}",
            marker.step_at(1.0, across)
        );
        // Which is a step well inside the dab rather than past it. The old
        // reading was 14.1 against a reach of 10.35.
        assert!(marker.step_at(1.0, across) < short);

        // Pulled *along* its long axis instead — a nib dragged sideways — the
        // dab really is 56.5 px wide and the step really is a quarter of that.
        // The rule is the dab's own geometry, not a special case for chisels.
        let along = Brush {
            dab_angle: 0.0,
            ..marker
        };
        assert!((along.step_at(1.0, along.off_heading(Vec2::X)) - marker.size * 0.25).abs() < 1e-3);

        // **And a round dab is untouched, at every heading.** `a == b` makes
        // the ellipse's support constant, so this is the identity for every
        // brush that has ever been round — which is 246 of the 258 shipped.
        let round = Brush {
            size: 40.0,
            dab_ratio: 1.0,
            spacing: 0.1,
            pressure_size: false,
            ..Default::default()
        };
        for eighth in 0..8 {
            let theta = eighth as f32 * std::f32::consts::FRAC_PI_4;
            assert!(
                (round.step_at(1.0, theta) - 40.0 * 0.1).abs() < 1e-4,
                "a round dab must step the same way whichever way it is walked"
            );
        }
    }

    /// A brush that does not follow the stroke is spaced by *which way the hand
    /// went*, and one that does is spaced by its own fixed offset.
    #[test]
    fn a_fixed_nib_is_measured_against_the_direction_of_travel() {
        let nib = Brush {
            size: 40.0,
            dab_ratio: 4.0,
            dab_angle: 0.0,
            dab_angle_follows_stroke: false,
            spacing: 0.25,
            pressure_size: false,
            ..Default::default()
        };
        // Dragged along its own long axis: the full 40 px is what it covers.
        let along = nib.step_at(1.0, nib.off_heading(Vec2::X));
        // Dragged across it: 10 px, so a quarter of that.
        let across = nib.step_at(1.0, nib.off_heading(Vec2::Y));
        assert!((along - 10.0).abs() < 1e-3, "got {along}");
        assert!((across - 2.5).abs() < 1e-3, "got {across}");

        // A brush that follows the stroke has one answer, because "follows"
        // means the offset is the same wherever the hand goes.
        let rake = Brush {
            dab_angle_follows_stroke: true,
            ..nib
        };
        assert_eq!(rake.off_heading(Vec2::X), rake.off_heading(Vec2::Y));
    }
}
