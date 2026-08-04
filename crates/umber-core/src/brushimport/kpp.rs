//! Krita paintop presets (`.kpp`).
//!
//! The single highest-value importer here: `.kpp` is what David Revoy,
//! Raghukamath and GDQuest all publish, so one reader unlocks three of the
//! packs `docs/brush-sources.md` lists.
//!
//! A `.kpp` is a **PNG** — the thumbnail you see in Krita's brush chooser —
//! carrying the settings in a text chunk keyed `preset`, usually `zTXt`
//! (deflated) and occasionally `tEXt`. The chunk holds XML:
//!
//! ```xml
//! <Preset embedded_resources="1" name="Pencil" paintopid="paintbrush">
//!   <resources>
//!     <resource name="bristle" filename="bristle.png" type="brushes">
//!       <![CDATA[iVBORw0KGgo…]]></resource>
//!   </resources>
//!   <param name="OpacityValue" type="string"><![CDATA[1]]></param>
//!   <param type="internal" name="PressureSize">true</param>
//!   <param name="brush_definition" type="string"><![CDATA[
//!     <Brush type="auto_brush" spacing="0.1" angle="0">
//!       <MaskGenerator type="circle" id="default" diameter="40"
//!                      ratio="0.65" hfade="0.89" vfade="0.89" spikes="2"/>
//!     </Brush>]]></param>
//! </Preset>
//! ```
//!
//! Three things about that are easy to get wrong:
//!
//! - **A `param` may or may not use CDATA.** `type="internal"` writes the value
//!   as element text; `type="string"` wraps it. Both are read here.
//! - **`Pressure<Name>` is *not* "pressure drives this".** It is Krita's
//!   historical spelling of "the `<Name>` dynamic is switched on"; which input
//!   drives it is in `<Name>Sensor`. Reading it the obvious way turns every
//!   speed- and angle-driven dynamic into a pressure one — and it gates the
//!   setting's **value** as well as its curve. Krita writes `ScatterValue` at
//!   its default of 1 into every preset whether or not scattering is on, so a
//!   reader that consults the value alone sprays 93 of the 119 presets in the
//!   fetched packs.
//! - **`angle` is in radians** in `brush_definition`, and in degrees nowhere.
//!
//! # Fade is hardness, not softness
//!
//! `MaskGenerator@hfade` is the fraction of the radius that stays **fully
//! opaque**, so `hfade="1"` is a hard-edged disc and `hfade="0"` the softest
//! dab the generator draws. That is the same quantity `dab.wgsl` calls
//! hardness, and it carries across unchanged.
//!
//! This read `1 - hfade` for a while, on the reasonable-sounding grounds that a
//! setting called "fade" ought to be softness. It is not, and reasoning was the
//! wrong way to settle it. A `.kpp` is a PNG whose image is *Krita's own
//! preview of the brush*, so the packs answer the question directly:
//!
//! | Preset | generator | `hfade` | Krita's preview |
//! |---|---|---|---|
//! | GDQuest "Ink Brush" | `default` | 1 | a crisp black line |
//! | Raghukamath "Inkbrush" | `default` | 1 | a crisp black line |
//! | GDQuest "Ink Rough" | `default` | 0.78 | crisp, faintly feathered |
//! | Raghukamath "Basic Render" | `default` | 0.67 | plainly soft |
//! | Deevad "Basic Oval Brush" | `default` | 0.89 | hard-edged |
//! | Deevad "Eraser Kneaded Soft" | `gauss` | 0 | a wide feathered fade |
//! | Deevad "Pencil H Sketch" | `gauss` | 0.15 | soft, faint sketch lines |
//!
//! Every one of those was imported inside out. Three mask generators share the
//! attribute and all three run the same way round: `default`
//! (`KisCircleMaskGenerator`) fades from `hfade` of the radius outwards,
//! `gauss` blurs by `1 - hfade`, and `soft` (`KisCurveMaskGenerator`) ignores
//! `hfade` entirely and states its falloff in `softness_curve`, which
//! [`hardness_from_curve`] reads.
//!
//! # The tip
//!
//! `brush_definition@type` is `auto_brush` — a generated ellipse, which Umber
//! draws exactly — or a *predefined* brush (`png_brush`, `gbr_brush`,
//! `svg_brush`) naming a file. A preset with `embedded_resources` carries that
//! file base64-encoded in `<resources>`; one without expects Krita's resource
//! database to have it, and in a `.bundle` it is in `brushes/`. When neither is
//! true the brush arrives round and [`KppPreset::missing_tip`] names what was
//! wanted, because a stamp brush silently painting round is the failure this
//! whole reader exists to avoid.
//!
//! **A Krita PNG brush is inverted relative to a `.gbr`**: white is no paint
//! and black is full, the opposite of [`TipMask`]'s convention. Reading one the
//! `.gbr` way gives a solid square with a hole in it.
//!
//! # What is dropped
//!
//! - **Every paint engine except `paintbrush` and `colorsmudge`.** `deformbrush`
//!   moves pixels around, `experimentbrush` fills an outline, `hairybrush`
//!   simulates bristles — these are *different programs*, not settings, and a
//!   round dab wearing their name would be pure invention. They are refused by
//!   name rather than approximated.
//! - **Masking brushes** — a second brush multiplied into the first.
//! - **Edge sharpening.** Krita's Sharpness thresholds the finished mask into a
//!   hard, aliased edge; `dab.wgsl` antialiases unconditionally and has nothing
//!   to switch off. It is the whole of a pixel-art brush, so it is named rather
//!   than ignored.
//! - **Paint thickness (impasto)**.
//! - **Mirrored dabs**, where Krita is actually asked for one: the option has
//!   an enable flag *and* a checkbox per axis, and with neither axis ticked it
//!   mirrors nothing however the sensor reads.
//! - **Square and star mask generators**, exactly as [`super::vbr`] drops them.
//! - **Brush-tip randomness and density**, which perturb the generated mask
//!   rather than the dab — above [`NEGLIGIBLE_MASK_NOISE`], below which they
//!   are a difference of two or three levels of alpha.
//! - **Flow build-up.** Krita composites each dab, so flow below 1 darkens
//!   where a stroke crosses itself. Umber takes a `max` of coverage and applies
//!   opacity once at commit — the wet-layer design in `CLAUDE.md` — so flow is
//!   folded into stroke opacity instead. Same trade, and the same reason, as
//!   `opaque_linearize` in [`super::mypaint`].
//! - **Auto-spacing.** `useAutoSpacing` asks Krita to derive spacing from the
//!   tip; the recorded `spacing` is used instead of guessing at the formula.
//! - **Sensors Umber has no input for**, which after the section below are
//!   `speed`, `fuzzystroke`, `tilt`, `time` and their relatives. A **rotation**
//!   driven by `ascension` (tilt direction) or `rotation` (barrel rotation) —
//!   neither of which a desktop pointer reports — is switched on and does
//!   nothing, so it is named too.
//!
//! # Sensors, and which of them Umber has an input for
//!
//! Krita states a dynamic as a *set* of sensors and multiplies their outputs
//! together. Three shapes of that reach here:
//!
//! - **Pressure** is stated on the brush itself, as one of Umber's four
//!   pressure curves. [`Preset::dynamic`] reads it.
//! - **`fuzzy`** is a fresh uniform draw per dab, which is exactly
//!   [`DabInput::Random`], so it becomes a [`Modulation`] on the same setting.
//!   Krita multiplies, so the sensor's *peak* lands on the setting's own value
//!   and the entry carries the fraction of that peak each draw asks for — the
//!   arrangement [`super::mypaint`]'s opacity path already uses, and the reason
//!   `Brush::size` and `Brush::opacity` stay "the value at the peak".
//! - **Everything else** is named. Two are worth the sentence: `speed`, whose
//!   axis is a fraction of a fixed maximum drawing speed where Umber's is
//!   MyPaint's log-speed scale, so a curve written for one cannot be placed on
//!   the other from anything the file says; and `fuzzystroke`, which is one
//!   draw for a whole stroke where Umber's random is one per dab.
//!
//! Two sensors are read for what they *mean* rather than as curves, and only
//! on rotation: a `drawingangle` one is "this dab follows the stroke" and a
//! `fuzzy` one is [`Brush::dab_angle_jitter`].
//!
//! **`<Name>UseSameCurve` decides which curve is in force**, and Krita's editor
//! leaves the other one in the file either way. Six options across four
//! presets in the fetched packs carry both; reading the shared one
//! regardless gave Deevad's "Eraser Kneaded Soft" an opacity ramp Krita never
//! applies to it. A sensor that states no curve of its own is the *identity*,
//! which is what Krita's own curve object is constructed as.
//!
//! # The paper texture
//!
//! Krita's texture option is a **pattern**, a scale, a levels pipeline over the
//! pattern's own greyscale, and a *blending mode* saying how the finished mask
//! meets the dab's alpha. Umber's grain is one line of that —
//! `mix(1.0, tile, strength)` multiplied into coverage — so the reading here is
//! decided by which of those four Umber holds exactly.
//!
//! - **The pattern comes across.** Twenty of the thirty-one textured presets in
//!   the fetched packs carry it base64-encoded in `Texture/Pattern/Pattern` and
//!   eleven name a file in the bundle's `patterns/`; [`from_kpp_in`] takes the
//!   same shape of resolver for both that it already takes for a tip.
//! - **The scale comes across**, as [`Brush::grain_scale`]: the tile is stored
//!   at its own resolution and the shader stretches it over `width × scale`
//!   document pixels, which is the picture Krita's own pre-resample produces
//!   with the resampling left to the sampler that was going to run anyway.
//! - **The levels pipeline is baked into the stored tile**, by
//!   [`TextureSpec::levels`]. Brightness, contrast, invert, the neutral point
//!   and the two cutoffs are each a pure function of one texel's grey, so the
//!   whole of it is a 256-entry table applied once at import — where a shader
//!   would pay for it on every fragment of every dab for ever, and `Brush`
//!   would have to grow six `Copy` fields and six controls to carry settings
//!   that describe a *picture* rather than a brush.
//! - **The blending mode is the one part that cannot be**, and it is why a
//!   texture is still named as a loss rather more often than not.
//!   `TexturingMode` 0 is Multiply, and Krita's own arithmetic there is
//!   `alpha × (mask × strength + (1 − strength))` — Umber's grain exactly, so
//!   those presets are reproduced rather than approximated. Every other mode
//!   subtracts, dodges, burns or thresholds; the largest of them is Subtract
//!   (`alpha − mask`), which no multiply can stand in for at all.
//!
//! Three further readings, each a field that decides whether another is read:
//!
//! - **`Texture/Pattern/Enabled`, and the prefix is the whole point.** This
//!   read `Texture/Enabled` for a while, which is a key Krita has never
//!   written, so the paper was dropped in silence from all 119 presets.
//! - **The strength**, `Texture/Strength/Value` or the older
//!   `Texture/Pattern/Strength`, because a texture at zero strength paints
//!   identically with and without paper and naming it would refuse a brush over
//!   a loss that is not one.
//! - **Whether that strength follows pressure.** Krita puts a curve on it;
//!   Umber's grain is one number for the stroke, so a curve that actually moves
//!   is named. A curve that does not move is Krita's editor leaving a default
//!   behind — the judgement [`Dynamic::from_samples`] already makes.
//!
//! The greyscale is [`crate::tip::grain_of`]'s rather than a second copy of
//! `qGray`, and the difference was measured rather than waved through: Umber
//! weights Rec. 709 where Krita weights Rec. 601, which on a **neutral** texel
//! is exactly the same number. Twenty-eight of the thirty-one patterns are
//! neutral in every texel and differ by nothing at all; the three that are
//! faintly tinted differ by at most 1.9 levels of 255, on one of them, and by
//! 0.3 on average.
//!
//! Compositing a transparent pattern over **white** rather than keeping its
//! alpha is Krita's own behaviour here and not a simplification:
//! `recalculateMask` takes that branch unless `preserveAlpha` is set, and
//! `KisTextureOption` sets it only for the Lightness and Gradient modes, which
//! are refused above with every other non-Multiply mode.
//!
//! ## Two settings that are read and deliberately not named
//!
//! - **`OffsetX`/`OffsetY` and `isRandomOffsetX`/`isRandomOffsetY`.** Krita
//!   shifts where the tile starts, and for twelve of the thirty-one it shifts
//!   it by a *fresh random amount every stroke*. Umber anchors the grain to the
//!   document origin. Naming that would be crying wolf twice over: a shift
//!   through a tiling texture changes which pits a mark lands in and nothing
//!   about the mark, and Krita's own answer is different on every stroke, so
//!   there is no "what the author saw" to have lost. What Umber's anchoring
//!   *does* change is that a second stroke lands in the same pits as the first
//!   — which is the whole point of the grain being document-anchored, and is
//!   what Krita gives with both randomisers off.
//! - **`curveMode`.** It selects how Krita's editor draws the strength curve,
//!   not how the curve is applied. Whether the curve applies at all is
//!   `Texture/Strength/UseCurve`, above.
//!
//! # Approximated rather than dropped
//!
//! - **The paint-deposit rate**, and it belongs in *this* section rather than
//!   the one above, which is a decision and not a bookkeeping detail. Krita's
//!   colour-smudge engine states the pickup and the deposit as two knobs
//!   (`SmudgeRate`, `ColorRate`) where Umber has one mix. Reading the pickup
//!   alone and naming the rest as a loss was wrong twice: it turned a brush
//!   laying down as much paint as it lifts into a pure blender, *and* the
//!   `dropped` entry then refused it from the shipped library — a brush kept
//!   out for a defect the reader had introduced. [`Brush::smudge_from_rates`]
//!   carries the ratio instead. **Two things are discarded and neither is
//!   named**, which is what makes this an approximation rather than a fix:
//!   - `SmudgeRate`'s *magnitude*. It is the dab's own opacity in Krita
//!     (`smearRateOpacity = s × opacity`), so it sits exactly where Umber's
//!     stroke opacity does and there *is* a slot for it — the same slot
//!     `FlowValue` is already folded into below. It is not folded in only
//!     because Umber's pickup is a trailing average rather than Krita's offset
//!     smear, so the two are not the same quantity to multiply; saying it has
//!     nowhere to go would be false.
//!   - `ColorRate`'s **pressure curve**, entirely. `has_foreign_sensor` is
//!     asked only about Size, Opacity and Scatter, so a `ColorRateSensor` is
//!     never examined at all. A brush whose author had it blending at a light
//!     touch and laying down paint at full pressure arrives as one flat mix.
//!
//!   **The consequence is that three brushes now ship on that approximation**,
//!   under Revoy's and GDquest's names, where `build-brush-library.rs` would
//!   previously have refused them — and that generator's whole rule is that
//!   nothing ships under an author's name that paints unlike their brush. It is
//!   defensible on the same terms as fan corners below, and the flattened curve
//!   is the part to weigh rather than the ratio. But it is a deliberate
//!   exception rather than a side effect, which is why it is written here.
//! - **Fan corners.** Krita adds extra dabs through a sharp corner so that a
//!   rake fans round it instead of jumping; Umber's dab turns with the heading
//!   and the heading turns at the corner. Six presets in the fetched packs ask
//!   for it. A stroke differs from Krita's only where it changes direction
//!   abruptly, and only for a dab or two — the same register as the HSL-in-HSV
//!   approximation in [`super::mypaint`], and stated for the same reason.
//! - **A random sensor on *scatter*.** Krita multiplies where Umber adds, so
//!   the entry carries `scatter × (factor − 1)` — exact for a brush whose
//!   scatter has no pressure sensor beside it, which is seven of the nine in
//!   the fetched packs, and at light pressure an under-estimate rather than a
//!   spray nobody asked for. Size and opacity need no such trade: a log offset
//!   and a factor are both products already.

use std::collections::BTreeMap;

use quick_xml::events::Event;

use crate::brush::{Brush, BrushMode};
use crate::curve::ResponseCurve;
use crate::dynamics::{DabInput, DabTarget, Modulation, Modulations};
use crate::preset::PresetError;
use crate::tip::TipMask;

use super::{gbr, gih};

/// Krita paint engines Umber can render something honest from.
///
/// `paintbrush` is the ordinary stamped dab and `colorsmudge` is the same thing
/// plus canvas pickup, which is exactly Umber's [`Brush::smudge`]. Everything
/// else in Krita is a separate engine.
const SUPPORTED_PAINTOPS: [&str; 2] = ["paintbrush", "colorsmudge"];

/// The largest preset chunk this will inflate.
///
/// The settings themselves are a few kilobytes, but a preset with
/// `embedded_resources` carries its tips base64-encoded *inside* them, and a
/// `.gih` tip runs to a quarter of a megabyte before encoding — Revoy's
/// "Bristle Thick Textured" inflates past two. Sixteen leaves room for a brush
/// built out of several while still refusing a decompression bomb.
const MAX_PRESET_BYTES: usize = 16 << 20;

/// How much of Krita's per-dab mask noise counts as none.
///
/// One part in a hundred, and it is a threshold rather than a zero for the
/// reason [`Brush::has_grain`]'s is: naming a difference nobody can see spends
/// the credibility of the list that names the ones they can, and — because the
/// shipped-library generator refuses anything that lost something — it keeps
/// out brushes that paint exactly like their originals.
///
/// A hundredth rather than a rounder fiftieth because the figure has to match
/// the argument it is made from: randomness scales a mask texel by a draw from
/// `1 - r ..= 1`, so a hundredth is at most 2.6 levels of 255 and a fiftieth is
/// 5.1 — and 5 levels is a difference somebody could find. Both presets in the
/// fetched packs that this admits state exactly `0.01`. See [`TipSpec::parse`].
const NEGLIGIBLE_MASK_NOISE: f32 = 0.01;

/// A decoded Krita preset.
#[derive(Clone, Debug)]
pub struct KppPreset {
    pub name: String,
    pub brush: Brush,
    /// The bitmap tip, when the preset carried one or the caller could find it.
    pub tip: Option<TipMask>,
    /// The file name of a predefined tip this preset wanted and nothing could
    /// supply. The brush still paints — round — and the caller says so.
    pub missing_tip: Option<String>,
    /// The paper this brush's grain bites through, where the texture was one
    /// Umber can reproduce and its pattern could be found.
    ///
    /// `None` and [`Brush::grain`] at zero go together, and deliberately: a
    /// strength kept without a tile would send [`crate::BrushPreset::paper`] to
    /// nothing and paint **flat**, which is the exact identity, where a
    /// strength kept with one of Umber's own tiles substituted would be a grain
    /// the author never chose. See `BrushPreset::paper`'s own note on that.
    pub paper: Option<TipMask>,
    pub dropped: Vec<&'static str>,
}

/// Decode a standalone `.kpp`.
///
/// A preset naming a predefined brush it does not embed arrives round; use
/// [`from_kpp_in`] when there is somewhere to look the file up, which for a
/// `.bundle` is its `brushes/` directory.
pub fn from_kpp(bytes: &[u8]) -> Result<KppPreset, PresetError> {
    from_kpp_in(bytes, &Sidecar::none())
}

/// Where a `.kpp` looks for the files it names rather than carries.
///
/// Two resolvers rather than one taking a directory, because a `.bundle` holds
/// its entries in memory and a loose preset's are on disk beside it — and
/// because the two directories are different (`brushes/` and `patterns/`), so a
/// single lookup would have to be told which kind of file it was after and
/// would then be two functions wearing one name.
pub struct Sidecar<'a> {
    /// A predefined brush tip, by the file name `brush_definition` gives.
    pub brushes: &'a dyn Fn(&str) -> Option<Vec<u8>>,
    /// A paper pattern, by the file name the texture option gives.
    pub patterns: &'a dyn Fn(&str) -> Option<Vec<u8>>,
}

impl Sidecar<'_> {
    /// Nothing beside the file. A preset that embeds everything it names reads
    /// identically either way; one that does not arrives round, or flat, and
    /// says so.
    pub fn none() -> Self {
        Self {
            brushes: &|_| None,
            patterns: &|_| None,
        }
    }
}

/// Decode a `.kpp`, resolving what it names but does not carry through `beside`.
pub fn from_kpp_in(bytes: &[u8], beside: &Sidecar<'_>) -> Result<KppPreset, PresetError> {
    let brushes = beside.brushes;
    let xml = preset_chunk(bytes)?;
    let preset = Preset::parse(&xml)?;

    let paintop = preset
        .params
        .get("paintop")
        .map(String::as_str)
        .unwrap_or(&preset.paintop);
    if !SUPPORTED_PAINTOPS.contains(&paintop) {
        return Err(PresetError::Malformed(
            None,
            format!(
                "`{paintop}` is one of Krita's other painting engines, not a brush setting — \
                 Umber has no equivalent to approximate it with"
            ),
        ));
    }
    let smudging = paintop == "colorsmudge";

    let definition = preset
        .params
        .get("brush_definition")
        .ok_or_else(|| malformed("it has no brush_definition"))?;
    let tip_spec = TipSpec::parse(definition)?;

    let mut dropped = Vec::new();
    dropped.extend(tip_spec.dropped.iter().copied());
    if preset.flag("MaskingBrush/Enabled") {
        dropped.push("masking brushes");
    }
    // The paper. See the module's own section: the pattern, the scale and the
    // levels pipeline all come across, and the *blending mode* is what decides
    // whether the brush is reproduced or named as a loss.
    let (paper, grain, grain_scale) = match TextureSpec::read(&preset) {
        None => (None, 0.0, Brush::default().grain_scale),
        Some(spec) => {
            let (paper, losses) = spec.resolve(beside.patterns);
            dropped.extend(losses);
            match paper {
                // Reading the strength and leaving the tile behind would be the
                // worst of both: `BrushPreset::paper` names nothing, the shader
                // paints flat, and the brush claims a grain it does not have.
                // They go together or neither does.
                Some(paper) => (Some(paper.tile), spec.strength, paper.scale),
                None => (None, 0.0, Brush::default().grain_scale),
            }
        }
    };
    // Mirroring has an enable flag *and* a checkbox per axis: Krita's
    // `KisMirrorOption::apply` guards the whole of its work on
    // `isChecked() && (horizontal || vertical)`, so with neither axis ticked
    // the option is on and mirrors nothing however the sensor reads. This is
    // therefore not one flag but three, and reading only the first names a loss
    // that did not happen. Same shape as `ScatterValue` above, and worth the
    // two extra reads: an import that cries wolf costs the losses that matter.
    let mirrors = preset.flag("PressureMirror")
        && (preset.flag("HorizontalMirrorEnabled") || preset.flag("VerticalMirrorEnabled"));
    if mirrors {
        dropped.push("mirrored dabs");
    }
    if preset.flag("PressurePaintThickness") {
        dropped.push("paint thickness");
    }
    // Krita's Sharpness thresholds the finished mask into a hard, aliased edge.
    // That is not a hardness — `dab.wgsl` antialiases unconditionally, sized
    // from the dab's short axis — and it is the whole of a pixel-art brush:
    // GDQuest's "PixelArt OnePixel" is a one-pixel dab that is *only* one pixel
    // because of this. Naming it is what keeps such a brush out of the shipped
    // library rather than shipping a soft blob under its author's name.
    if preset.flag("PressureSharpness") && preset.number("SharpnessValue").is_some_and(|v| v > 0.0)
    {
        dropped.push("edge sharpening");
    }

    // --- the tip ------------------------------------------------------------
    let mut missing_tip = None;
    let mut tip = None;
    if let Some(file) = &tip_spec.file {
        let raw = preset
            .resources
            .get(file.as_str())
            .cloned()
            .or_else(|| brushes(file));
        match raw {
            Some(raw) => {
                let decoded = decode_tip(file, &raw)?;
                for loss in decoded.dropped {
                    if !dropped.contains(&loss) {
                        dropped.push(loss);
                    }
                }
                tip = Some(decoded.mask);
            }
            None => missing_tip = Some(file.clone()),
        }
    }

    // --- size ---------------------------------------------------------------
    //
    // A generated brush states its diameter; a predefined one is its bitmap
    // scaled. Either way Krita's size dynamic *multiplies* that, so the peak of
    // the size curve is what Umber's `size` has to be — `radius_at` then
    // reproduces Krita at every sample point.
    let base_size = match (&tip, tip_spec.diameter) {
        (Some(mask), _) => mask.width().max(mask.height()) as f32 * tip_spec.scale,
        (None, Some(diameter)) => diameter,
        // A predefined tip that could not be found: its scale is a multiple of
        // a picture nobody has. Umber's default size is a better answer than
        // one derived from nothing.
        (None, None) => Brush::default().size,
    };

    let size = preset.dynamic("Size");
    let opacity = preset.dynamic("Opacity");
    let scatter = preset.dynamic("Scatter");

    // Krita drives a setting from several sensors at once and multiplies their
    // outputs together. Pressure is the one Umber states on the brush itself;
    // the rest are modulation-table entries, and each one's *peak* multiplies
    // the setting's own value exactly as the pressure curve's does.
    let mut mods: Vec<Modulation> = Vec::new();
    let size_extras = preset.extras("Size");
    let opacity_extras = preset.extras("Opacity");
    let scatter_extras = preset.extras("Scatter");

    let default = Brush::default();
    let (min_size_ratio, size_curve, pressure_size) = size.split(default.min_size_ratio.max(0.01));

    // Krita's opacity and flow multiply: opacity is the stroke's and flow is
    // the dab's. Umber applies opacity once at commit, so the two collapse into
    // one number — see the module docs for why that is the honest trade.
    let flow = preset.number("FlowValue").unwrap_or(1.0).clamp(0.0, 1.0);
    let opacity_peak = preset.number("OpacityValue").unwrap_or(1.0).clamp(0.0, 1.0);

    // --- the rest -----------------------------------------------------------
    let mode = if preset.flag("EraserMode")
        || preset
            .params
            .get("CompositeOp")
            .is_some_and(|op| op == "erase")
    {
        BrushMode::Erase
    } else {
        BrushMode::Paint
    };

    // Krita has spelled the airbrush option two ways. `PaintOpSettings/` is the
    // older one and `AirbrushOption/` the current; both are in the fetched
    // packs — 45 presets use the first, 52 the second — so knowing only one
    // silently imports an airbrush as a distance-driven brush. GDQuest's
    // "Airbrush", which asks for 1000 dabs a second, was exactly that.
    let airbrushing =
        preset.flag("PaintOpSettings/isAirbrushing") || preset.flag("AirbrushOption/isAirbrushing");
    let dabs_per_second = if airbrushing {
        preset
            .number("PaintOpSettings/rate")
            .or_else(|| preset.number("AirbrushOption/rate"))
            .unwrap_or(0.0)
            .clamp(0.0, 300.0)
    } else {
        0.0
    };

    // Krita spells "this dab turns to follow the stroke" as a `drawingangle`
    // sensor on rotation, and "point it anywhere" as a `fuzzy` one. Neither is
    // a curve; both are what the dab *is*. A rake read as a nib is immediately
    // visible in a curve, which is why they are worth this much attention.
    //
    // Every sensor the option names, not the first: Krita also writes a
    // *compound* `<params id="sensorslist">` with a `<ChildSensor>` per input,
    // and five presets in the fetched packs rotate that way. Reading the first
    // id called those "sensorslist", matched neither branch, and imported a
    // brush that turns every stamp as one that lays them all the same way up —
    // which for a bitmap tip is a comb.
    let rotation = preset.sensor_ids("Rotation");
    let rotates = preset.flag("PressureRotation");
    let dab_angle_follows_stroke = rotates && rotation.contains(&"drawingangle");
    let dab_angle_jitter = if rotates && rotation.contains(&"fuzzy") {
        360.0
    } else {
        0.0
    };
    // The rake's lean. `angleOffset` is stated in degrees and Krita adds it to
    // the drawing angle *inside the sensor* —
    // `0.5 + drawingAngle / 2π + angleOffset / 360` — so it goes through
    // exactly the transformation the heading itself does. Umber composes the
    // same two terms the same way, `angle = heading + dab_angle`, so the offset
    // carries across as degrees on `dab_angle` with the sign the heading
    // already has. Four presets in the fetched packs lean their rake between
    // 92° and 139°, and without this every one of them arrives dragging its
    // bristles along the stroke instead of across it.
    let angle_offset = if dab_angle_follows_stroke {
        preset
            .sensor_number("Rotation", "angleOffset")
            .unwrap_or(0.0)
    } else {
        0.0
    };
    // A rotation driven by something else is switched on and does nothing.
    // `ascension` is tilt direction and `rotation` is barrel rotation, neither
    // of which any desktop pointer reports here (see the pressure section of
    // `CLAUDE.md`); `pressure` would need an `Angle` modulation this reader
    // does not build. Named for the same reason the other foreign sensors are:
    // those brushes feel dead rather than wrong, and a stamp that was meant to
    // turn is the most visible case of it there is.
    if rotates && !dab_angle_follows_stroke && dab_angle_jitter == 0.0 {
        dropped.push("dab rotation driven by tilt, pen rotation or pressure");
    }

    // A smudging brush picks colour up off the canvas. Krita states the pickup
    // and the deposit separately — `SmudgeRate` is how much of the canvas a dab
    // lifts, `ColorRate` how much fresh paint it lays down — and Umber has one
    // mix between the palette and what was found. That is not a feature it
    // lacks: `1 - Brush::smudge` *is* a deposit rate, so what carries across is
    // the ratio of the two, through `Brush::smudge_from_rates`.
    //
    // **`PressureColorRate` is the enable flag**, the rule the module docs open
    // with, and reading the value without it is the `ScatterValue` bug again.
    // Krita leaves `ColorRateValue` in the file with Color Rate switched off,
    // and `kis_colorsmudgeop.cpp`'s `paintAt` is explicit about what that means:
    //
    //     colorRate = m_colorRateOption.isChecked() ? …value… : 0.0;
    //
    // so a clear flag is a deposit of **exactly zero** — a pure blender,
    // whatever the pickup rate says. Hence one rule and not two: the flag
    // decides the deposit, and the deposit and the pickup then go through the
    // same reduction. Branching on the flag *around* the reduction is the same
    // bug in a new place, and it shipped once: it left `smudge` at the pickup
    // rate, so "Blend Smoky" (0.57) deposited 43% palette colour where Krita
    // deposits none.
    //
    // The seven presets in the fetched packs with the flag clear are exactly
    // the ones their authors called Blend, Blender or Smear — nothing else is
    // in that group and none of them is outside it. (The converse is weaker and
    // is not claimed: the twelve with it set are mostly Paint and OilPaint, but
    // "Watercolor Sponge" is among them — the author's own spelling, so that it
    // can be searched for.)
    let (smudge, smudge_radius) = if smudging {
        // The same `paintAt` gives the pickup its own flag and its own
        // fallback, and it is *not* the deposit's:
        //
        //     smudgeRate = m_smudgeRateOption.isChecked() ? …value… : 1.0;
        //
        // — a brush with the option off lifts the canvas whole rather than not
        // at all. No preset in the fetched packs has it clear, so this changes
        // nothing shipped; it is here because the rule this module opens with
        // is that a value is read through the flag beside it, and a reader that
        // applies it to one of three neighbouring settings has not applied it.
        let pickup = if preset.flag("PressureSmudgeRate") {
            preset
                .number("SmudgeRateValue")
                .unwrap_or(1.0)
                .clamp(0.0, 1.0)
        } else {
            1.0
        };
        let deposit = if preset.flag("PressureColorRate") {
            preset
                .number("ColorRateValue")
                .unwrap_or(1.0)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        // KNOWN WRONG, and left alone deliberately rather than overlooked.
        // `SmudgeRadiusValue` has an enable flag too — `smudgeRadiusPortion =
        // isChecked() ? …value… : 0.0` — and **14 of the 19 colour-smudge
        // presets in the fetched packs have it clear**, carrying leftovers of
        // 297.82, 3, 0.41 and 0.0041 that this clamp turns into live radii of
        // 8.0, 3.0, 0.41 and 0.25. That is the `ScatterValue` bug at scale.
        //
        // It is not repaired here because the repair needs a number this cannot
        // supply honestly: Krita's portion is a fraction of the brush size and
        // Umber's `smudge_radius` a multiple of the dab radius, so what a
        // portion of zero maps to — the dab itself, most likely `1.0`, which is
        // `Brush::default().smudge_radius` — is a reading of Krita's sampling
        // geometry that nobody here has checked against a running Krita. Fixing
        // it changes the pickup radius of fourteen brushes, several of them
        // shipped, so it wants that check first. Guessing would be the thing
        // this module refuses everywhere else.
        (
            Brush::smudge_from_rates(pickup, deposit),
            preset
                .number("SmudgeRadiusValue")
                .unwrap_or(1.0)
                .clamp(0.25, 8.0),
        )
    } else {
        (0.0, default.smudge_radius)
    };

    let (min_scatter_ratio, scatter_curve, pressure_scatter) = scatter.split(0.0);
    let (_, opacity_curve, pressure_opacity) = opacity.split(0.0);

    // --- the other inputs -----------------------------------------------------
    //
    // Each entry's units are its target's, and the three differ — see
    // `Modulated`. Size is a **log** offset, because `radius_logarithmic` is a
    // log and a factor there composes by multiplication in pixels, which is
    // precisely what Krita's own sensor product is. Opacity is a factor
    // already. Scatter is the odd one: Umber adds it in dab radii where Krita
    // multiplies, so the entry carries `scatter × (factor − 1)` — exact for a
    // brush whose scatter has no pressure sensor beside it, which is seven of
    // the nine in the fetched packs, and an under-estimate at light pressure
    // for the other two rather than a spray they never asked for.
    let mut size_scale = 1.0f32;
    for e in &size_extras {
        size_scale *= e.peak;
        // A factor of zero is a dab of no size at all; `radius_at` floors the
        // radius at half a pixel anyway, so the log is floored to match rather
        // than being allowed to reach negative infinity.
        let values = e.factors.map(|f| f.max(0.01).ln());
        mods.extend(entry(DabTarget::Size, e.input, values));
    }
    let mut opacity_scale = 1.0f32;
    for e in &opacity_extras {
        opacity_scale *= e.peak;
        mods.extend(entry(DabTarget::Opacity, e.input, e.factors));
    }
    let scatter_scale: f32 = scatter_extras.iter().map(|e| e.peak).product();
    let scatter_value = if preset.flag("PressureScatter") {
        scatter.peak * scatter_scale * preset.number("ScatterValue").unwrap_or(0.0)
    } else {
        0.0
    };
    for e in &scatter_extras {
        let values = e.factors.map(|f| scatter_value * (f - 1.0));
        mods.extend(entry(DabTarget::Scatter, e.input, values));
    }

    // A dynamic that is switched on and whose sensor reached nothing arrives as
    // a constant. Worth naming: those brushes are the ones that will feel dead
    // rather than wrong. Two sentences, because there are two causes — see
    // `Preset::dropped_sensor`.
    for name in ["Size", "Opacity", "Scatter"] {
        if let Some(loss) = preset.dropped_sensor(name)
            && !dropped.contains(&loss)
        {
            dropped.push(loss);
        }
    }

    let spacing = tip_spec.spacing.unwrap_or(default.spacing);
    // Whether a `max` stroke of this stamp is the mark its author drew or a
    // fraction of it. Krita composites every dab, so a sparse tip builds to
    // solid along a stroke where Umber's `max` caps it at the mask's brightest
    // texel — see `crate::tip::stroke_coverage`. Measured, exactly as
    // [`super::gbr::to_brush`] and [`super::abr::to_brush`] measure it, and for
    // the same reason: a photographic stamp looks dense and is not. A generated
    // dab is solid, so this is the identity for every brush without a tip.
    let build_up = tip
        .as_ref()
        .is_some_and(|mask| crate::tip::stroke_coverage(mask, spacing).needs_build_up());

    let brush = Brush {
        size: (base_size * size.peak * size_scale).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE),
        min_size_ratio,
        size_curve,
        pressure_size,
        build_up,
        // A bitmap tip *replaces* the procedural falloff, so hardness has
        // nothing left to shape and the file states none for a predefined
        // brush either.
        hardness: tip_spec.hardness.unwrap_or(default.hardness),
        opacity: (opacity_peak * flow * opacity.peak * opacity_scale).clamp(0.0, 1.0),
        opacity_curve,
        pressure_opacity,
        spacing,
        mode,
        dabs_per_second,
        dab_ratio: tip_spec.dab_ratio,
        dab_angle: (tip_spec.angle + angle_offset).rem_euclid(360.0),
        dab_angle_follows_stroke,
        dab_angle_jitter,
        // `ScatterValue` only means anything when Krita's scatter option is
        // switched on: `KisScatterOption::apply` returns the unscattered
        // position when `!isChecked()`, and `isChecked()` is `PressureScatter`
        // — the same "`Pressure<Name>` is the enable flag" rule the module docs
        // open with, which was being applied to the *curve* and not to the
        // value. Krita leaves `ScatterValue` at its default of 1 in a preset
        // that never scatters, so reading it unconditionally turned twenty of
        // the twenty-one shipped Krita brushes into sprays.
        scatter: scatter_value,
        min_scatter_ratio,
        scatter_curve,
        pressure_scatter,
        smudge,
        smudge_radius,
        grain,
        grain_scale,
        // Heaviest first, which is `mypaint`'s rule and is currently inert
        // here: `dab_input` maps one id to a non-pressure input and three
        // options are read, so this can never exceed three entries against
        // `Modulations::MAX`'s twelve. It is kept rather than dropped because
        // the cap becomes reachable the moment a fourth option or a second
        // input is added, and a table that silently loses its widest entry is
        // exactly the failure the ordering exists to prevent. Nothing is
        // dropped for faintness here either — `entry` already refuses anything
        // `Modulations::push` would.
        modulations: {
            mods.sort_by(|a, b| b.weight().total_cmp(&a.weight()));
            mods.into_iter().collect::<Modulations>()
        },
        ..default
    };

    Ok(KppPreset {
        name: preset.name,
        brush,
        tip,
        missing_tip,
        paper,
        dropped,
    })
}

/// What reading this `.kpp` will throw away.
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    let Ok(preset) = from_kpp(bytes) else {
        return Vec::new();
    };
    let mut out = preset.dropped;
    if preset.missing_tip.is_some() {
        out.push(MISSING_TIP);
    }
    out
}

/// A preset naming a bitmap tip nobody has. The brush arrives round, which is
/// most of a stamp brush gone; both this reader and [`super::bundle`] can
/// produce it, so the sentence is written once.
pub const MISSING_TIP: &str = "bitmap tips stored outside the file";

// ---------------------------------------------------------------------------
// The PNG wrapper
// ---------------------------------------------------------------------------

/// Pull the `preset` text chunk out of a `.kpp`.
///
/// **All three of PNG's text chunks turn up in the wild**, and a reader that
/// knows only one silently rejects a quarter of a real bundle: of Revoy's 46
/// presets, 33 use `zTXt`, 11 use `iTXt` and the remainder `tEXt`. There is no
/// pattern to it — it is whichever Krita felt like on the day — so all three
/// are looked at.
///
/// The two Latin-1 chunks need a further step. `png` hands them back one byte
/// per `char`, because that is what the specification says they hold; Krita
/// writes UTF-8 into them regardless, so the chars go back to bytes and are
/// re-read. Skipping that turns "Aérographe" into "AÃ©rographe" in the picker.
/// `iTXt` really is UTF-8 and must *not* be put through the same step.
fn preset_chunk(bytes: &[u8]) -> Result<String, PresetError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().map_err(|e| {
        PresetError::Malformed(
            None,
            format!("a .kpp is a PNG and this one will not open ({e})"),
        )
    })?;
    let info = reader.info();
    let failed =
        |e| PresetError::Malformed(None, format!("its settings would not decompress ({e})"));

    if let Some(chunk) = info.utf8_text.iter().find(|c| c.keyword == "preset") {
        let mut chunk = chunk.clone();
        chunk
            .decompress_text_with_limit(MAX_PRESET_BYTES)
            .map_err(failed)?;
        return chunk.get_text().map_err(failed);
    }

    let latin1 = if let Some(chunk) = info
        .compressed_latin1_text
        .iter()
        .find(|c| c.keyword == "preset")
    {
        let mut chunk = chunk.clone();
        chunk
            .decompress_text_with_limit(MAX_PRESET_BYTES)
            .map_err(failed)?;
        chunk.get_text().map_err(failed)?
    } else if let Some(chunk) = info
        .uncompressed_latin1_text
        .iter()
        .find(|c| c.keyword == "preset")
    {
        chunk.text.clone()
    } else {
        return Err(malformed(
            "the PNG carries no `preset` chunk, so it is a picture and not a brush",
        ));
    };

    // The chunk's bytes, recovered: `decode_iso_8859_1` gave one char per byte,
    // so this cannot lose anything. If they are valid UTF-8 the writer was
    // Krita smuggling UTF-8 through a Latin-1 chunk; if they are not, it was a
    // writer that meant Latin-1, and the string `png` built is already right.
    // Trying UTF-8 *first* is what matters: the other order mangles every
    // accented brush name in the Revoy bundle.
    let raw: Vec<u8> = latin1.chars().map(|c| c as u8).collect();
    Ok(String::from_utf8(raw).unwrap_or(latin1))
}

// ---------------------------------------------------------------------------
// The settings XML
// ---------------------------------------------------------------------------

struct Preset {
    name: String,
    paintop: String,
    params: BTreeMap<String, String>,
    /// Embedded files, by the name `brush_definition` refers to them by.
    resources: BTreeMap<String, Vec<u8>>,
}

impl Preset {
    fn parse(xml: &str) -> Result<Self, PresetError> {
        let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
        reader.config_mut().trim_text(false);

        let mut out = Self {
            name: String::new(),
            paintop: String::new(),
            params: BTreeMap::new(),
            resources: BTreeMap::new(),
        };
        // The element currently collecting text, and what to file it under.
        let mut collecting: Option<(bool, String)> = None;
        let mut text = String::new();

        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    let attrs = attributes(&e)?;
                    match e.local_name().as_ref() {
                        b"Preset" => {
                            out.name = attrs.get("name").cloned().unwrap_or_default();
                            out.paintop = attrs.get("paintopid").cloned().unwrap_or_default();
                        }
                        b"param" => {
                            text.clear();
                            collecting =
                                Some((false, attrs.get("name").cloned().unwrap_or_default()));
                        }
                        b"resource" => {
                            text.clear();
                            // Referred to by file name, which is what
                            // `brush_definition` records.
                            collecting = Some((
                                true,
                                attrs
                                    .get("filename")
                                    .or_else(|| attrs.get("name"))
                                    .cloned()
                                    .unwrap_or_default(),
                            ));
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(e)) => {
                    if collecting.is_some() {
                        text.push_str(&String::from_utf8_lossy(&e));
                    }
                }
                // `type="string"` wraps its value in CDATA and `type="internal"`
                // does not, so both events feed the same buffer. Missing either
                // one loses half the settings in every file.
                Ok(Event::CData(e)) => {
                    if collecting.is_some() {
                        text.push_str(&String::from_utf8_lossy(&e));
                    }
                }
                Ok(Event::End(e)) => {
                    let ended = matches!(e.local_name().as_ref(), b"param" | b"resource");
                    if let Some((resource, key)) = collecting.take()
                        && ended
                    {
                        if resource {
                            if let Some(data) = base64(&text) {
                                out.resources.insert(key, data);
                            }
                        } else {
                            out.params.insert(key, text.trim().to_string());
                        }
                    } else if !ended {
                        // A `</resources>` or `</Preset>`: nothing was open.
                    }
                    text.clear();
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(PresetError::Malformed(
                        None,
                        format!("its settings are not readable XML ({e})"),
                    ));
                }
                _ => {}
            }
        }

        if out.params.is_empty() {
            return Err(malformed("its `preset` chunk holds no settings"));
        }
        Ok(out)
    }

    fn number(&self, key: &str) -> Option<f32> {
        self.params.get(key)?.parse().ok()
    }

    fn flag(&self, key: &str) -> bool {
        self.params.get(key).is_some_and(|v| v == "true")
    }

    /// A flag whose *absence* does not mean `false`.
    ///
    /// Krita reads `<Name>UseSameCurve` with a default of `true`, so a preset
    /// that states a `commonCurve` and nothing beside it means that curve to
    /// apply. Every preset in the fetched packs writes the flag, so this is
    /// about what a hand-written or older file means rather than about them.
    fn flag_or(&self, key: &str, default: bool) -> bool {
        self.params.get(key).map_or(default, |v| v == "true")
    }

    /// **Every** id the sensor names, in the order it names them.
    ///
    /// Usually there is one — `<params id="pressure">`. Krita also writes a
    /// *compound* sensor, `<params id="sensorslist">` holding a
    /// `<ChildSensor id="…">` per input, whose first id is the wrapper's own
    /// and matches nothing. Seven presets in the fetched packs drive their
    /// rotation that way and eleven drive Size, Opacity or Scatter that way.
    ///
    /// **This is the one door**, and it is why there is no longer a
    /// `sensor_id` beside it: a reader that takes the first id it finds is
    /// right about every simple preset and silently wrong about every compound
    /// one, which is the shape of every fault this module has had.
    fn sensor_ids(&self, name: &str) -> Vec<&str> {
        let Some(sensor) = self.params.get(&format!("{name}Sensor")) else {
            return Vec::new();
        };
        sensor
            .match_indices("id=\"")
            .filter_map(|(at, _)| {
                let rest = &sensor[at + 4..];
                rest.find('"').map(|end| &rest[..end])
            })
            .collect()
    }

    /// A numeric attribute off a sensor, such as `angleOffset` on
    /// `drawingangle`. The sensor is a scrap of XML inside a parameter, and the
    /// attribute is read the same way its id is.
    fn sensor_number(&self, name: &str, attribute: &str) -> Option<f32> {
        let sensor = self.params.get(&format!("{name}Sensor"))?;
        let key = format!("{attribute}=\"");
        let at = sensor.find(&key)? + key.len();
        let end = sensor[at..].find('"')?;
        sensor[at..at + end].parse().ok()
    }

    /// The curve Krita applies to one *named* sensor of one option.
    ///
    /// `<Name>UseSameCurve` decides between the two: with it set the file
    /// states one `<Name>commonCurve` for every sensor, and without it each
    /// sensor carries its own `<curve>` and the shared one — which Krita's
    /// editor leaves behind whether or not it is in force — is not read. Six
    /// options across four presets in the fetched packs carry both, so
    /// consulting the shared curve unconditionally is how Deevad's "Eraser
    /// Kneaded Soft" came out with an opacity ramp Krita never applies to it.
    ///
    /// A sensor with no `<curve>` of its own is linear, which is what Krita's
    /// own curve object is constructed as and what its editor draws for it.
    ///
    /// The search is bounded at the *next* `id="`, which is the whole reason
    /// this cannot simply take the first `<curve>` in the blob: a compound
    /// sensor lists a `<ChildSensor>` per input, and the pressure child is
    /// routinely the one written without a curve — so an unbounded search
    /// hands pressure whatever the next child happens to state.
    ///
    /// With the flag set and no shared curve in the file this falls through to
    /// the sensor's own, which is deliberate: Krita only started writing
    /// `commonCurve` alongside `curveMode`, and every preset in the fetched
    /// packs that omits one omits both. Falling through recovers the curve
    /// those files do carry, where Krita's own default would be the diagonal.
    fn sensor_curve(&self, name: &str, sensor: &str) -> Option<Vec<(f32, f32)>> {
        if self.flag_or(&format!("{name}UseSameCurve"), true)
            && let Some(points) = self
                .params
                .get(&format!("{name}commonCurve"))
                .and_then(|text| curve_points(text))
        {
            return Some(points);
        }
        let blob = self.params.get(&format!("{name}Sensor"))?;
        let key = format!("id=\"{sensor}\"");
        let rest = &blob[blob.find(&key)? + key.len()..];
        let rest = rest.find("id=\"").map_or(rest, |next| &rest[..next]);
        let at = rest.find("<curve>")? + 7;
        let end = rest[at..].find("</curve>")?;
        curve_points(&rest[at..at + end])
    }

    /// One of Krita's curve-driven settings as one *input* sees it.
    ///
    /// `Pressure<Name>` is the *enabled* flag — see the module docs — and
    /// `<Name>UseCurve` says whether the curve is in force at all. A sensor the
    /// option does not name contributes nothing, which is what makes
    /// [`Dynamic::flat`] the identity here: Krita multiplies its sensors
    /// together, so one is a factor of 1.
    fn sensor_dynamic(&self, name: &str, sensor: &str) -> Dynamic {
        let live = self.flag(&format!("Pressure{name}"))
            && self.flag(&format!("{name}UseCurve"))
            && self.sensor_ids(name).contains(&sensor);
        if !live {
            return Dynamic::flat();
        }
        // A sensor the option names but that states no curve is the *identity*,
        // not a dynamic that does nothing: Krita's curve object is constructed
        // as the diagonal and only `fromXML` finding a `<curve>` child replaces
        // it. Reading the absence as flat is how Deevad's "Eraser Kneaded Soft"
        // — whose pressure sensor is a bare `<params id="pressure"/>` — would
        // have arrived with no pressure ramp at all once the shared curve
        // beside it stopped being read.
        Dynamic::from_samples(
            self.sensor_curve(name, sensor)
                .unwrap_or_else(|| vec![(0.0, 0.0), (1.0, 1.0)]),
        )
    }

    /// The pressure half of a dynamic, which is the half Umber states as a
    /// curve on the brush itself.
    ///
    /// Read through [`Preset::sensor_ids`] rather than off the first id in the
    /// blob, and that is not a refinement: Krita's compound
    /// `<params id="sensorslist">` reports the *wrapper's* own name, which is
    /// not `pressure`, so every preset that drives a setting by pressure **and**
    /// something else lost its pressure curve entirely. Eleven presets in the
    /// fetched packs do that, and the same fault refused all eleven, which is
    /// the only reason none of them shipped wrong.
    fn dynamic(&self, name: &str) -> Dynamic {
        self.sensor_dynamic(name, "pressure")
    }

    /// The non-pressure sensors of a dynamic, as Umber's modulation table
    /// states them.
    ///
    /// Krita **multiplies** its sensors together, which is why the peak belongs
    /// in the setting's own value and the entry carries only the fraction of
    /// that peak each input asks for — exactly what [`super::mypaint`]'s
    /// opacity path does, and for the same reason: `Brush::size` and
    /// `Brush::opacity` are the value at the peak, not the value now.
    ///
    /// **A sensor whose curve never moves still has a peak, and that peak still
    /// scales the setting.** Only [`Dynamic::live`] decides whether an *entry*
    /// is worth a slot; gating the whole `Extra` on it would throw the peak away
    /// with it, so a `fuzzy` sensor sitting flat at 0.3 — which Krita renders as
    /// a third of the size, every dab — would import at full size and be named
    /// nowhere. The pressure half has never had that hole: `size.peak` is read
    /// off the struct whether or not the curve varies.
    fn extras(&self, name: &str) -> Vec<Extra> {
        self.sensor_ids(name)
            .into_iter()
            .filter_map(|id| {
                let input = dab_input(id).filter(|i| *i != DabInput::Pressure)?;
                let d = self.sensor_dynamic(name, id);
                if d.peak <= 0.0 {
                    return None;
                }
                Some(Extra {
                    input,
                    peak: d.peak,
                    // A curve that never moves normalises to all ones, which is
                    // the exact identity and which `entry` then refuses as too
                    // faint for a slot — so the peak lands and nothing else does.
                    factors: d.samples.map(|s| (s / d.peak).clamp(0.0, 1.0)),
                })
            })
            .collect()
    }

    /// What to say about a sensor of this dynamic whose contribution went
    /// nowhere, if there is one.
    ///
    /// Derived from the same two facts [`Preset::extras`] reads rather than
    /// from a second scan, so a sensor cannot be carried and named at the same
    /// time — nor silently dropped. **The two causes get two sentences**,
    /// because they are not the same loss and one message covering both names
    /// a cause that is not the cause: `fuzzy` reaching a curve that is switched
    /// off is an input Umber demonstrably *can* produce. `FOREIGN_INPUT` wins
    /// where both apply, for the reason the library generator asks what was
    /// dropped before it asks about the mask — it is the more informative of
    /// the two.
    ///
    /// **Pressure is exempt from the second clause, and deliberately.**
    /// `<Name>UseCurve` being off is read by this module as "no dynamic", where
    /// Krita reads it as "the sensor, applied straight" — a linear ramp. 34 of
    /// the fetched presets' Opacity and Scatter options have it off, so if that
    /// reading is wrong it is a real loss on all of them; it is also how this
    /// reader has behaved since before any of this, it is not a question the
    /// packs can settle the way they settled `hfade`, and putting a sentence on
    /// it here would be claiming a certainty nobody has. Named in
    /// `docs/brushes.md` as the open question it is instead.
    fn dropped_sensor(&self, name: &str) -> Option<&'static str> {
        if !self.flag(&format!("Pressure{name}")) {
            return None;
        }
        let curved = self.flag(&format!("{name}UseCurve"));
        let mut unread = None;
        for id in self.sensor_ids(name) {
            // The wrapper of a compound sensor is not an input of its own, and
            // pressure is the half stated on the brush rather than here.
            if matches!(id, "sensorslist" | "pressure") {
                continue;
            }
            if dab_input(id).is_none() {
                return Some(FOREIGN_INPUT);
            }
            if !curved {
                unread = Some(UNREAD_CURVE);
            }
        }
        unread
    }
}

/// A dynamic driven by something Umber has no input for — `speed`,
/// `fuzzystroke`, tilt and their relatives. See [`dab_input`].
const FOREIGN_INPUT: &str = "a dynamic driven by an input Umber cannot produce";

/// A dynamic whose input Umber *has*, reaching a curve Krita switched off. This
/// reader takes that to mean the dynamic does nothing, so the setting arrives
/// at one value where Krita varies it.
const UNREAD_CURVE: &str = "a dynamic that varies in Krita and arrives constant here";

/// One non-pressure sensor of one Krita dynamic.
struct Extra {
    input: DabInput,
    /// The sensor's greatest output, which multiplies the setting's own value.
    peak: f32,
    /// The five outputs, normalised so the largest is exactly 1.
    factors: [f32; ResponseCurve::N],
}

/// Which of Umber's dab inputs a Krita sensor id is, where there is one.
///
/// Two of Krita's sensors are read elsewhere for what they *mean* rather than
/// as curves — a `drawingangle` rotation is "this dab follows the stroke" and a
/// `fuzzy` rotation is [`Brush::dab_angle_jitter`] — and the rest have no input
/// here at all. Two of those are worth saying why:
///
/// - **`speed` is deliberately absent.** Krita's speed sensor is a fraction of
///   a fixed maximum drawing speed and Umber's [`DabInput::Speed`] is
///   MyPaint's log-speed axis, on which 45 px/s reads 0.5. Neither the preset
///   nor Umber states where the other's axis begins, so a curve written for
///   one cannot be placed on the other from anything in the file — and a
///   modulation on the wrong axis is a brush that thins at a speed nobody
///   draws at, which is worse than one that says it dropped something. One
///   preset in the fetched packs asks for it.
/// - **`fuzzystroke` is one draw for the whole stroke**, where
///   [`DabInput::Random`] is one per dab. Reading it as the latter turns a
///   single faint splash into a stroke of confetti, so it is named too.
fn dab_input(id: &str) -> Option<DabInput> {
    match id {
        "pressure" => Some(DabInput::Pressure),
        "fuzzy" => Some(DabInput::Random),
        _ => None,
    }
}

/// Wrap five sampled outputs as a modulation, or reject it as too faint to be
/// worth a slot — `mypaint::build`'s rule, restated because that one is private
/// to its own reader and neither wants the other's file format.
fn entry(
    target: DabTarget,
    input: DabInput,
    values: [f32; ResponseCurve::N],
) -> Option<Modulation> {
    let low = values.iter().copied().fold(f32::MAX, f32::min);
    let high = values.iter().copied().fold(f32::MIN, f32::max);
    if !low.is_finite() || !high.is_finite() {
        return None;
    }
    let span = high - low;
    let mut points = [0.0f32; ResponseCurve::N];
    if span > 0.0 {
        for (point, value) in points.iter_mut().zip(values) {
            *point = ((value - low) / span).clamp(0.0, 1.0);
        }
    }
    let m = Modulation {
        target,
        input,
        low,
        high,
        curve: ResponseCurve { points },
    };
    (m.weight() >= 1.0).then_some(m)
}

/// Krita's curve, sampled at the five pressures a [`ResponseCurve`] holds.
struct Dynamic {
    /// The curve's largest output, which multiplies the setting's own value.
    peak: f32,
    samples: [f32; ResponseCurve::N],
    live: bool,
}

impl Dynamic {
    fn flat() -> Self {
        Self {
            peak: 1.0,
            samples: [1.0; ResponseCurve::N],
            live: false,
        }
    }

    fn from_samples(points: Vec<(f32, f32)>) -> Self {
        let mut samples = [0.0f32; ResponseCurve::N];
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = piecewise(&points, ResponseCurve::x_of(i)).clamp(0.0, 1.0);
        }
        let peak = samples.iter().copied().fold(f32::MIN, f32::max);
        let floor = samples.iter().copied().fold(f32::MAX, f32::min);
        Self {
            peak,
            samples,
            // A curve that never moves is Krita's editor leaving a default
            // behind, not a dynamic — the same judgement `mypaint::span` makes,
            // and for the same reason.
            live: peak > 0.0 && peak - floor > 0.01,
        }
    }

    /// Umber's `(min_ratio, curve, enabled)` triple: the curve carries the
    /// shape and the ratio carries the range, so `peak × (ratio + (1 − ratio) ×
    /// curve(p))` reproduces Krita at all five points.
    fn split(&self, when_flat: f32) -> (f32, ResponseCurve, bool) {
        if !self.live {
            return (when_flat, ResponseCurve::LINEAR, false);
        }
        let floor = self.samples.iter().copied().fold(f32::MAX, f32::min);
        let span = self.peak - floor;
        let mut points = [0.0f32; ResponseCurve::N];
        for (point, sample) in points.iter_mut().zip(self.samples) {
            *point = ((sample - floor) / span).clamp(0.0, 1.0);
        }
        (
            (floor / self.peak).clamp(0.0, 1.0),
            ResponseCurve { points },
            true,
        )
    }
}

/// `0,0;0.5,0.25;1,1;` — Krita's curve, as pairs.
fn curve_points(text: &str) -> Option<Vec<(f32, f32)>> {
    let mut out = Vec::new();
    for pair in text.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (x, y) = pair.split_once(',')?;
        out.push((x.trim().parse().ok()?, y.trim().parse().ok()?));
    }
    (out.len() >= 2).then_some(out)
}

/// Evaluate a piecewise-linear curve, holding its end values outside the range.
fn piecewise(points: &[(f32, f32)], x: f32) -> f32 {
    if points.is_empty() {
        return 0.0;
    }
    if x <= points[0].0 {
        return points[0].1;
    }
    let last = points[points.len() - 1];
    if x >= last.0 {
        return last.1;
    }
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if x <= x1 {
            if (x1 - x0).abs() < f32::EPSILON {
                return y1;
            }
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    last.1
}

/// Krita's `softness_curve`, read as an Umber hardness.
///
/// `id="soft"` is `KisCurveMaskGenerator`, whose shape is a curve from the
/// centre of the dab (`x = 0`) to its edge (`x = 1`) giving the coverage there.
/// Umber's dab is `1 - smoothstep(hardness, 1, d)`, whose half-coverage radius
/// is `(hardness + 1) / 2` — so measuring where the curve crosses a half and
/// inverting that gives the same quantity `hfade` gives for the other two
/// generators, and the two paths stay comparable.
///
/// A curve that never reaches a half is softer than any hardness can express;
/// zero — the softest dab Umber draws — is the honest floor for it, and
/// GDQuest's airbrush, whose curve peaks at 0.4 in the middle, is exactly that
/// brush.
fn hardness_from_curve(text: &str) -> Option<f32> {
    let points = curve_points(text)?;
    let mut half = 0.0f32;
    // 64 steps across the radius: finer than the difference between two
    // adjacent hardnesses anyone can see, and it avoids having to reason about
    // a curve whose control points are not sorted.
    for i in 0..=64 {
        let x = i as f32 / 64.0;
        if piecewise(&points, x) >= 0.5 {
            half = x;
        }
    }
    Some((2.0 * half - 1.0).clamp(0.0, 1.0))
}

// ---------------------------------------------------------------------------
// brush_definition
// ---------------------------------------------------------------------------

/// What `brush_definition` says the dab is made of.
struct TipSpec {
    /// A predefined brush's file name, when the tip is a bitmap.
    file: Option<String>,
    /// How much the bitmap is scaled by.
    scale: f32,
    /// A generated brush's diameter in pixels.
    diameter: Option<f32>,
    dab_ratio: f32,
    /// Degrees, converted from the radians the file states.
    angle: f32,
    spacing: Option<f32>,
    hardness: Option<f32>,
    dropped: Vec<&'static str>,
}

impl TipSpec {
    fn parse(xml: &str) -> Result<Self, PresetError> {
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut out = Self {
            file: None,
            scale: 1.0,
            diameter: None,
            dab_ratio: 1.0,
            angle: 0.0,
            spacing: None,
            hardness: None,
            dropped: Vec::new(),
        };
        let mut seen = false;

        loop {
            let event = reader.read_event().map_err(|e| {
                PresetError::Malformed(
                    None,
                    format!("its brush_definition is not readable XML ({e})"),
                )
            })?;
            let e = match &event {
                Event::Start(e) => e.clone(),
                Event::Empty(e) => e.clone(),
                Event::Eof => break,
                _ => continue,
            };
            let attrs = attributes(&e)?;
            match e.local_name().as_ref() {
                b"Brush" => {
                    seen = true;
                    out.file = attrs.get("filename").filter(|f| !f.is_empty()).cloned();
                    out.scale = attrs.number("scale").unwrap_or(1.0).clamp(0.01, 100.0);
                    // Krita's own spacing slider runs to 10, and one shipped
                    // preset — Raghukamath's "Dots" — sits at 5.12. A ceiling
                    // of 4 pulled its dots 20% closer together than its author
                    // spaced them, which for a brush that *is* its spacing is
                    // the whole brush.
                    out.spacing = attrs.number("spacing").map(|s| s.clamp(0.01, 10.0));
                    // Radians. Krita's rotation runs the same way round as
                    // Umber's, so only the units differ.
                    out.angle = attrs
                        .number("angle")
                        .unwrap_or(0.0)
                        .to_degrees()
                        .rem_euclid(360.0);
                    // Both perturb the generated mask per dab and Umber has
                    // nothing that does — but neither threshold is zero, for
                    // the reason `Brush::has_grain`'s and `Brush::smudges`'
                    // are not. Krita's randomness multiplies each mask texel by
                    // a draw from `1 - randomness ..= 1`, so a hundredth is at
                    // most 2.6 levels of 255 at the darkest point of a dab and
                    // less everywhere else; two brushes that differ by that are
                    // not two brushes, and naming it refuses them from the
                    // shipped library for a difference nobody can see.
                    //
                    // Density drops that fraction of the texels at random, and
                    // the same reasoning applies to it — a hundredth is a
                    // speckle the next dab of the stroke covers. **Nothing in
                    // the fetched packs exercises that arm**, though: their
                    // densities are 0.15 to 0.97, all far below the threshold.
                    // It is here so the two settings answer one rule rather
                    // than because a brush was measured through it.
                    if attrs
                        .number("randomness")
                        .is_some_and(|r| r > NEGLIGIBLE_MASK_NOISE)
                    {
                        out.dropped.push("brush-tip randomness");
                    }
                    if attrs
                        .number("density")
                        .is_some_and(|d| d < 1.0 - NEGLIGIBLE_MASK_NOISE)
                    {
                        out.dropped.push("brush-tip density");
                    }
                }
                b"MaskGenerator" => {
                    out.diameter = attrs.number("diameter").map(|d| d.max(1.0));
                    // Krita's ratio scales the *short* axis: 1.0 is a circle
                    // and 0.17 is a chisel six times as long as it is wide.
                    // `Brush::dab_ratio` is long over short, so it inverts.
                    let ratio = attrs.number("ratio").unwrap_or(1.0).clamp(0.05, 20.0);
                    out.dab_ratio = if ratio < 1.0 { 1.0 / ratio } else { ratio };
                    if ratio > 1.0 {
                        // The long axis is now the *other* one, so the diameter
                        // is the short side and the dab is a quarter turn on.
                        out.diameter = out.diameter.map(|d| d * ratio);
                        out.angle = (out.angle + 90.0).rem_euclid(360.0);
                    }
                    // `fade` is **hardness**, not softness, and this read the
                    // wrong way round until it was checked against Krita's own
                    // rendered previews — see the "Fade is hardness" section of
                    // the module docs, which lists the seven presets that
                    // settle it. `hfade` is the fraction of the radius that
                    // stays fully opaque, which is exactly what `dab.wgsl`'s
                    // `smoothstep(hardness, 1.0, d)` means by hardness.
                    //
                    // `id="soft"` is a different generator whose shape is in
                    // `softness_curve`; `hfade` is a leftover field there and
                    // says nothing, so the curve wins where there is one.
                    out.hardness = attrs
                        .get("softness_curve")
                        .and_then(|c| hardness_from_curve(c))
                        .or_else(|| attrs.number("hfade").map(|f| f.clamp(0.0, 1.0)));
                    if attrs.number("spikes").is_some_and(|s| s > 2.5) {
                        out.dropped.push("star-shaped brushes");
                    }
                    if attrs.get("type").is_some_and(|t| t == "rect") {
                        out.dropped.push("square brush shapes");
                    }
                }
                _ => {}
            }
        }

        if !seen {
            return Err(malformed("its brush_definition has no <Brush> in it"));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// The paper texture
// ---------------------------------------------------------------------------

/// Krita's Multiply, whose arithmetic against the dab's alpha is Umber's grain
/// exactly. Thirteen of the thirty-one textured presets in the fetched packs.
///
/// Every other value of `TexturingMode` names a mode that subtracts, dodges,
/// burns or thresholds, and the largest of them is Subtract (14 presets):
/// `alpha − mask`, which `mix(1.0, tile, strength)` cannot stand in for at any
/// tile. Those are named rather than approximated, which is this reader's
/// standing rule and the reason `build-brush-library.rs` refuses them.
const MULTIPLY: i32 = 0;

/// A texture Umber cannot reproduce, because Krita composites its mask with the
/// dab in a way a multiply is not.
pub const OTHER_TEXTURE_MODE: &str = "a paper texture in one of Krita's other blending modes";

/// The texture is on and its pattern is not in the file or beside it.
///
/// Named rather than substituted, for the reason [`KppPreset::paper`] gives:
/// one of Umber's own papers in place of the author's is a grain nobody drew.
pub const MISSING_PATTERN: &str = "a paper texture whose pattern is stored outside the file";

/// Krita's texture strength follows pressure and Umber's grain is one number
/// for the whole stroke.
pub const PAPER_UNDER_PRESSURE: &str = "a paper texture whose strength follows pressure";

/// What Krita's texture option says, read only where Krita reads it.
struct TextureSpec {
    /// `Texture/Strength/Value`, or `Texture/Pattern/Strength` before it.
    strength: f32,
    mode: i32,
    /// How much Krita resamples the pattern by before tiling it.
    scale: f32,
    invert: bool,
    brightness: f32,
    contrast: f32,
    neutral_point: f32,
    cutoff_left: f32,
    cutoff_right: f32,
    cutoff_policy: i32,
    /// The pattern, base64-encoded inside the preset.
    embedded: Option<String>,
    /// The file the pattern is in, when it is not embedded.
    file: Option<String>,
    /// Whether the strength's own curve actually moves.
    strength_varies: bool,
}

/// A pattern read, levelled and measured into what a `Brush` needs.
struct Paper {
    tile: TipMask,
    /// [`Brush::grain_scale`]: the side of one tile in document pixels.
    scale: f32,
}

impl TextureSpec {
    /// The texture option, where it is switched on and doing something.
    ///
    /// `None` covers both "no texture" and "a texture at zero strength", which
    /// paint identically — the same guard `ScatterValue` gets from
    /// `PressureScatter`, and for the same reason: a setting Krita leaves in
    /// the file is not a setting Krita applies, so naming it would refuse a
    /// brush from the shipped library over a loss that is not one. No fetched
    /// preset is at zero (thirty at 1.0 and one at 0.45), so this is the guard
    /// stated rather than assumed.
    fn read(preset: &Preset) -> Option<Self> {
        if !preset.flag("Texture/Pattern/Enabled") {
            return None;
        }
        // Krita's two spellings, current first. Both are in the packs — the
        // curve option's `Value` in eleven presets and the older
        // `Pattern/Strength` in twenty. Absent is Krita's own default of full.
        let strength = preset
            .number("Texture/Strength/Value")
            .or_else(|| preset.number("Texture/Pattern/Strength"))
            .unwrap_or(1.0)
            .clamp(0.0, 1.0);
        if strength <= 0.0 {
            return None;
        }
        Some(Self {
            strength,
            mode: preset
                .number("Texture/Pattern/TexturingMode")
                .unwrap_or(0.0)
                .round() as i32,
            // Krita's own default, and clamped to what a tile can be stretched
            // to: `Brush::grain_scale` bounds the result and a scale of zero
            // would ask for a tile no pixels wide.
            scale: preset
                .number("Texture/Pattern/Scale")
                .unwrap_or(1.0)
                .clamp(0.001, 100.0),
            invert: preset.flag("Texture/Pattern/Invert"),
            brightness: preset.number("Texture/Pattern/Brightness").unwrap_or(0.0),
            contrast: preset.number("Texture/Pattern/Contrast").unwrap_or(1.0),
            neutral_point: preset
                .number("Texture/Pattern/NeutralPoint")
                .unwrap_or(0.5)
                .clamp(0.0, 1.0),
            cutoff_left: preset.number("Texture/Pattern/CutoffLeft").unwrap_or(0.0),
            cutoff_right: preset
                .number("Texture/Pattern/CutoffRight")
                .unwrap_or(255.0),
            cutoff_policy: preset
                .number("Texture/Pattern/CutoffPolicy")
                .unwrap_or(0.0)
                .round() as i32,
            embedded: preset.params.get("Texture/Pattern/Pattern").cloned(),
            file: preset
                .params
                .get("Texture/Pattern/PatternFileName")
                .or_else(|| preset.params.get("Texture/Pattern/Name"))
                .map(|name| {
                    // Krita records whatever path the pattern had on its
                    // author's machine — `/home/raghu/kf5/…` and
                    // `C:/Users/…/AppData/…` are both in the packs — and a
                    // bundle holds the file under its bare name. Never a path,
                    // so a preset cannot reach out of the pack it came in.
                    name.rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(name)
                        .trim()
                        .to_string()
                })
                .filter(|name| !name.is_empty()),
            // `Texture/Strength/UseCurve` gates the sensors exactly as
            // `<Name>UseCurve` gates a dynamic's — `KisCurveOption` reads no
            // sensor at all without it — and the curve is found the same two
            // ways, shared or on the sensor. A sensor that states no curve is
            // the *identity*, which for a strength means one that ramps from
            // nothing at a feather touch: the same reading `sensor_dynamic`
            // takes, and the reason it is not "flat" there either.
            strength_varies: preset.flag("Texture/Strength/UseCurve")
                && Dynamic::from_samples(
                    preset
                        .sensor_curve("Texture/Strength/", "pressure")
                        .unwrap_or_else(|| vec![(0.0, 0.0), (1.0, 1.0)]),
                )
                .live,
        })
    }

    /// The paper this texture paints through, and what it cost to get there.
    ///
    /// One method rather than a `tile` beside a `losses`, because the two
    /// answers are the same answer: a paper that did not resolve *is* the loss,
    /// and two functions deciding that separately is how a texture comes back
    /// carrying a tile and a sentence saying it lost one — or, the way round it
    /// actually failed, painting flat with nothing said.
    ///
    /// **One sentence at most, and the mode goes first.** A Subtract texture
    /// reproduces nothing whatever its pattern does, so naming its pressure
    /// curve underneath would be noise on top of a loss already named — the
    /// rule the library generator follows when it asks what was dropped before
    /// it asks about the mask. An import that cries wolf costs the losses that
    /// matter.
    fn resolve(
        &self,
        patterns: &dyn Fn(&str) -> Option<Vec<u8>>,
    ) -> (Option<Paper>, Vec<&'static str>) {
        if self.mode != MULTIPLY {
            return (None, vec![OTHER_TEXTURE_MODE]);
        }
        let Some(paper) = self.tile(patterns) else {
            return (None, vec![MISSING_PATTERN]);
        };
        if self.strength_varies {
            return (Some(paper), vec![PAPER_UNDER_PRESSURE]);
        }
        (Some(paper), Vec::new())
    }

    /// Krita's mask pipeline as a table, one entry per greyscale value.
    ///
    /// **A table because every step of it is a pure function of one texel's
    /// grey** — `KisTextureMaskInfo::recalculateMask`, in order: subtract the
    /// brightness, apply the contrast about a half, clamp, invert, then the
    /// neutral-point remap and the cutoff. Nothing in it looks at a neighbour
    /// and nothing at the dab, so applying it once at import is exactly what
    /// applying it per fragment would produce, minus six `Copy` fields on
    /// `Brush`, six controls in the brush editor and the arithmetic on every
    /// fragment of every dab for ever.
    ///
    /// The input is [`crate::tip::grain_of`]'s eight-bit reading rather than
    /// Krita's float, which costs at most half a level before a contrast that
    /// is 1.0 in thirty of the thirty-one fetched presets and below 1.0 in the
    /// other — so the one case that could magnify the rounding shrinks it.
    fn levels(&self) -> [u8; 256] {
        let mut table = [0u8; 256];
        for (grey, out) in table.iter_mut().enumerate() {
            let mut value = grey as f32 / 255.0;
            value -= self.brightness;
            value = (value - 0.5) * self.contrast + 0.5;
            value = value.clamp(0.0, 1.0);
            if self.invert {
                value = 1.0 - value;
            }
            // Krita's own two-segment remap, which at the default neutral point
            // of a half is the identity in both segments. It is here rather
            // than skipped because eleven of the fetched presets state the
            // field at all, and a reader that ignored it would be right about
            // those eleven by luck.
            value = if self.neutral_point >= 1.0
                || (self.neutral_point > 0.0 && value <= self.neutral_point)
            {
                value / (2.0 * self.neutral_point.max(f32::EPSILON))
            } else {
                0.5 + (value - self.neutral_point) / (2.0 - 2.0 * self.neutral_point)
            };
            // Outside the cutoff the texel becomes all pit (policy 1) or all
            // paper (policy 2); a policy of zero is off, which is what
            // twenty-two of the thirty-one say.
            let outside = value < self.cutoff_left / 255.0 || value > self.cutoff_right / 255.0;
            if outside {
                match self.cutoff_policy {
                    1 => value = 0.0,
                    2 => value = 1.0,
                    _ => {}
                }
            }
            *out = (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        table
    }

    /// The tile this texture paints through, where Umber can paint it at all.
    ///
    /// `None` is "paint flat and say why": either the mode is one no multiply
    /// reproduces, or the pattern is somewhere this reader cannot reach.
    /// [`Self::losses`] has already named which.
    fn tile(&self, patterns: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<Paper> {
        if self.mode != MULTIPLY {
            return None;
        }
        let png = self.pattern(patterns)?;
        let (width, height, rgba) = decode_rgba(&png)?;
        // The tile is stored at the pattern's own resolution and stretched over
        // `side × scale` document pixels by the sampler that was going to run
        // anyway — which is the picture Krita's pre-resample makes, without a
        // second resampler in `umber-core` to keep in step with the hardware's.
        //
        // The longer side, because `Brush::grain_scale` is one number and the
        // shader stretches the tile square over it. That squashes a non-square
        // pattern, which is not something the packs can be used to justify —
        // every one of the thirty-one is square — and is the same trade the
        // Clip Studio reader already makes for the same reason. A tip escapes
        // it through `TipMask::aspect`; a paper has no equivalent, because the
        // grain is anchored to the document rather than shaped to a dab.
        let side = width.max(height) as f32;
        let scale = (side * self.scale).clamp(Brush::MIN_GRAIN_SCALE, Brush::MAX_GRAIN_SCALE);

        let table = self.levels();
        let coverage: Vec<u8> = crate::tip::grain_of(&rgba)
            .into_iter()
            .map(|grey| table[grey as usize])
            .collect();
        // A pattern larger than a mask may be is refused rather than resampled
        // down, exactly as `TipMask::from_picture` refuses an oversized stamp:
        // a silent reduction is a paper that bites at the wrong pitch with
        // nothing saying so. Nothing in the fetched packs is over 512.
        let tile = TipMask::new(width, height, coverage).ok()?;
        Some(Paper { tile, scale })
    }

    /// The pattern's PNG bytes, from inside the preset or from beside it.
    ///
    /// **Krita's embedded pattern is base64 twice**: the option holds the
    /// picture as a base64 string and the properties configuration then encodes
    /// that string in turn, so one decode hands back text beginning `iVBORw0K`.
    /// Sniffing the result rather than decoding a fixed number of times is what
    /// keeps this right for a writer that only does it once.
    ///
    /// An embedded blob that will not decode **falls through to the file**
    /// rather than ending the lookup. A preset routinely carries both — every
    /// one of the twenty embedded patterns also names its author's own path —
    /// so a `?` inside the first branch would turn one unreadable blob into a
    /// brush painting flat beside a bundle that holds the picture.
    fn pattern(&self, patterns: &dyn Fn(&str) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
        const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

        let embedded = || {
            let mut raw = base64(self.embedded.as_ref()?)?;
            if !raw.starts_with(PNG_MAGIC) {
                raw = base64(std::str::from_utf8(&raw).ok()?)?;
            }
            raw.starts_with(PNG_MAGIC).then_some(raw)
        };
        embedded().or_else(|| self.file.as_ref().and_then(|name| patterns(name)))
    }
}

/// A pattern's pixels as straight-alpha sRGB RGBA8 — what
/// [`crate::tip::grain_of`] takes.
///
/// Every pattern in the fetched packs is a PNG, including the three whose names
/// end `.pat`: Krita re-encodes a GIMP pattern when it embeds one, so there is
/// no second decoder to write and none is written on the guess that there might
/// be. A pattern in any other format resolves to nothing and the brush is told
/// it lost its paper.
fn decode_rgba(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png));
    // Palette and 16-bit patterns are both in the packs, and this expands
    // either to the eight-bit channels the reading below assumes.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;

    // The slices below are bounded by what the *frame* declares and the buffer
    // by what the *image* declared, and this is a picture out of somebody
    // else's file: an APNG's frame, or a decoder that ever disagreed with
    // itself, would index past the end and panic inside an import. The
    // `checked_mul` is the same guard on the other side, for two dimensions
    // whose product a `usize` does not hold.
    let texels = (info.width as usize).checked_mul(info.height as usize)?;
    if texels.checked_mul(info.color_type.samples())? > buf.len() {
        return None;
    }
    let rgba: Vec<u8> = match info.color_type {
        png::ColorType::Grayscale => buf[..texels].iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf[..texels * 2]
            .chunks_exact(2)
            .flat_map(|px| [px[0], px[0], px[0], px[1]])
            .collect(),
        png::ColorType::Rgb => buf[..texels * 3]
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 255])
            .collect(),
        png::ColorType::Rgba => buf[..texels * 4].to_vec(),
        _ => return None,
    };
    Some((info.width, info.height, rgba))
}

// ---------------------------------------------------------------------------
// Tips
// ---------------------------------------------------------------------------

struct DecodedTip {
    mask: TipMask,
    dropped: Vec<&'static str>,
}

/// Turn a predefined brush's file into a coverage mask.
///
/// Krita stores the same three formats GIMP does, so this sniffs the contents
/// rather than trusting the extension: `brush_definition` routinely says
/// `type="gbr_brush"` for a file whose name ends `.gih`, and the Revoy bundle
/// is full of both.
fn decode_tip(name: &str, raw: &[u8]) -> Result<DecodedTip, PresetError> {
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    if raw.starts_with(PNG_MAGIC) {
        return Ok(DecodedTip {
            mask: tip_from_png(raw)?,
            dropped: Vec::new(),
        });
    }
    // A `.gih` starts with a name line and a count line; a `.gbr` starts with
    // four big-endian words. The two are told apart by whether the first bytes
    // are a plausible header size, which for text they never are.
    if let Ok(pipe) = gih::from_gih(raw) {
        let mut dropped = Vec::new();
        if pipe.angular {
            dropped.push(gih::ANGULAR);
        } else if pipe.animated {
            dropped.push(gih::ANIMATION);
        }
        // A coloured cell is no longer a loss: `gih` and `gbr` go through the
        // same decoder, and a `TipMask` carries a colour plane now. What a pipe
        // still loses is its *sequencing*, reported above.
        let cell = pipe
            .cells
            .into_iter()
            .next()
            .ok_or_else(|| malformed("an empty brush pipe"))?;
        return Ok(DecodedTip {
            mask: cell.tip,
            dropped,
        });
    }
    match gbr::from_gbr(raw) {
        // Nothing dropped: a coloured `.gbr` keeps its colour through the same
        // decoder every other stamp uses.
        Ok(brush) => Ok(DecodedTip {
            dropped: Vec::new(),
            mask: brush.tip,
        }),
        Err(_) => Err(PresetError::Malformed(
            None,
            format!("its tip `{name}` is in a format Umber cannot read"),
        )),
    }
}

/// Decode a Krita PNG brush.
///
/// **Inverted relative to a `.gbr`.** Krita paints where the picture is *dark*,
/// so coverage is `255 - luminance` — the opposite of [`TipMask`]'s convention
/// and of GIMP's. A file with real transparency is a colour stamp instead, and
/// its silhouette is its alpha; those are the only two rules Krita itself has,
/// so neither is a guess.
fn tip_from_png(bytes: &[u8]) -> Result<TipMask, PresetError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| PresetError::Malformed(None, format!("a brush tip would not open ({e})")))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("a brush tip is too large to decode"))?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| PresetError::Malformed(None, format!("a brush tip would not decode ({e})")))?;

    let texels = info.width as usize * info.height as usize;
    let luminance = |px: &[u8]| {
        // Rec. 601, which is what Qt's `qGray` uses and therefore what Krita
        // saw when the brush was made.
        ((px[0] as u32 * 11 + px[1] as u32 * 16 + px[2] as u32 * 5) / 32) as u8
    };
    let coverage: Vec<u8> = match info.color_type {
        png::ColorType::Grayscale => buf[..texels].iter().map(|g| 255 - g).collect(),
        png::ColorType::GrayscaleAlpha => {
            let px: Vec<&[u8]> = buf[..texels * 2].chunks_exact(2).collect();
            if px.iter().any(|p| p[1] != 255) {
                px.iter().map(|p| p[1]).collect()
            } else {
                px.iter().map(|p| 255 - p[0]).collect()
            }
        }
        png::ColorType::Rgb => buf[..texels * 3]
            .chunks_exact(3)
            .map(|px| 255 - luminance(px))
            .collect(),
        png::ColorType::Rgba => {
            let px: Vec<&[u8]> = buf[..texels * 4].chunks_exact(4).collect();
            if px.iter().any(|p| p[3] != 255) {
                px.iter().map(|p| p[3]).collect()
            } else {
                px.iter().map(|p| 255 - luminance(p)).collect()
            }
        }
        other => {
            return Err(PresetError::Malformed(
                None,
                format!("a brush tip in {other:?} is not something Krita writes"),
            ));
        }
    };
    // Not padded out to a square: the dab carries the mask's proportions in its
    // own scale, so a tall Krita tip stays tall without spending a texture on
    // the margin. See `TipMask::aspect`.
    TipMask::new(info.width, info.height, coverage)
}

// ---------------------------------------------------------------------------
// Odds and ends
// ---------------------------------------------------------------------------

/// One element's attributes, decoded once.
///
/// A local copy rather than `docimport::container::Attrs`, which is private to
/// the document readers — sharing it would mean making the document import's
/// plumbing public to the brush import, and the two have nothing else in
/// common.
struct Attrs(BTreeMap<String, String>);

impl Attrs {
    fn get(&self, key: &str) -> Option<&String> {
        self.0.get(key)
    }

    fn number(&self, key: &str) -> Option<f32> {
        self.0.get(key)?.trim().parse().ok()
    }
}

fn attributes(e: &quick_xml::events::BytesStart<'_>) -> Result<Attrs, PresetError> {
    let mut out = BTreeMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|e| {
            PresetError::Malformed(None, format!("a malformed XML attribute ({e})"))
        })?;
        let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| {
                PresetError::Malformed(None, format!("an unreadable XML attribute ({e})"))
            })?;
        out.insert(key, value.into_owned());
    }
    Ok(Attrs(out))
}

/// Decode base64, ignoring whitespace and tolerating missing padding.
///
/// Written here rather than taken from a crate. It is thirty lines, it is used
/// by exactly one caller, and the alternative is a dependency in the *engine*
/// crate — which `CLAUDE.md` asks to be justified by what it saves. This saves
/// nothing.
fn base64(text: &str) -> Option<Vec<u8>> {
    fn sextet(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }

    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for &c in text.as_bytes() {
        if c.is_ascii_whitespace() || c == b'=' {
            continue;
        }
        accumulator = (accumulator << 6) | sextet(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn malformed(message: &str) -> PresetError {
    PresetError::Malformed(None, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap settings XML in a PNG, which is what a `.kpp` is. Built by hand
    /// rather than vendored, the same discipline as the `.gbr` fixtures: the
    /// test then pins the byte layout instead of describing whatever file
    /// happened to be on disk.
    fn kpp(xml: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_ztxt_chunk("preset".to_string(), xml.to_string())
            .expect("text chunk");
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[128]).expect("pixel");
        drop(writer);
        out
    }

    /// The same, with the settings in a chunk of the caller's choosing.
    fn kpp_with(chunk: &str, xml: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        match chunk {
            "tEXt" => encoder.add_text_chunk("preset".to_string(), xml.to_string()),
            "iTXt" => encoder.add_itxt_chunk("preset".to_string(), xml.to_string()),
            _ => encoder.add_ztxt_chunk("preset".to_string(), xml.to_string()),
        }
        .expect("text chunk");
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[128]).expect("pixel");
        drop(writer);
        out
    }

    fn param(name: &str, value: &str) -> String {
        format!("<param name=\"{name}\" type=\"string\"><![CDATA[{value}]]></param>")
    }

    /// The other spelling: `type="internal"` writes the value as element text
    /// rather than wrapping it in CDATA, and every `Pressure<Name>` flag in a
    /// real preset is written this way.
    fn internal(name: &str, value: impl std::fmt::Display) -> String {
        format!("<param type=\"internal\" name=\"{name}\">{value}</param>")
    }

    /// Revoy's "Basic Oval Brush", trimmed to what this importer reads.
    fn oval() -> String {
        format!(
            "<Preset embedded_resources=\"0\" name=\"Basic Oval\" paintopid=\"paintbrush\">{}{}{}{}{}{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.02\" angle=\"5.98648\" \
                 useAutoSpacing=\"0\" density=\"1\" randomness=\"0\"> \
                 <MaskGenerator diameter=\"40\" type=\"circle\" hfade=\"0.89\" ratio=\"0.65\" \
                 id=\"default\" vfade=\"0.89\" spikes=\"2\"/> </Brush>"
            ),
            param("OpacityValue", "1"),
            param("FlowValue", "1"),
            param("OpacitySensor", "<params id=\"pressure\"/>"),
            param("OpacitycommonCurve", "0,0;1,1;"),
            "<param type=\"internal\" name=\"PressureOpacity\">true</param>",
            "<param type=\"internal\" name=\"OpacityUseCurve\">true</param>",
            "<param type=\"internal\" name=\"PressureSize\">false</param>",
            "<param type=\"internal\" name=\"EraserMode\">false</param>",
        )
    }

    #[test]
    fn a_generated_krita_brush_lands_on_umbers_own_ellipse() {
        let preset = from_kpp(&kpp(&oval())).expect("decode");
        assert_eq!(preset.name, "Basic Oval");
        assert_eq!(preset.brush.size, 40.0);
        assert_eq!(preset.brush.spacing, 0.02);
        // Krita's ratio scales the short axis, so it is the reciprocal of
        // Umber's. Reading it straight through would make a chisel a circle.
        assert!((preset.brush.dab_ratio - 1.0 / 0.65).abs() < 1e-4);
        // `hfade` is the opaque fraction of the radius, which is exactly what
        // `dab.wgsl` means by hardness. Revoy's oval brush draws a hard-edged
        // stroke in Krita's own preview of it; `1 - hfade` made it a cloud.
        assert!((preset.brush.hardness - 0.89).abs() < 1e-5);
        // Radians in the file, degrees in the brush.
        assert!(
            (preset.brush.dab_angle - 343.0).abs() < 0.5,
            "{}",
            preset.brush.dab_angle
        );
        assert!(preset.brush.pressure_opacity);
        assert!(!preset.brush.pressure_size);
        assert!(preset.tip.is_none());
        assert!(preset.dropped.is_empty());
    }

    /// The mistake this reader exists not to make. `PressureSize` is Krita's
    /// spelling of "the Size dynamic is switched on"; the sensor says what
    /// drives it. Reading the flag as "pressure drives size" turns every
    /// speed-driven brush into a pressure-driven one.
    #[test]
    fn a_dynamic_driven_by_something_else_is_not_read_as_pressure() {
        let with = |sensor: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"20\" type=\"circle\" ratio=\"1\" hfade=\"0\"/></Brush>"
                ),
                param("SizeSensor", &format!("<params id=\"{sensor}\"/>")),
                param("SizecommonCurve", "0,0;1,1;"),
                "<param type=\"internal\" name=\"PressureSize\">true</param>",
                "<param type=\"internal\" name=\"SizeUseCurve\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode")
        };

        let by_pressure = with("pressure");
        assert!(by_pressure.brush.pressure_size);
        assert!(by_pressure.brush.min_size_ratio < 0.01);

        // `speed` stays unread, and is named. Krita's speed sensor is a
        // fraction of a fixed maximum drawing speed where Umber's is MyPaint's
        // log-speed axis; neither the preset nor Umber states where the other's
        // begins, so the curve cannot be placed and a guess would be a brush
        // that thins at a speed nobody draws at.
        let by_speed = with("speed");
        assert!(!by_speed.brush.pressure_size);
        assert!(by_speed.brush.modulations.is_empty());
        assert!(
            by_speed.dropped.contains(&FOREIGN_INPUT),
            "{:?}",
            by_speed.dropped
        );

        // `fuzzy` is a fresh uniform draw per dab, which is exactly
        // `DabInput::Random`, so it is carried rather than named.
        let by_fuzzy = with("fuzzy");
        assert!(!by_fuzzy.brush.pressure_size);
        assert!(by_fuzzy.dropped.is_empty(), "{:?}", by_fuzzy.dropped);
        let m = by_fuzzy.brush.modulations.as_slice();
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(
            (m[0].target, m[0].input),
            (DabTarget::Size, DabInput::Random)
        );
    }

    /// The `sensorslist` fault, which cost a preset **both** halves of its
    /// dynamic.
    ///
    /// Krita writes a compound sensor as `<params id="sensorslist">` with a
    /// `<ChildSensor>` per input, so reading the first id off the blob answers
    /// "sensorslist" — which is not `pressure`, so the pressure curve was
    /// dropped, and is not an input Umber has, so the whole preset was refused.
    /// Eleven presets in the fetched packs drive Size, Opacity or Scatter that
    /// way.
    #[test]
    fn a_compound_sensor_keeps_its_pressure_curve_and_its_random_one() {
        // Deevad's "c3) Thin Brush Textured", trimmed: the pressure child
        // carries no curve of its own and the fuzzy one does, with
        // `UseSameCurve` off — so Krita gives pressure the default linear ramp
        // and fuzzy its own.
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"20\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
            ),
            param("OpacityValue", "1"),
            param(
                "OpacitySensor",
                "<params id=\"sensorslist\"> <ChildSensor id=\"pressure\"/> \
                 <ChildSensor id=\"fuzzy\"> <curve>0,0.5;1,1;</curve> </ChildSensor> </params>"
            ),
            param("OpacitycommonCurve", "0,0.5;1,1;"),
            internal("PressureOpacity", true),
            internal("OpacityUseCurve", true),
            internal("OpacityUseSameCurve", false),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(preset.dropped.is_empty(), "{:?}", preset.dropped);

        // The pressure half, which used to vanish entirely.
        assert!(preset.brush.pressure_opacity);
        assert!(preset.brush.coverage_at(0.0) < 0.01);
        assert!((preset.brush.coverage_at(1.0) - 1.0).abs() < 0.01);

        // The random half, as a factor on coverage: Krita multiplies its
        // sensors, and `Modulated::opacity` is the one target that already
        // composes that way.
        let m = preset.brush.modulations.as_slice();
        assert_eq!(m.len(), 1, "{m:?}");
        assert_eq!(
            (m[0].target, m[0].input),
            (DabTarget::Opacity, DabInput::Random)
        );
        assert!((m[0].at(0.0) - 0.5).abs() < 0.01, "{:?}", m[0]);
        assert!((m[0].at(1.0) - 1.0).abs() < 0.01, "{:?}", m[0]);
        // The sensor's peak is 1, so the brush's own opacity is untouched.
        assert!((preset.brush.opacity - 1.0).abs() < 1e-5);
    }

    /// The units differ per target and getting one wrong is a brush at the
    /// wrong size — which has happened here before.
    ///
    /// Krita multiplies its sensors together, so the sensor's peak belongs in
    /// the setting's own value and the entry carries the fraction of it each
    /// draw asks for. Size is a **log** offset, because a factor there
    /// multiplies the radius; scatter is stated in dab radii and Umber adds it.
    #[test]
    fn a_random_sensor_carries_its_targets_own_units() {
        let with = |option: &str, extra: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{extra}{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"40\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                     </Brush>"
                ),
                param(
                    &format!("{option}Sensor"),
                    "<params id=\"fuzzy\"> <curve>0,0.5;1,1;</curve> </params>"
                ),
                internal(&format!("Pressure{option}"), true),
                internal(&format!("{option}UseCurve"), true),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush
        };

        // Size: half the diameter at the bottom of the draw, all of it at the
        // top, and the base size is the peak — which is 1 here, so 40 stands.
        let size = with("Size", "");
        assert_eq!(size.size, 40.0);
        let m = size.modulations.as_slice()[0];
        assert!((m.at(1.0).exp() - 1.0).abs() < 0.01, "{m:?}");
        assert!((m.at(0.0).exp() - 0.5).abs() < 0.01, "{m:?}");

        // Scatter: Umber adds where Krita multiplies, so the entry runs from
        // minus half the scatter to nothing.
        let scatter = with("Scatter", &param("ScatterValue", "2"));
        assert!((scatter.scatter - 2.0).abs() < 1e-5);
        let m = scatter.modulations.as_slice()[0];
        assert_eq!(m.target, DabTarget::Scatter);
        assert!((m.at(1.0)).abs() < 0.01, "{m:?}");
        assert!((m.at(0.0) + 1.0).abs() < 0.01, "{m:?}");
    }

    /// A sensor's peak multiplies the setting's own value, exactly as the
    /// pressure curve's does — so a draw that never reaches full strength
    /// makes the brush smaller rather than leaving it at its stated size and
    /// putting the shortfall in the table.
    #[test]
    fn a_sensors_peak_lands_on_the_setting_it_scales() {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"40\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
            ),
            param(
                "SizeSensor",
                "<params id=\"fuzzy\"> <curve>0,0.25;1,0.5;</curve> </params>"
            ),
            internal("PressureSize", true),
            internal("SizeUseCurve", true),
        );
        let brush = from_kpp(&kpp(&xml)).expect("decode").brush;
        assert!((brush.size - 20.0).abs() < 0.01, "{}", brush.size);
        let m = brush.modulations.as_slice()[0];
        assert!((m.at(1.0).exp() - 1.0).abs() < 0.01, "{m:?}");
        assert!((m.at(0.0).exp() - 0.5).abs() < 0.01, "{m:?}");
    }

    /// `<Name>UseSameCurve` decides which of the two curves in the file is in
    /// force, and Krita's editor leaves the other one behind. Six options
    /// across four presets in the fetched packs carry both — Deevad's "Eraser
    /// Kneaded Soft" among them, whose shared curve is not the one Krita
    /// applies to it.
    #[test]
    fn the_shared_curve_is_read_only_where_krita_would_read_it() {
        // `same` is an `Option` so the flag's *absence* is expressible, which
        // is the only case `flag_or`'s default decides. Spelling it as another
        // "true" would be a test that passes with the default set either way.
        let with = |same: Option<bool>| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"40\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                     </Brush>"
                ),
                param(
                    "SizeSensor",
                    "<params id=\"pressure\"> <curve>0,0;1,0.25;</curve> </params>"
                ),
                param("SizecommonCurve", "0,0;1,1;"),
                internal("PressureSize", true),
                internal("SizeUseCurve", true),
                same.map(|s| internal("SizeUseSameCurve", s))
                    .unwrap_or_default(),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush.size
        };
        // Shared: the peak of `0,0;1,1;` is 1, so the diameter stands.
        assert!((with(Some(true)) - 40.0).abs() < 0.01);
        // Not shared: the sensor's own curve peaks at 0.25.
        assert!((with(Some(false)) - 10.0).abs() < 0.01);
        // Absent means shared, which is Krita's own default for the flag — and
        // a preset that states a `commonCurve` and nothing beside it means that
        // curve to apply.
        assert!((with(None) - 40.0).abs() < 0.01);
    }

    /// A dynamic whose curve is switched off has nothing to sample, so an
    /// input that reaches it through one really does do nothing here — and
    /// must be named rather than quietly carried at full strength.
    ///
    /// It gets its **own** sentence: `fuzzy` is an input Umber plainly can
    /// produce, so borrowing `FOREIGN_INPUT` here would put a cause on the
    /// notice that is not the cause.
    #[test]
    fn a_sensor_whose_curve_is_switched_off_is_named_rather_than_invented() {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"40\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
            ),
            param("ScatterValue", "2"),
            param(
                "ScatterSensor",
                "<params id=\"sensorslist\"> <ChildSensor id=\"pressure\"/> \
                 <ChildSensor id=\"fuzzy\"/> </params>"
            ),
            internal("PressureScatter", true),
            internal("ScatterUseCurve", false),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(preset.brush.modulations.is_empty());
        assert!(
            preset.dropped.contains(&UNREAD_CURVE),
            "{:?}",
            preset.dropped
        );
        assert!(
            !preset.dropped.contains(&FOREIGN_INPUT),
            "{:?}",
            preset.dropped
        );
    }

    /// A sensor whose curve never moves still scales the setting.
    ///
    /// Krita multiplies its sensors in, so a `fuzzy` curve sitting flat at 0.3
    /// is a third of the size on every dab. Gating the whole reading on the
    /// curve *varying* threw that peak away with the entry, and the brush came
    /// out three times too big — carried nowhere and named nowhere, which is
    /// the one outcome `dropped_sensor` exists to make impossible.
    #[test]
    fn a_sensor_that_never_moves_still_scales_what_it_drives() {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"60\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
            ),
            param(
                "SizeSensor",
                "<params id=\"fuzzy\"> <curve>0,0.5;1,0.5;</curve> </params>"
            ),
            internal("PressureSize", true),
            internal("SizeUseCurve", true),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(
            (preset.brush.size - 30.0).abs() < 0.01,
            "{}",
            preset.brush.size
        );
        // Flat is the exact identity per dab, so it earns no slot in the table.
        assert!(preset.brush.modulations.is_empty());
        // And nothing was lost, so nothing is named.
        assert!(preset.dropped.is_empty(), "{:?}", preset.dropped);
    }

    /// The fast path. A preset that names nothing but pressure must arrive with
    /// an empty table, or every Krita brush in the library pays the stroke
    /// builder's modulation machinery to be told it changes nothing.
    #[test]
    fn a_preset_driven_by_pressure_alone_gains_no_modulations() {
        let preset = from_kpp(&kpp(&oval())).expect("decode");
        assert!(preset.brush.modulations.is_empty());
        assert!(!preset.brush.is_modulated());
    }

    /// A pressure curve has to come back through Umber's own `radius_at`.
    #[test]
    fn the_size_curve_reproduces_kritas_at_every_sample() {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"100\" type=\"circle\" ratio=\"1\" hfade=\"0\"/></Brush>"
            ),
            param("SizeSensor", "<params id=\"pressure\"/>"),
            param("SizecommonCurve", "0,0.2;1,0.8;"),
            "<param type=\"internal\" name=\"PressureSize\">true</param>",
            "<param type=\"internal\" name=\"SizeUseCurve\">true</param>",
        );
        let brush = from_kpp(&kpp(&xml)).expect("decode").brush;
        // Krita's diameter times the curve's peak.
        assert!((brush.size - 80.0).abs() < 0.01, "size {}", brush.size);
        for i in 0..=4 {
            let p = i as f32 / 4.0;
            let expected = 100.0 * (0.2 + 0.6 * p) / 2.0;
            assert!(
                (brush.radius_at(p) - expected).abs() < 0.1,
                "at {p}: expected {expected}, got {}",
                brush.radius_at(p)
            );
        }
    }

    /// A predefined tip that nothing can supply must not arrive as a silent
    /// round brush — that is the failure the whole reader is written against.
    #[test]
    fn a_missing_predefined_tip_is_named_rather_than_painted_round_in_silence() {
        let xml = format!(
            "<Preset name=\"Stamp\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"png_brush\" filename=\"bristle.png\" spacing=\"0.05\" \
                 angle=\"0\" scale=\"0.5\"/>"
            ),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert_eq!(preset.missing_tip.as_deref(), Some("bristle.png"));
        assert!(preset.tip.is_none());
        // The spacing it did carry still arrives.
        assert_eq!(preset.brush.spacing, 0.05);
    }

    /// Krita paints where the picture is dark. Reading a PNG brush the `.gbr`
    /// way gives a solid square with a hole in it.
    #[test]
    fn an_embedded_png_tip_is_read_the_way_krita_reads_it() {
        // A 2x1 greyscale tip: black then white.
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[0, 255]).expect("data");
        }
        let xml = format!(
            "<Preset embedded_resources=\"1\" name=\"Stamp\" paintopid=\"paintbrush\">\
             <resources><resource name=\"t\" filename=\"t.png\" type=\"brushes\">\
             <![CDATA[{}]]></resource></resources>{}</Preset>",
            encode_base64(&png),
            param(
                "brush_definition",
                "<Brush type=\"png_brush\" filename=\"t.png\" spacing=\"0.05\" \
                 angle=\"0\" scale=\"2\"/>"
            ),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(preset.missing_tip.is_none());
        let tip = preset.tip.expect("a mask came with it");
        // Kept at its own proportions rather than padded out to a square — the
        // dab carries the aspect — and black is full paint.
        assert_eq!((tip.width(), tip.height()), (2, 1));
        assert_eq!(tip.at(0, 0), 255, "black should be full coverage");
        assert_eq!(tip.at(1, 0), 0, "white should be none");
        // The size follows the bitmap through its scale.
        assert_eq!(preset.brush.size, 4.0);
    }

    /// Whether a Krita stamp builds up is **measured**, not assumed.
    ///
    /// Krita composites every dab, so a faint tip builds to solid along a
    /// stroke; Umber's `max` caps a stroke at the mask's brightest texel and
    /// paints a fraction of the author's mark. `.gbr` and `.abr` have measured
    /// this since build-up existed and `.kpp` did not, which meant a Krita
    /// stamp — Raghukamath's "Drybrush" was the one in the library — shipped at
    /// 88% of the strength it was drawn at. That preset no longer ships, for a
    /// paper texture Umber cannot carry, so the measurement now serves imports
    /// alone; it is the same stamp either way.
    #[test]
    fn a_krita_stamp_is_measured_for_build_up() {
        let stamp = |level: u8| {
            // Krita's convention: 0 is full paint, 255 is none. A flat grey of
            // 130 is a mask whose strongest texel is just under half.
            let mut png = Vec::new();
            {
                let mut encoder = png::Encoder::new(&mut png, 8, 8);
                encoder.set_color(png::ColorType::Grayscale);
                encoder.set_depth(png::BitDepth::Eight);
                let mut writer = encoder.write_header().expect("header");
                writer.write_image_data(&[level; 64]).expect("data");
            }
            let xml = format!(
                "<Preset embedded_resources=\"1\" name=\"Stamp\" paintopid=\"paintbrush\">\
                 <resources><resource name=\"t\" filename=\"t.png\" type=\"brushes\">\
                 <![CDATA[{}]]></resource></resources>{}</Preset>",
                encode_base64(&png),
                param(
                    "brush_definition",
                    "<Brush type=\"png_brush\" filename=\"t.png\" spacing=\"0.1\" \
                     angle=\"0\" scale=\"1\"/>"
                ),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush.build_up
        };

        assert!(stamp(130), "a faint stamp has to be allowed to accumulate");
        assert!(!stamp(0), "a solid one paints the same either way");

        // A generated dab is solid by construction, so the `max` path stays the
        // default for every brush without a tip.
        assert!(!from_kpp(&kpp(&oval())).expect("decode").brush.build_up);
    }

    /// A caller with the file elsewhere — a `.bundle`'s `brushes/` — must be
    /// able to supply it.
    #[test]
    fn a_tip_can_be_resolved_from_outside_the_file() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[0]).expect("data");
        }
        let xml = format!(
            "<Preset name=\"Stamp\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"png_brush\" filename=\"outside.png\" spacing=\"0.05\" \
                 angle=\"0\" scale=\"1\"/>"
            ),
        );
        let preset = from_kpp_in(
            &kpp(&xml),
            &Sidecar {
                brushes: &|name| (name == "outside.png").then(|| png.clone()),
                patterns: &|_| None,
            },
        )
        .expect("decode");
        assert!(preset.missing_tip.is_none());
        assert_eq!(preset.tip.expect("mask").at(0, 0), 255);
    }

    /// A different painting engine is refused by name. A round dab wearing
    /// `deformbrush`'s name would be invention, and the standing rule is that
    /// subtly wrong pixels are worse than a refusal.
    #[test]
    fn another_painting_engine_is_refused_and_says_which() {
        let xml = "<Preset name=\"Warp\" paintopid=\"deformbrush\">\
                   <param type=\"internal\" name=\"x\">1</param></Preset>";
        let err = from_kpp(&kpp(xml)).expect_err("refused");
        assert!(err.to_string().contains("deformbrush"), "{err}");
    }

    #[test]
    fn a_smudging_preset_arrives_as_one() {
        let xml = format!(
            "<Preset name=\"Blender\" paintopid=\"colorsmudge\">{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.09\" angle=\"0\">\
                 <MaskGenerator diameter=\"72\" type=\"circle\" ratio=\"0.5\" hfade=\"1\"/></Brush>"
            ),
            param("SmudgeRateValue", "0.8"),
            param("SmudgeRadiusValue", "1.5"),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(preset.brush.smudges());
        // No `PressureColorRate`, so Krita's Color Rate is off and its
        // `colorRate` is `0.0` — the dab carries only what it lifted. This
        // asserted `0.8`, the pickup rate, which read that field as though it
        // were the mix; the rate governs how *faintly* the dab lands, which
        // this import discards.
        assert_eq!(preset.brush.smudge, 1.0);
        assert!((preset.brush.smudge_radius - 1.5).abs() < 1e-5);
        assert_eq!(preset.brush.dab_ratio, 2.0);
    }

    /// Krita's deposit rate is a second knob on the one mix Umber has, and
    /// `PressureColorRate` is its enable flag — the rule the module docs open
    /// with, and the `ScatterValue` bug again if it is not read. With the flag
    /// clear the value is a leftover and the brush is a pure blender; with it
    /// set the ratio decides the mix, and neither case loses anything to name.
    #[test]
    fn the_deposit_rate_is_the_other_half_of_the_mix_and_only_counts_when_it_is_on() {
        let brush = |deposit_on: bool| {
            let xml = format!(
                "<Preset name=\"Mix\" paintopid=\"colorsmudge\">{}{}{}\
                 <param type=\"internal\" name=\"PressureSmudgeRate\">true</param>\
                 <param type=\"internal\" name=\"PressureColorRate\">{deposit_on}</param>\
                 </Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"40\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
                ),
                param("SmudgeRateValue", "0.8"),
                param("ColorRateValue", "0.6"),
            );
            from_kpp(&kpp(&xml)).expect("decode")
        };

        // Off: Krita's `colorRate` is literally `0.0` there, so the dab carries
        // nothing but what it lifted — a pure blender, *whatever* the pickup
        // rate reads. Answering the pickup instead was a real bug and this is
        // the guard: with 0.8 here it left the brush depositing a fifth of the
        // palette colour where Krita deposits none. What 0.8 does govern is how
        // faintly the dab lands, which this import discards — see the module
        // docs, which name the slot it would go in rather than claiming none.
        let off = brush(false);
        assert_eq!(off.brush.smudge, 1.0);

        // On: 0.8 lifted against 0.6 laid down, so four sevenths of a dab's
        // colour came off the canvas — less pickup than the rate alone says,
        // which is the whole correction.
        let on = brush(true);
        assert!(
            (on.brush.smudge - 4.0 / 7.0).abs() < 1e-5,
            "{}",
            on.brush.smudge
        );

        // Neither reading has anything left over to apologise for.
        for preset in [&off, &on] {
            assert!(
                !preset.dropped.iter().any(|d| d.contains("deposit")),
                "{:?}",
                preset.dropped
            );
        }
    }

    #[test]
    fn an_eraser_arrives_as_an_eraser() {
        let xml = format!(
            "<Preset name=\"Eraser\" paintopid=\"paintbrush\">{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"0.5\"/></Brush>"
            ),
            "<param type=\"internal\" name=\"EraserMode\">true</param>",
        );
        assert_eq!(from_kpp(&kpp(&xml)).unwrap().brush.mode, BrushMode::Erase);
    }

    /// Note the texture key. This fixture said `Texture/Enabled`, which is
    /// what the reader looked for and what Krita has never written, so the two
    /// agreed with each other and with no real file — a hand-built fixture's
    /// one failure mode, and the reason the pack sweep in `docs/brushes.md`
    /// exists beside these tests rather than instead of them.
    #[test]
    fn features_umber_cannot_render_are_named() {
        let xml = format!(
            "<Preset name=\"Rich\" paintopid=\"paintbrush\">{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\" randomness=\"0.4\" \
                 density=\"0.5\"><MaskGenerator diameter=\"30\" type=\"rect\" ratio=\"1\" \
                 hfade=\"0.5\" spikes=\"5\"/></Brush>"
            ),
            "<param type=\"internal\" name=\"MaskingBrush/Enabled\">true</param>",
            "<param type=\"internal\" name=\"Texture/Pattern/Enabled\">true</param>",
        );
        let dropped = from_kpp(&kpp(&xml)).expect("decode").dropped;
        for expected in [
            "brush-tip randomness",
            "brush-tip density",
            "star-shaped brushes",
            "square brush shapes",
            "masking brushes",
            // The texture is on and its pattern is nowhere: this fixture
            // embeds none and there is nothing beside it.
            MISSING_PATTERN,
        ] {
            assert!(
                dropped.contains(&expected),
                "{expected} missing: {dropped:?}"
            );
        }
    }

    /// Three settings that are switched on and do nothing, and a reader that
    /// takes any of them at face value apologises for a loss that did not
    /// happen. Krita mirrors nothing with neither axis ticked; a hundredth of
    /// mask randomness and a fiftieth of density are below what the alpha they
    /// perturb can even hold.
    #[test]
    fn a_setting_switched_on_and_doing_nothing_is_not_a_loss() {
        let xml = format!(
            "<Preset name=\"Quiet\" paintopid=\"paintbrush\">{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\" randomness=\"0.01\" \
                 density=\"0.99\"><MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" \
                 hfade=\"0.5\"/></Brush>"
            ),
            "<param type=\"internal\" name=\"PressureMirror\">true</param>",
            "<param type=\"internal\" name=\"HorizontalMirrorEnabled\">false</param>",
            "<param type=\"internal\" name=\"VerticalMirrorEnabled\">false</param>",
        );
        assert!(
            from_kpp(&kpp(&xml)).expect("decode").dropped.is_empty(),
            "{:?}",
            from_kpp(&kpp(&xml)).unwrap().dropped
        );

        // One axis ticked is a real mirror, and it is still named.
        let mirrored = xml.replace(
            "name=\"HorizontalMirrorEnabled\">false",
            "name=\"HorizontalMirrorEnabled\">true",
        );
        assert_eq!(
            from_kpp(&kpp(&mirrored)).expect("decode").dropped,
            ["mirrored dabs"]
        );
    }

    #[test]
    fn a_rake_and_a_fringe_are_told_apart() {
        let with = |sensor: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"0.2\" hfade=\"0\"/></Brush>"
                ),
                param("RotationSensor", &format!("<params id=\"{sensor}\"/>")),
                "<param type=\"internal\" name=\"PressureRotation\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode").brush
        };
        assert!(with("drawingangle").dab_angle_follows_stroke);
        assert_eq!(with("fuzzy").dab_angle_jitter, 360.0);
        assert!(!with("fuzzy").dab_angle_follows_stroke);
    }

    /// A rotation sensor spelled the compound way.
    ///
    /// Krita writes `<params id="sensorslist">` with a `<ChildSensor>` per
    /// input when a dynamic is driven by more than one. Reading only the first
    /// id gives "sensorslist", which matches neither branch above, so five
    /// presets in the fetched packs — GDQuest's cloud and rock brushes among
    /// them — imported as stamps that all lie the same way up.
    #[test]
    fn a_compound_rotation_sensor_is_read_through() {
        let with = |sensors: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"0.2\" hfade=\"0\"/></Brush>"
                ),
                param(
                    "RotationSensor",
                    &format!("<params id=\"sensorslist\">{sensors}</params>")
                ),
                "<param type=\"internal\" name=\"PressureRotation\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode")
        };

        let fuzzy = with("<ChildSensor id=\"fuzzy\"/><ChildSensor id=\"pressure\"/>");
        assert_eq!(fuzzy.brush.dab_angle_jitter, 360.0);
        assert!(fuzzy.dropped.is_empty(), "{:?}", fuzzy.dropped);

        // Both at once is a rake that also rolls, which Umber states natively:
        // the jitter is an offset on whichever angle applies.
        let both = with("<ChildSensor id=\"fuzzy\"/><ChildSensor id=\"drawingangle\"/>");
        assert!(both.brush.dab_angle_follows_stroke);
        assert_eq!(both.brush.dab_angle_jitter, 360.0);
    }

    /// The rake's lean.
    ///
    /// Krita adds `angleOffset` to the drawing angle inside the sensor, so it
    /// travels with the heading and lands on `dab_angle`, which Umber adds to
    /// the heading in exactly the same place. Four presets in the fetched packs
    /// state one, between 92° and 139°, and without it every one of them drags
    /// its bristles along the stroke instead of across it.
    #[test]
    fn a_rake_keeps_the_angle_it_leans_at() {
        let with = |offset: &str, angle: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    &format!(
                        "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"{angle}\">\
                         <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"0.2\" \
                         hfade=\"0\"/></Brush>"
                    )
                ),
                param(
                    "RotationSensor",
                    &format!(
                        "<params id=\"drawingangle\" angleOffset=\"{offset}\" \
                         fanCornersEnabled=\"0\"/>"
                    )
                ),
                "<param type=\"internal\" name=\"PressureRotation\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode").brush
        };

        let leaning = with("92", "0");
        assert!(leaning.dab_angle_follows_stroke);
        assert!((leaning.dab_angle - 92.0).abs() < 1e-3, "{leaning:?}");

        // The tip's own angle and the sensor's offset are two terms of one sum,
        // and both are measured from the heading once the dab follows it.
        let both = with("30", "1.5707963");
        assert!((both.dab_angle - 120.0).abs() < 0.1, "{}", both.dab_angle);

        // An offset means nothing when the dab does not follow the stroke:
        // Krita never reads the sensor, so neither does this.
        assert_eq!(with("0", "0").dab_angle, 0.0);
    }

    /// A rotation switched on and driven by something no desktop pointer
    /// reports is a dynamic that does nothing, and the generator has to know.
    /// Two of Revoy's stamps rotate by tilt direction and four of GDQuest's by
    /// barrel rotation.
    #[test]
    fn a_rotation_umber_cannot_drive_is_named() {
        let with = |sensor: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"0.2\" hfade=\"0\"/></Brush>"
                ),
                param("RotationSensor", &format!("<params id=\"{sensor}\"/>")),
                "<param type=\"internal\" name=\"PressureRotation\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode").dropped
        };

        for sensor in ["ascension", "rotation", "pressure"] {
            assert!(
                with(sensor).contains(&"dab rotation driven by tilt, pen rotation or pressure"),
                "{sensor} should be named: {:?}",
                with(sensor)
            );
        }
        assert!(with("drawingangle").is_empty());
        assert!(with("fuzzy").is_empty());
    }

    /// An 8-bit greyscale PNG of the given texels, which is what every pattern
    /// in the fetched packs turns out to be.
    fn grey_pattern(width: u32, height: u32, texels: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(texels).expect("data");
        drop(writer);
        out
    }

    /// Base64, the encoder to [`base64`]'s decoder. Fifteen lines and used by
    /// the tests alone, for the reason the decoder is hand-written: a
    /// dependency in the engine crate has to buy more than this.
    fn to_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut block = [0u8; 3];
            block[..chunk.len()].copy_from_slice(chunk);
            let n = u32::from_be_bytes([0, block[0], block[1], block[2]]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// The `Texture/Pattern/Pattern` param as Krita writes it: the picture's
    /// base64, base64'd again by the properties configuration around it.
    fn embedded_pattern(png: &[u8]) -> String {
        param(
            "Texture/Pattern/Pattern",
            &to_base64(to_base64(png).as_bytes()),
        )
    }

    /// A textured preset, with whatever extra settings the caller wants.
    fn textured(extra: &str) -> Vec<u8> {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{extra}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
            ),
            internal("Texture/Pattern/Enabled", "true"),
        );
        kpp(&xml)
    }

    /// A paper texture is read under the key Krita actually writes.
    ///
    /// Krita states every texture setting under `Texture/Pattern/`, and this
    /// reader looked for `Texture/Enabled` — a key nothing writes. So the
    /// option was never seen at all, for the shipped library or for a user's
    /// import, and thirty-one textured presets read as plain ones. Asserting
    /// the flag alone would have passed under the old spelling too, so the
    /// second half is the one that matters: the key Krita does not write must
    /// change nothing.
    #[test]
    fn a_paper_texture_is_read_under_the_key_krita_actually_writes() {
        let png = grey_pattern(2, 2, &[255, 128, 128, 255]);

        let read = from_kpp(&textured(&embedded_pattern(&png))).expect("decode");
        assert!(read.dropped.is_empty(), "{:?}", read.dropped);
        assert_eq!(read.paper.expect("tile").coverage(), [255, 128, 128, 255]);
        assert_eq!(read.brush.grain, 1.0);

        // The spelling this reader used to look for. Krita has never written
        // it, so a brush that says only this has no texture at all — no paper
        // and, since nothing was asked for, nothing lost.
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"1\"/></Brush>"
            ),
            internal("Texture/Enabled", "true"),
            embedded_pattern(&png),
        );
        let old_key = from_kpp(&kpp(&xml)).expect("decode");
        assert!(old_key.dropped.is_empty());
        assert!(old_key.paper.is_none());
        assert_eq!(old_key.brush.grain, 0.0);

        // And a texture switched on at no strength is neither paper nor a loss.
        // Krita leaves the option's settings in the file either way, which is
        // the trap `ScatterValue` and `SharpnessValue` are both read against.
        for key in ["Texture/Strength/Value", "Texture/Pattern/Strength"] {
            let off = from_kpp(&textured(&format!(
                "{}{}",
                embedded_pattern(&png),
                param(key, "0")
            )))
            .expect("decode");
            assert!(off.dropped.is_empty(), "{key}: {:?}", off.dropped);
            assert!(off.paper.is_none(), "{key}");
            assert_eq!(off.brush.grain, 0.0, "{key}");
        }

        // A strength between the two is the strength, and 0.45 is what one
        // fetched preset actually states.
        let faint = from_kpp(&textured(&format!(
            "{}{}",
            embedded_pattern(&png),
            param("Texture/Strength/Value", "0.45")
        )))
        .expect("decode");
        assert!(faint.paper.is_some());
        assert!((faint.brush.grain - 0.45).abs() < 1e-6);
    }

    /// The strength and the tile travel together or neither does.
    ///
    /// A pattern this reader cannot reach leaves `BrushPreset::paper` naming
    /// nothing, and `Editor::paper_tile` then paints **flat** — which is the
    /// exact identity and the right answer. Keeping the strength would make
    /// that brush claim a grain it does not have; substituting one of Umber's
    /// own papers would put a texture the author never chose into every mark,
    /// which is the fault that made a Clip Studio import paint at 78% of its
    /// stated opacity.
    #[test]
    fn a_pattern_this_reader_cannot_reach_paints_flat_and_says_so() {
        let named = param("Texture/Pattern/PatternFileName", "elsewhere.png");
        let missing = from_kpp(&textured(&named)).expect("decode");
        assert_eq!(missing.dropped, vec![MISSING_PATTERN]);
        assert!(missing.paper.is_none());
        assert_eq!(missing.brush.grain, 0.0);

        // The same preset with the file supplied beside it: no loss, and the
        // tile is the picture. This is the `.bundle`'s `patterns/` route, and
        // eleven of Revoy's presets take it.
        let png = grey_pattern(1, 1, &[64]);
        let found = from_kpp_in(
            &textured(&named),
            &Sidecar {
                brushes: &|_| None,
                patterns: &|wanted| (wanted == "elsewhere.png").then(|| png.clone()),
            },
        )
        .expect("decode");
        assert!(found.dropped.is_empty(), "{:?}", found.dropped);
        assert_eq!(found.paper.expect("tile").coverage(), [64]);

        // Krita records whatever path the pattern had on its author's machine —
        // `/home/raghu/kf5/inst/share/krita/patterns/07_big-grain.png` is in the
        // fetched packs — and a bundle holds it under the bare name. Reading
        // the path through would find nothing; reading it as a *path* would let
        // a stranger's preset name a file outside the pack.
        let absolute = param(
            "Texture/Pattern/PatternFileName",
            "/home/somebody/.local/share/krita/patterns/elsewhere.png",
        );
        let resolved = from_kpp_in(
            &textured(&absolute),
            &Sidecar {
                brushes: &|_| None,
                patterns: &|wanted| (wanted == "elsewhere.png").then(|| png.clone()),
            },
        )
        .expect("decode");
        assert!(resolved.paper.is_some(), "{:?}", resolved.dropped);
    }

    /// Multiply is the one texturing mode Umber's grain *is*, and every other
    /// one is named rather than approximated.
    ///
    /// Krita's Multiply against the dab's alpha is
    /// `alpha × (mask × strength + (1 − strength))`, which is
    /// `mix(1.0, tile, strength)` written out. Subtract — the largest group in
    /// the fetched packs, fourteen presets — is `alpha − mask`, and no tile
    /// makes a multiply do that: at half coverage through a half-lit texel the
    /// two differ by a quarter of the mark.
    #[test]
    fn only_krita_multiply_is_a_grain_umber_can_paint() {
        let png = grey_pattern(1, 1, &[128]);
        let with_mode = |mode: &str| {
            from_kpp(&textured(&format!(
                "{}{}",
                embedded_pattern(&png),
                param("Texture/Pattern/TexturingMode", mode)
            )))
            .expect("decode")
        };

        let multiply = with_mode("0");
        assert!(multiply.dropped.is_empty());
        assert!(multiply.paper.is_some());

        // 1 is Subtract, 6 a dodge, 11 Hard Mix and 12 Height — the four other
        // modes the fetched packs actually use.
        for mode in ["1", "6", "11", "12"] {
            let other = with_mode(mode);
            assert_eq!(other.dropped, vec![OTHER_TEXTURE_MODE], "mode {mode}");
            assert!(other.paper.is_none(), "mode {mode}");
            assert_eq!(other.brush.grain, 0.0, "mode {mode}");
        }
    }

    /// Krita's levels pipeline is baked into the stored tile, and each step of
    /// it is pinned against `KisTextureMaskInfo::recalculateMask` by hand.
    ///
    /// Baked rather than carried, because every one of these is a pure function
    /// of one texel's grey: a shader would pay for it on every fragment of
    /// every dab for ever, and `Brush` would grow six `Copy` fields and six
    /// controls describing a picture rather than a brush.
    #[test]
    fn the_patterns_levels_are_baked_into_the_tile() {
        let png = grey_pattern(4, 1, &[0, 64, 192, 255]);
        let baked = |settings: &str| {
            from_kpp(&textured(&format!("{}{settings}", embedded_pattern(&png))))
                .expect("decode")
                .paper
                .expect("tile")
                .coverage()
                .to_vec()
        };

        // No levels at all is the picture, byte for byte. Krita's defaults are
        // brightness 0, contrast 1, neutral point a half and no cutoff, and all
        // four have to be the identity or every untweaked pattern moves.
        assert_eq!(baked(""), [0, 64, 192, 255]);

        // Brightness is *subtracted*, which is the way round Krita has it — so
        // a positive brightness darkens the paper and bites harder. Two fetched
        // presets sit at −0.1 and −0.29, which lighten.
        assert_eq!(
            baked(&param("Texture/Pattern/Brightness", "-0.2")),
            [51, 115, 243, 255]
        );

        // Contrast pivots about a half.
        assert_eq!(
            baked(&param("Texture/Pattern/Contrast", "0.5")),
            [64, 96, 160, 191]
        );

        // Invert is the complement, and it is eleven of the thirty-one.
        assert_eq!(
            baked(&internal("Texture/Pattern/Invert", "true")),
            [255, 191, 63, 0]
        );

        // Cutoff policy 1 makes everything outside the window all pit, and
        // policy 2 makes it all paper. Policy 0 — twenty-two of the
        // thirty-one — is off, whatever the window says. The window is stated
        // in eight-bit levels and compared against the value as a fraction, so
        // 64 and 192 sit inside 50..=200 and the two ends do not.
        let window = format!(
            "{}{}",
            param("Texture/Pattern/CutoffLeft", "50"),
            param("Texture/Pattern/CutoffRight", "200")
        );
        assert_eq!(
            baked(&format!(
                "{window}{}",
                param("Texture/Pattern/CutoffPolicy", "1")
            )),
            [0, 64, 192, 0]
        );
        assert_eq!(
            baked(&format!(
                "{window}{}",
                param("Texture/Pattern/CutoffPolicy", "2")
            )),
            [255, 64, 192, 255]
        );
        assert_eq!(
            baked(&format!(
                "{window}{}",
                param("Texture/Pattern/CutoffPolicy", "0")
            )),
            [0, 64, 192, 255]
        );

        // The neutral point stretches the two halves of the range separately,
        // and its default of a half is the identity in both — which is what
        // makes the twenty presets that never state it read correctly.
        assert_eq!(
            baked(&param("Texture/Pattern/NeutralPoint", "0.5")),
            [0, 64, 192, 255]
        );
        assert_eq!(
            baked(&param("Texture/Pattern/NeutralPoint", "0.25")),
            [0, 128, 213, 255]
        );
    }

    /// The tile is stored at the pattern's own resolution and the *scale*
    /// becomes the size of one tile in document pixels.
    ///
    /// Krita resamples the pattern and tiles the result 1:1; Umber stores the
    /// picture and lets the sampler stretch it, which is the same tiling
    /// without a second resampler in `umber-core` to keep in step with the
    /// hardware's. A 512-texel pattern at Krita's 0.37 is a 189-pixel tile, and
    /// reading the scale as anything else makes a paper twice as fine or twice
    /// as coarse as its author's — which is the whole of what a grain looks
    /// like.
    #[test]
    fn the_patterns_scale_becomes_the_tile_size_in_document_pixels() {
        let png = grey_pattern(64, 32, &[200; 64 * 32]);
        let at = |scale: &str| {
            from_kpp(&textured(&format!(
                "{}{}",
                embedded_pattern(&png),
                param("Texture/Pattern/Scale", scale)
            )))
            .expect("decode")
            .brush
            .grain_scale
        };

        // The longer side, because `Brush::grain_scale` is one number and the
        // shader stretches the tile square over it.
        assert_eq!(at("1"), 64.0);
        assert_eq!(at("2"), 128.0);
        assert!((at("0.5") - 32.0).abs() < 1e-6);

        // And it is bounded by what a grain scale may be, rather than trusting
        // a number out of somebody else's file.
        assert_eq!(at("0.001"), Brush::MIN_GRAIN_SCALE);
        assert_eq!(at("100"), Brush::MAX_GRAIN_SCALE);

        // The tile itself is the pattern, unresampled.
        let read = from_kpp(&textured(&embedded_pattern(&png))).expect("decode");
        let tile = read.paper.expect("tile");
        assert_eq!((tile.width(), tile.height()), (64, 32));
    }

    /// A texture strength that follows pressure is named, and one that does not
    /// is left alone.
    ///
    /// Umber's grain is one number for the whole stroke, so a curve that
    /// actually moves is a real difference — the paper appearing as the hand
    /// presses. A curve that does not move is Krita's editor leaving a default
    /// behind, which is the judgement `Dynamic::from_samples` already makes,
    /// and naming it would cry wolf on a brush that paints identically.
    #[test]
    fn a_texture_strength_that_follows_pressure_is_named() {
        let png = grey_pattern(1, 1, &[128]);
        let with_curve = |use_curve: &str, curve: &str| {
            from_kpp(&textured(&format!(
                "{}{}{}",
                embedded_pattern(&png),
                internal("Texture/Strength/UseCurve", use_curve),
                param("Texture/Strength/commonCurve", curve)
            )))
            .expect("decode")
        };

        let ramp = with_curve("true", "0,0;1,1;");
        assert_eq!(ramp.dropped, vec![PAPER_UNDER_PRESSURE]);
        // The paper still comes across: the brush paints its author's grain,
        // and what it loses is only that the grain does not fade at a light
        // touch. That is an approximation, which is why it keeps the tile —
        // unlike the mode, which keeps nothing.
        assert!(ramp.paper.is_some());
        assert_eq!(ramp.brush.grain, 1.0);

        assert!(with_curve("true", "0,1;1,1;").dropped.is_empty());
        // Switched off, the curve in the file is not a curve Krita applies.
        assert!(with_curve("false", "0,0;1,1;").dropped.is_empty());
    }

    /// One sentence at most, and the mode wins.
    ///
    /// A Subtract texture reproduces nothing whatever its pattern does, so a
    /// second sentence about its pressure curve would be noise on top of a loss
    /// already named — the rule the library generator follows when it asks what
    /// was dropped before it asks about the mask.
    #[test]
    fn a_texture_umber_cannot_paint_is_named_once() {
        let png = grey_pattern(1, 1, &[128]);
        let both = from_kpp(&textured(&format!(
            "{}{}{}{}",
            embedded_pattern(&png),
            param("Texture/Pattern/TexturingMode", "1"),
            internal("Texture/Strength/UseCurve", "true"),
            param("Texture/Strength/commonCurve", "0,0;1,1;")
        )))
        .expect("decode");
        assert_eq!(both.dropped, vec![OTHER_TEXTURE_MODE]);
    }

    /// All three of PNG's text chunks turn up in one real pack, and a reader
    /// that knows only `zTXt` silently rejects a quarter of it. This is not a
    /// hypothetical: eleven of Revoy's forty-six presets are `iTXt`, and they
    /// were the eleven that failed before this was fixed.
    #[test]
    fn the_settings_are_found_in_any_of_pngs_three_text_chunks() {
        for chunk in ["zTXt", "tEXt", "iTXt"] {
            let preset =
                from_kpp(&kpp_with(chunk, &oval())).unwrap_or_else(|e| panic!("{chunk}: {e}"));
            assert_eq!(preset.name, "Basic Oval", "{chunk}");
            assert_eq!(preset.brush.size, 40.0, "{chunk}");
        }
    }

    /// `tEXt` and `zTXt` are defined as Latin-1 and Krita writes UTF-8 into
    /// them anyway, so both spellings of an accented name have to come out
    /// right. Reading the UTF-8 one as Latin-1 gives "AÃ©rographe" in the
    /// picker; reading the Latin-1 one as UTF-8 gives a replacement character.
    #[test]
    fn an_accented_name_survives_whichever_way_it_was_written() {
        let xml = oval().replace("Basic Oval", "Aérographe fin");

        // What Krita does: UTF-8 bytes in a chunk that claims Latin-1. `png`'s
        // encoder writes one byte per char, so the fixture has to hand it the
        // mojibake spelling for the bytes to come out right.
        let smuggled: String = xml.bytes().map(|b| b as char).collect();
        for chunk in ["zTXt", "tEXt"] {
            let preset =
                from_kpp(&kpp_with(chunk, &smuggled)).unwrap_or_else(|e| panic!("{chunk}: {e}"));
            assert_eq!(preset.name, "Aérographe fin", "{chunk}");
        }

        // A writer that really did mean Latin-1 must not be mangled either.
        for chunk in ["zTXt", "tEXt"] {
            let preset =
                from_kpp(&kpp_with(chunk, &xml)).unwrap_or_else(|e| panic!("{chunk}: {e}"));
            assert_eq!(preset.name, "Aérographe fin", "{chunk}");
        }

        // `iTXt` is UTF-8 by definition and must skip the recovery entirely.
        let preset = from_kpp(&kpp_with("iTXt", &xml)).expect("iTXt");
        assert_eq!(preset.name, "Aérographe fin");
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_panic() {
        assert!(from_kpp(b"not a png").is_err());
        // A PNG with no preset chunk is a picture, not a brush.
        let mut plain = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut plain, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[1]).expect("data");
        }
        assert!(from_kpp(&plain).is_err());
        assert!(from_kpp(&kpp("<Preset name=\"X\"/>")).is_err());
        assert!(from_kpp(&kpp("not xml at all <<<")).is_err());
        assert!(dropped_features(b"rubbish").is_empty());
    }

    /// A `brush_definition` around one mask generator, with everything the
    /// reader needs and nothing else.
    fn with_generator(generator: &str) -> Vec<u8> {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                &format!(
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\" \
                     density=\"1\" randomness=\"0\">{generator}</Brush>"
                )
            ),
        );
        kpp(&xml)
    }

    /// The polarity, pinned against the numbers the shipped packs actually
    /// carry. Every value below is copied out of a real preset, and every one
    /// of them was imported inside out until this was checked against Krita's
    /// own preview images — see the module docs for the table.
    #[test]
    fn fade_is_hardness_and_not_softness() {
        let hardness = |generator: &str| {
            from_kpp(&with_generator(generator))
                .expect("decode")
                .brush
                .hardness
        };

        // GDQuest "Ink Brush" and Raghukamath "Inkbrush": both draw a crisp
        // black line, and both used to import at hardness 0.0 — a cloud.
        assert!(
            (hardness(
                "<MaskGenerator id=\"default\" type=\"circle\" diameter=\"30\" \
                 ratio=\"1\" hfade=\"1\" vfade=\"1\" spikes=\"2\"/>"
            ) - 1.0)
                .abs()
                < 1e-5
        );
        // Deevad "Eraser Kneaded Soft": a wide feathered fade, and the name
        // says so. It used to import at hardness 1.0.
        assert!(
            hardness(
                "<MaskGenerator id=\"gauss\" type=\"circle\" diameter=\"250\" \
                 ratio=\"1\" hfade=\"0\" vfade=\"0\" spikes=\"2\"/>"
            ) < 1e-5
        );
        // Raghukamath "Basic Render": plainly soft in its preview, and between
        // the two extremes rather than at one of them.
        let render = hardness(
            "<MaskGenerator id=\"default\" type=\"circle\" diameter=\"95.81\" \
             ratio=\"1\" hfade=\"0.67\" vfade=\"0.67\" spikes=\"2\"/>",
        );
        assert!((render - 0.67).abs() < 1e-5, "{render}");
    }

    /// `id="soft"` is a different generator: its shape is the `softness_curve`
    /// and its `hfade` is a leftover the editor wrote and never reads. Reading
    /// the field would make GDQuest's airbrush — whose curve never exceeds 0.4
    /// even at the centre — a hard disc.
    #[test]
    fn a_softness_curve_beats_the_fade_field_beside_it() {
        // GDQuest "Airbrush", verbatim.
        let airbrush = from_kpp(&with_generator(
            "<MaskGenerator id=\"soft\" type=\"circle\" diameter=\"440\" ratio=\"1\" \
             hfade=\"0\" vfade=\"0\" spikes=\"2\" \
             softness_curve=\"0,0.39911;0.429719,0.118523;1,0;\"/>",
        ))
        .expect("decode");
        assert!(
            airbrush.brush.hardness < 1e-5,
            "{}",
            airbrush.brush.hardness
        );

        // Raghukamath "Basic": full coverage in the middle falling to nothing
        // at the rim, with `hfade="1"` sitting beside it saying "hard".
        let basic = from_kpp(&with_generator(
            "<MaskGenerator id=\"soft\" type=\"circle\" diameter=\"20\" ratio=\"1\" \
             hfade=\"1\" vfade=\"1\" spikes=\"2\" \
             softness_curve=\"0,1;0.562249,0.721362;1,0;\"/>",
        ))
        .expect("decode");
        assert!(
            basic.brush.hardness > 0.2 && basic.brush.hardness < 0.6,
            "{}",
            basic.brush.hardness
        );
    }

    /// `ScatterValue` sits at Krita's default of 1 in a preset that never
    /// scatters, so the enable flag is the whole of the answer. Reading the
    /// value alone gave GDQuest's "Ink Brush" a five-radius spray.
    #[test]
    fn scatter_needs_kritas_own_switch_as_well_as_its_value() {
        let with = |on: bool, value: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                     </Brush>"
                ),
                param("ScatterValue", value),
                param("ScatterSensor", "<params id=\"pressure\"/>"),
                internal("PressureScatter", on),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush
        };
        // GDQuest "Ink Brush": switched off, value left at 5.
        assert_eq!(with(false, "5").scatter, 0.0);
        // Raghukamath "Dots": switched on, and it really does scatter.
        assert!((with(true, "0.1").scatter - 0.1).abs() < 1e-5);
    }

    /// Krita has spelled the airbrush option two ways and both are in the
    /// fetched packs. Knowing one silently imports an airbrush as an ordinary
    /// distance-driven brush.
    #[test]
    fn both_of_kritas_airbrush_spellings_are_read() {
        let with = |prefix: &str, rate: &str| {
            let switch = internal(&format!("{prefix}/isAirbrushing"), true);
            let speed = param(&format!("{prefix}/rate"), rate);
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{switch}{speed}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                     </Brush>"
                ),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush.dabs_per_second
        };
        assert_eq!(with("PaintOpSettings", "50"), 50.0);
        // GDQuest "Airbrush" asks for 1000 a second; the ceiling is Umber's.
        assert_eq!(with("AirbrushOption", "1000"), 300.0);
    }

    /// Krita's Sharpness thresholds the mask into a hard, aliased edge, which
    /// is the whole of a pixel-art brush and something Umber cannot do. It has
    /// to be *named*, or such a preset ships as a soft blob under its author's
    /// name.
    #[test]
    fn edge_sharpening_is_named_rather_than_quietly_dropped() {
        let with = |on: bool, value: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"1\" type=\"circle\" ratio=\"1\" hfade=\"1\" \
                     id=\"gauss\"/></Brush>"
                ),
                internal("PressureSharpness", on),
                param("SharpnessValue", value),
            );
            from_kpp(&kpp(&xml)).expect("decode").dropped
        };
        // GDQuest "PixelArt OnePixel".
        assert!(with(true, "1").contains(&"edge sharpening"));
        // Switched on with nothing behind it changes no pixel.
        assert!(!with(true, "0").contains(&"edge sharpening"));
        assert!(!with(false, "1").contains(&"edge sharpening"));
    }

    /// Raghukamath's "Dots" spaces its dabs 5.12 diameters apart, and the
    /// spacing *is* the brush. A ceiling of 4 pulled them 20% closer.
    #[test]
    fn kritas_whole_spacing_range_survives() {
        let brush = from_kpp(&with_generator(
            "<MaskGenerator id=\"gauss\" type=\"circle\" diameter=\"26.89\" \
             ratio=\"1\" hfade=\"1\" vfade=\"1\" spikes=\"2\"/>",
        ))
        .expect("decode");
        assert_eq!(brush.brush.spacing, 0.1);

        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"5.12\" angle=\"0\">\
                 <MaskGenerator diameter=\"26.89\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                 </Brush>"
            ),
        );
        assert_eq!(from_kpp(&kpp(&xml)).expect("decode").brush.spacing, 5.12);
    }

    #[test]
    fn base64_round_trips_and_refuses_rubbish() {
        for original in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            &[0u8, 255, 128, 3][..],
        ] {
            let encoded = encode_base64(original);
            match base64(&encoded) {
                Some(back) => assert_eq!(back, original),
                None => assert!(original.is_empty()),
            }
        }
        // Whitespace is how a long resource is wrapped in the file.
        assert_eq!(base64("aGVs\n bG8=").unwrap(), b"hello");
        assert!(base64("not base64 !!").is_none());
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut word = 0u32;
            for (i, b) in chunk.iter().enumerate() {
                word |= (*b as u32) << (16 - 8 * i);
            }
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((word >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}
