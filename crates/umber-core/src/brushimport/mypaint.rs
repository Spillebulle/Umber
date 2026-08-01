//! MyPaint `.myb` brushes.
//!
//! A `.myb` file is JSON. Each entry under `settings` is
//! `{ "base_value": f, "inputs": { "<input>": [[x, y], ...] } }`, and MyPaint
//! evaluates a setting as
//!
//! ```text
//! value(inputs) = base_value + Σ mapping_i(input_i)
//! ```
//!
//! — the mappings are **added** to the base, not multiplied by it. Getting that
//! backwards is the single easiest way to mis-import a brush, because it looks
//! plausible for the handful of settings whose base value happens to be 1.
//!
//! # What survives the conversion, and what does not
//!
//! MyPaint's brush model is much richer than Umber's parametric round brush.
//! The mappings below are chosen to preserve *what a stroke looks like* as far
//! as the model allows; everything else is dropped, and dropped loudly here
//! rather than quietly at the call site.
//!
//! Carried across:
//!
//! | MyPaint | Umber | Notes |
//! |---|---|---|
//! | `radius_logarithmic` | `size`, `min_size_ratio`, `size_curve` | exact, see below |
//! | `hardness` | `hardness`, `min_hardness_ratio`, `hardness_curve` | the falloff *shape* differs |
//! | `opaque` × `opaque_multiply` | `opacity`, `opacity_curve` | see below |
//! | `dabs_per_actual_radius`, `dabs_per_basic_radius` | `spacing` | |
//! | `eraser` | `mode` | |
//! | `slow_tracking` | `stabilization` | ordering preserved, feel differs |
//! | `smudge`, `smudge_length`, `smudge_radius_log` | `smudge`, `smudge_length`, `smudge_radius` | the sample lags a frame or two |
//! | `dabs_per_second` | `dabs_per_second` | direct |
//! | `elliptical_dab_ratio` | `dab_ratio` | base value only, see below |
//! | `elliptical_dab_angle` | `dab_angle`, `dab_angle_follows_stroke`, `dab_angle_jitter` | a `direction` input becomes "follows the stroke", a `random` one becomes jitter |
//! | `offset_by_random` | `scatter`, `min_scatter_ratio`, `scatter_curve` | pressure mapping carried |
//! | `radius_by_random` | `radius_jitter` | base value only |
//!
//! Three settings — `radius_logarithmic`, `hardness` and `offset_by_random` —
//! are read across the five pressures a [`ResponseCurve`] samples at rather
//! than off the base value, because MyPaint states most of what a brush *does*
//! as a mapping on top of a base of zero. Umber's form is `peak × (min_ratio +
//! (1 - min_ratio) × curve(p))`, which reproduces the mapping exactly at those
//! five points for a monotonic one and degrades gracefully otherwise.
//!
//! Dropped, and where that shows:
//!
//! - **`elliptical_dab_ratio` driven by an input.** 46 brushes map it, and 15
//!   of those state a round base, so they arrive round. It is deliberately not
//!   approximated by a constant taken from the mapping: the inputs it is
//!   actually driven by are `random` (16), `speed1` (14), `stroke` (9),
//!   `pressure` (8) and `tilt_declination` (7), and for three of those five the
//!   input sits at its neutral on a desktop with a mouse — where the base value
//!   is exactly what MyPaint would render. Substituting the mapping's peak
//!   would make those brushes wrong in a new way rather than right.
//! - **`radius_by_random` driven by an input** — 9 brushes, all with a round
//!   base and all but one driven by `custom` or `attack_angle`, neither of
//!   which Umber has.
//! - **`offset_by_speed`** — scatter that grows with how fast the pen is
//!   moving. 14 brushes; the constant part of their scatter is imported, the
//!   speed-reactive part is not, so a fast flick spreads less than it should.
//! - **`colorize`, `change_color_*`** — the dab pass modulates a colour, it does
//!   not recolour what is under it, so a brush that shifts the hue of the paint
//!   beneath has no equivalent. Two brushes name `colorize` and both leave it
//!   at zero.
//! - **`lock_alpha`** — painting only where the layer already has coverage
//!   needs the layer read at composite time as a mask; the stroke scratch has
//!   no channel for it. No brush in the pack sets it.
//! - **`opaque_linearize`** — MyPaint uses it to compensate for dabs
//!   compounding as they overlap. Umber's wet layer takes a `max` of coverage,
//!   so there is nothing to compensate for.
//! - **`paint_mode`** — MyPaint 2's spectral pigment mixing, a different
//!   colour model rather than a brush setting. 19 brushes ask for it.
//! - **`tracking_noise`, `direction_filter`, `snap_to_pixel`, `anti_aliasing`,
//!   `stroke_*`, `custom_input`, `speed*`, `pressure_gain_log`** — no
//!   equivalent, and no visible loss for most brushes.
//!
//! Of MyPaint's inputs, `pressure` is read as a curve, `direction` and `random`
//! are read on the dab angle only, and `speed1`, `speed2`, `stroke`, `tilt_*`,
//! `custom`, `brush_radius` and the rest are ignored. Umber's `Brush` is a
//! `Copy` struct of fixed-size curves, so every input it gains costs a curve on
//! every brush.

use std::collections::HashMap;

use serde::Deserialize;

use crate::brush::{Brush, BrushMode};
use crate::curve::ResponseCurve;
use crate::preset::PresetError;

/// `.myb` versions this understands. Version 3 is what MyPaint 1.2 onwards
/// writes and what every brush in the vendored packs uses; version 2 is the
/// same JSON with fewer settings. Versions below 2 are a line-based text format
/// that no maintained pack still ships, so they are refused rather than guessed
/// at.
const SUPPORTED_VERSIONS: std::ops::RangeInclusive<u32> = 2..=3;

/// Convert the contents of a `.myb` file into a brush.
pub fn from_myb(json: &str) -> Result<Brush, PresetError> {
    let file: MybFile =
        serde_json::from_str(json).map_err(|e| PresetError::Malformed(None, e.to_string()))?;
    if !SUPPORTED_VERSIONS.contains(&file.version) {
        return Err(PresetError::UnsupportedVersion(None, file.version));
    }

    let radius = file.setting("radius_logarithmic");
    let opaque = file.setting("opaque");
    let opaque_multiply = file.setting("opaque_multiply");

    // --- size ---------------------------------------------------------------
    //
    // `radius_logarithmic` is the natural log of the dab radius in pixels, and
    // the pressure mapping is an offset *in log space*, so the radius at
    // pressure p is exp(base + map(p)). Reading the base value as a radius
    // directly is the classic mistake: it would make a 2.6 px pen 0.96 px wide.
    let radius_at = |p: f32| radius.value_at(p).exp();
    let radii: Vec<f32> = sample_points().map(radius_at).collect();
    let r_max = radii.iter().copied().fold(f32::MIN, f32::max);
    let r_min = radii.iter().copied().fold(f32::MAX, f32::min);

    // Umber's radius is `size * 0.5 * (min_ratio + (1 - min_ratio) * curve(p))`.
    // Taking `size` from the widest sampled radius and normalising the rest
    // against it reproduces MyPaint's radius exactly at the five sample points
    // for the usual monotonically-increasing case, and degrades gracefully for
    // the rare brush whose radius *falls* with pressure.
    let size = (2.0 * r_max).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE);
    let span = r_max - r_min;
    let varies = radius.has_pressure && span > r_max * 0.01;

    let (min_size_ratio, size_curve) = if varies {
        (
            (r_min / r_max).clamp(0.01, 1.0),
            normalised_curve(&radii, r_min, r_max),
        )
    } else {
        (Brush::default().min_size_ratio, ResponseCurve::LINEAR)
    };

    // --- hardness -----------------------------------------------------------
    //
    // The third pressure dynamic, and by count the most used: 69 of the pack's
    // 196 brushes map hardness onto pressure, more than map scatter or shape.
    // Reading only the base value made every one of them stamp the same edge
    // whatever the hand was doing, which is most of the difference between a
    // pencil that feathers and one that rules lines.
    let hardness = file.setting("hardness");
    let hardnesses: Vec<f32> = sample_points()
        .map(|p| hardness.value_at(p).clamp(0.0, 1.0))
        .collect();
    let h_max = hardnesses.iter().copied().fold(f32::MIN, f32::max);
    let h_min = hardnesses.iter().copied().fold(f32::MAX, f32::min);
    // An absolute threshold, not a relative one: a brush whose hardness runs
    // 0.02..0.05 varies by 150% and is soft mush at both ends.
    let hardness_varies = hardness.has_pressure && h_max > 0.0 && h_max - h_min > 0.02;
    let (min_hardness_ratio, hardness_curve) = if hardness_varies {
        (
            (h_min / h_max).clamp(0.0, 1.0),
            normalised_curve(&hardnesses, h_min, h_max),
        )
    } else {
        (Brush::default().min_hardness_ratio, ResponseCurve::LINEAR)
    };

    // --- opacity ------------------------------------------------------------
    //
    // MyPaint multiplies the two settings together: `alpha = opaque *
    // opaque_multiply`. Nearly every real brush leaves `opaque_multiply` at a
    // base of 0 and puts a pressure mapping on it, which is how MyPaint spells
    // "pressure drives opacity" — so the multiplier at full pressure, not the
    // base value, is the one that matters.
    let multipliers: Vec<f32> = sample_points()
        .map(|p| opaque_multiply.value_at(p))
        .collect();
    let m_max = multipliers.iter().copied().fold(f32::MIN, f32::max);

    // `opaque` has a range of 0..2 in MyPaint, where anything above 1 exists to
    // push a low-coverage dab back up to solid. Umber's opacity is a plain
    // fraction, so it clamps.
    let opacity = if m_max > 0.0 {
        (opaque.base * m_max).clamp(0.0, 1.0)
    } else {
        // A multiplier that never rises above zero would mean an invisible
        // brush. Treat it as "no opacity dynamics" rather than importing
        // something that paints nothing.
        opaque.base.clamp(0.0, 1.0)
    };

    let opacity_varies = opaque_multiply.has_pressure
        && m_max > 0.0
        && multipliers.iter().copied().fold(f32::MAX, f32::min) < m_max * 0.99;
    let opacity_curve = if opacity_varies {
        let mut points = [0.0f32; ResponseCurve::N];
        for (i, m) in multipliers.iter().enumerate() {
            points[i] = (m / m_max).clamp(0.0, 1.0);
        }
        ResponseCurve { points }
    } else {
        ResponseCurve::LINEAR
    };

    // --- spacing ------------------------------------------------------------
    //
    // MyPaint states dab density as dabs per radius travelled, summed over a
    // term scaled by the *current* radius and one scaled by the *base* radius.
    // Umber has a single spacing expressed as a fraction of the diameter, so
    // the two terms are added as though the radii were equal — true at full
    // pressure, and increasingly wrong at light pressure for the few brushes
    // that use `dabs_per_basic_radius`.
    let per_radius =
        file.setting("dabs_per_actual_radius").base + file.setting("dabs_per_basic_radius").base;
    let spacing = if per_radius > 0.0 {
        (1.0 / (2.0 * per_radius)).clamp(0.01, 0.5)
    } else {
        Brush::default().spacing
    };

    // `dabs_per_second` carries straight across — Umber's dab loop now has a
    // time term of its own. A brush with *no* distance term is an airbrush and
    // depends on this entirely; one with both gets both, as MyPaint does.
    let dabs_per_second = file.setting("dabs_per_second").base.clamp(0.0, 300.0);

    // --- smudge -------------------------------------------------------------
    //
    // MyPaint samples the canvas under the dab and mixes it into the colour it
    // deposits; Umber does the same, a frame or two behind because its read is
    // asynchronous. `smudge_radius_log` is a natural log like `radius_
    // logarithmic`, and is a multiplier on the dab radius rather than a radius
    // in pixels — reading it as pixels would make every blender canvas-wide.
    let smudge = file.setting("smudge").base.clamp(0.0, 1.0);
    let smudge_length = file.setting("smudge_length").base.clamp(0.0, 0.99);
    let smudge_radius = file
        .setting("smudge_radius_log")
        .base
        .exp()
        .clamp(0.25, 8.0);

    // --- dab shape ----------------------------------------------------------
    //
    // MyPaint's dab is an ellipse whose *long* axis is the radius and whose
    // short axis is `radius / elliptical_dab_ratio`, tilted by
    // `elliptical_dab_angle` degrees. Umber's is the same, so these carry
    // across directly. The ratio is documented as >= 1.0 and a few brushes
    // state slightly less, which would turn the dab inside out.
    let dab_ratio = file.setting("elliptical_dab_ratio").base.clamp(1.0, 20.0);
    let angle = file.setting("elliptical_dab_angle");
    let dab_angle = angle.base.rem_euclid(360.0);

    // A brush whose angle is driven by the `direction` input turns to follow
    // the stroke — a rake or a fan. One with a fixed angle is a broad nib, and
    // holding its angle through a curve is what makes calligraphy thick and
    // thin. Reading a rake as a nib, or the reverse, is immediately visible.
    let dab_angle_follows_stroke = angle.has_direction;

    // A `random` mapping on the angle is the third case, and the pack's most
    // common shape mapping after direction: 31 brushes ask for it and 29 of
    // those have an elongated dab, so ignoring it turned a watercolour fringe,
    // a charcoal and a grain brush into combs — every stamp lying the same way
    // down the stroke. MyPaint's `random` input runs 0..1, so the span of the
    // mapping *is* the full width of the rotation, in degrees.
    let dab_angle_jitter = angle.random_span.clamp(0.0, 360.0);

    // `offset_by_random` is a standard deviation in "basic radius" units, and
    // `radius_by_random` one in log-radius — both exactly what `Brush` holds.
    // These are what make a spray can spray and a charcoal catch on the paper;
    // without them those brushes import as smooth, even lines.
    //
    // Scatter is read across the pressure samples rather than off the base
    // value. 38 brushes map it, and 16 state *only* the mapping — a base of
    // zero and the whole of the scatter on pressure — so reading the base alone
    // imported them as clean lines. Negative offsets are clamped away: MyPaint
    // treats the setting as a magnitude, and a mapping that dips below zero
    // means "no scatter here", not "scatter the other way".
    let offset = file.setting("offset_by_random");
    let scatters: Vec<f32> = sample_points()
        .map(|p| offset.value_at(p).clamp(0.0, 8.0))
        .collect();
    let s_max = scatters.iter().copied().fold(f32::MIN, f32::max);
    let s_min = scatters.iter().copied().fold(f32::MAX, f32::min);
    let scatter = s_max;
    // Absolute, because the interesting case is a brush that scatters by 0.4
    // radii at one end and not at all at the other — no relative threshold can
    // see that without also firing on rounding noise near zero.
    let pressure_scatter = offset.has_pressure && s_max > 0.0 && s_max - s_min > 0.01;
    let (min_scatter_ratio, scatter_curve) = if pressure_scatter {
        (s_min / s_max, normalised_curve(&scatters, s_min, s_max))
    } else {
        (Brush::default().min_scatter_ratio, ResponseCurve::LINEAR)
    };

    let radius_jitter = file.setting("radius_by_random").base.clamp(0.0, 3.0);

    // --- the rest -----------------------------------------------------------
    let mode = if file.setting("eraser").base >= 0.5 {
        BrushMode::Erase
    } else {
        BrushMode::Paint
    };

    // MyPaint's `slow_tracking` runs 0..10 and damps over *time*; Umber's
    // stabilisation is a per-sample exponential factor in 0..1. `s / (s + 1)`
    // is the fixed point of "one sample of smoothing at strength s", which
    // preserves the ordering of the brushes even though the damping cannot be
    // identical. A stabilisation of 1.0 would never reach the pointer, hence
    // the ceiling — which matches the app's own slider range.
    let slow = file.setting("slow_tracking").base.max(0.0);
    let stabilization = (slow / (slow + 1.0)).clamp(0.0, 0.95);

    Ok(Brush {
        size,
        min_size_ratio,
        hardness: h_max,
        opacity,
        spacing,
        pressure_size: varies,
        pressure_opacity: opacity_varies,
        pressure_hardness: hardness_varies,
        size_curve,
        opacity_curve,
        hardness_curve,
        min_hardness_ratio,
        stabilization,
        mode,
        smudge,
        smudge_length,
        smudge_radius,
        dabs_per_second,
        dab_ratio,
        dab_angle,
        dab_angle_follows_stroke,
        dab_angle_jitter,
        scatter,
        pressure_scatter,
        min_scatter_ratio,
        scatter_curve,
        radius_jitter,
        // MyPaint composites every dab, so it is tempting to import all 196 of
        // these with build-up on. It would be wrong, and mostly a no-op.
        //
        // Umber applies `Brush::opacity` once at commit, so an ordinary MyPaint
        // dab arrives here with a per-dab coverage of exactly 1.0 — and
        // building up from 1.0 is the same as taking a max of it. The only
        // brushes it would touch are the ones whose coverage genuinely varies
        // per dab, where it would deepen a light pressure ramp into something
        // MyPaint does not draw either, because MyPaint's build-up is on
        // *opacity* and Umber's opacity is not in the dab. Build-up earns its
        // keep where a dab is sparse by construction — a bitmap tip, or grain —
        // which is precisely what a `.myb` never has.
        build_up: false,
        // MyPaint has no paper texture. The `.gbr` packs do.
        grain: 0.0,
        grain_scale: Brush::default().grain_scale,
        grain_pattern: Brush::default().grain_pattern,
    })
}

/// True if this brush leans on something Umber cannot render, so importing it
/// would produce a stroke that does not resemble the original.
///
/// This is not used by [`from_myb`] — a caller who asks for a specific file
/// should get it — but the library generator uses it to keep brushes that would
/// misrepresent themselves out of the shipped set. A "Blender" that paints
/// solid colour is worse than no Blender at all.
pub fn unsupported_features(json: &str) -> Result<Vec<&'static str>, PresetError> {
    let file: MybFile =
        serde_json::from_str(json).map_err(|e| PresetError::Malformed(None, e.to_string()))?;
    let mut reasons = Vec::new();
    // `smudge` and `dabs_per_second` used to be listed here. Both are now
    // rendered — colour pickup through the stroke's own colour scratch, timed
    // dabs through a time term in the dab loop — which is what let the shipped
    // library go from 128 brushes to all 196.
    if file.setting("colorize").base >= 0.5 {
        reasons.push("colorize");
    }
    if file.setting("lock_alpha").base >= 0.5 {
        reasons.push("lock_alpha");
    }
    Ok(reasons)
}

/// The five pressures a [`ResponseCurve`] samples at.
fn sample_points() -> impl Iterator<Item = f32> {
    (0..ResponseCurve::N).map(ResponseCurve::x_of)
}

/// Rescale five sampled values onto the curve's `0..=1`.
///
/// Umber states every pressure dynamic as `peak × (min_ratio + (1 - min_ratio)
/// × curve(p))`, so the curve carries only the *shape* and the two ratios carry
/// the range. Written once because size, hardness and scatter all do it, and
/// three copies of the same normalisation is three chances to get one of them
/// backwards.
fn normalised_curve(values: &[f32], min: f32, max: f32) -> ResponseCurve {
    let span = max - min;
    let mut points = [0.0f32; ResponseCurve::N];
    if span <= 0.0 {
        return ResponseCurve::LINEAR;
    }
    for (point, value) in points.iter_mut().zip(values) {
        *point = ((value - min) / span).clamp(0.0, 1.0);
    }
    ResponseCurve { points }
}

#[derive(Deserialize)]
struct MybFile {
    version: u32,
    #[serde(default)]
    settings: HashMap<String, MybSetting>,
}

impl MybFile {
    /// Settings absent from the file take MyPaint's default of zero. That is
    /// the right answer for every setting this importer reads except `hardness`
    /// and `opaque`, and no real `.myb` omits those — MyPaint writes the full
    /// table every time it saves.
    fn setting(&self, name: &str) -> Setting<'_> {
        match self.settings.get(name) {
            Some(s) => Setting {
                base: s.base_value,
                has_pressure: s
                    .inputs
                    .get("pressure")
                    .is_some_and(|points| points.len() >= 2),
                has_direction: s
                    .inputs
                    .get("direction")
                    .is_some_and(|points| points.len() >= 2),
                random_span: span(s.inputs.get("random")),
                pressure: s.inputs.get("pressure").map(Vec::as_slice).unwrap_or(&[]),
            },
            None => Setting {
                base: 0.0,
                has_pressure: false,
                has_direction: false,
                random_span: 0.0,
                pressure: &[],
            },
        }
    }
}

/// How far a mapping's output travels from end to end.
///
/// MyPaint's editor writes a two-point mapping for every input a brush has ever
/// touched, most of them flat — 24 of the 55 brushes that "map"
/// `elliptical_dab_ratio` map it to a constant zero. A flat mapping contributes
/// nothing, so measuring the span rather than the presence of points is what
/// separates a real setting from an editor artefact.
fn span(points: Option<&Vec<(f32, f32)>>) -> f32 {
    let Some(points) = points else { return 0.0 };
    if points.len() < 2 {
        return 0.0;
    }
    let lo = points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let hi = points.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    (hi - lo).max(0.0)
}

#[derive(Deserialize)]
struct MybSetting {
    #[serde(default)]
    base_value: f32,
    #[serde(default)]
    inputs: HashMap<String, Vec<(f32, f32)>>,
}

struct Setting<'a> {
    base: f32,
    has_pressure: bool,
    /// Whether the setting is driven by stroke direction, which is how MyPaint
    /// spells "this dab turns to follow the line".
    has_direction: bool,
    /// How far the `random` mapping's output travels, in the setting's own
    /// units. Zero when there is no such mapping or it is flat.
    random_span: f32,
    /// Piecewise-linear control points, x ascending, in MyPaint's input units —
    /// for pressure that is already 0..1.
    pressure: &'a [(f32, f32)],
}

impl Setting<'_> {
    /// The setting's value at a given pressure: base plus the mapping's output.
    fn value_at(&self, p: f32) -> f32 {
        self.base + piecewise(self.pressure, p)
    }
}

/// Evaluate MyPaint's piecewise-linear mapping, holding the end values outside
/// the control points' range. An empty or single-point mapping contributes
/// nothing, which matches MyPaint refusing to treat one point as a curve.
fn piecewise(points: &[(f32, f32)], x: f32) -> f32 {
    if points.len() < 2 {
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
        let (x0, y0) = pair[0];
        let (x1, y1) = pair[1];
        if x <= x1 {
            // MyPaint allows two points to share an x, which is how a brush
            // spells a step. Guard the division rather than producing NaN.
            if (x1 - x0).abs() < f32::EPSILON {
                return y1;
            }
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    last.1
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `classic/pen` from mypaint-brushes 2.0.2, trimmed to the settings this
    /// importer reads. Kept inline rather than read off disk so the test still
    /// means something on a checkout where the packs were never fetched.
    const PEN: &str = r#"{
        "version": 3,
        "settings": {
            "hardness": { "base_value": 0.9, "inputs": {} },
            "opaque": { "base_value": 1.0, "inputs": {} },
            "opaque_multiply": { "base_value": 0.0, "inputs": {
                "pressure": [[0.0, 0.0], [0.015, 0.0], [0.015, 1.0], [1.0, 1.0]] } },
            "dabs_per_actual_radius": { "base_value": 2.2, "inputs": {} },
            "dabs_per_basic_radius": { "base_value": 0.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 0.96, "inputs": {
                "pressure": [[0.0, 0.0], [1.0, 0.5]],
                "speed1": [[0.0, -0.0], [1.0, -0.15]] } },
            "slow_tracking": { "base_value": 0.65, "inputs": {} },
            "eraser": { "base_value": 0.0, "inputs": {} }
        }
    }"#;

    /// `classic/eraser`, same treatment.
    const ERASER: &str = r#"{
        "version": 3,
        "settings": {
            "eraser": { "base_value": 1.0, "inputs": {} },
            "hardness": { "base_value": 0.5, "inputs": {} },
            "opaque": { "base_value": 1.0, "inputs": {} },
            "opaque_multiply": { "base_value": 1.0, "inputs": {} },
            "dabs_per_actual_radius": { "base_value": 3.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.5, "inputs": {} },
            "slow_tracking": { "base_value": 0.0, "inputs": {} }
        }
    }"#;

    /// A soft airbrush shape: big, soft, opacity entirely on pressure.
    const AIRBRUSH: &str = r#"{
        "version": 3,
        "settings": {
            "hardness": { "base_value": 0.1, "inputs": {} },
            "opaque": { "base_value": 0.4, "inputs": {} },
            "opaque_multiply": { "base_value": 0.0, "inputs": {
                "pressure": [[0.0, 0.0], [0.5, 0.25], [1.0, 1.0]] } },
            "dabs_per_actual_radius": { "base_value": 6.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 3.0, "inputs": {} },
            "slow_tracking": { "base_value": 4.0, "inputs": {} },
            "smudge": { "base_value": 0.0, "inputs": {} }
        }
    }"#;

    #[test]
    fn a_pen_imports_with_a_believable_shape() {
        let b = from_myb(PEN).expect("import");

        // radius_logarithmic is a natural log: the pen is exp(0.96) = 2.61 px
        // in radius at zero pressure and exp(0.96 + 0.5) = 4.31 at full, so the
        // diameter Umber stores is 8.62 px, not 0.96 or 1.92.
        assert!((b.size - 8.61).abs() < 0.05, "size was {}", b.size);
        assert!(b.pressure_size);
        assert!(
            (b.min_size_ratio - 0.6065).abs() < 0.01,
            "min ratio was {}",
            b.min_size_ratio
        );
        assert!((b.hardness - 0.9).abs() < 1e-5);
        assert!((b.opacity - 1.0).abs() < 1e-5);
        // 2.2 dabs per radius is 1 / (2 * 2.2) of a diameter.
        assert!((b.spacing - 0.2273).abs() < 0.001, "spacing {}", b.spacing);
        assert_eq!(b.mode, BrushMode::Paint);
    }

    #[test]
    fn the_imported_radius_matches_mypaint_at_every_sample() {
        // The whole point of the size/min_ratio/curve triple: rebuilding the
        // radius through Umber's own `radius_at` must land back on
        // exp(base + map(p)).
        let b = from_myb(PEN).expect("import");
        for i in 0..=4 {
            let p = i as f32 / 4.0;
            let expected = (0.96 + 0.5 * p).exp();
            let got = b.radius_at(p);
            assert!(
                (got - expected).abs() < 0.02,
                "at pressure {p}: expected {expected}, got {got}"
            );
        }
    }

    #[test]
    fn an_eraser_imports_as_an_eraser() {
        let b = from_myb(ERASER).expect("import");
        assert_eq!(b.mode, BrushMode::Erase);
        // exp(2.5) = 12.18 radius, so a 24.4 px eraser.
        assert!((b.size - 24.36).abs() < 0.1, "size was {}", b.size);
        // No pressure mapping on the radius: the size must not vary.
        assert!(!b.pressure_size);
        assert!(!b.pressure_opacity);
        assert!((b.stabilization - 0.0).abs() < 1e-6);
    }

    #[test]
    fn an_airbrush_keeps_its_opacity_curve() {
        let b = from_myb(AIRBRUSH).expect("import");
        assert!(b.pressure_opacity);
        assert!((b.opacity - 0.4).abs() < 1e-5, "opacity {}", b.opacity);
        // The mapping is 0 → 0, 0.5 → 0.25, 1 → 1: strongly eased in, which is
        // what makes an airbrush feel like one.
        assert!(b.opacity_curve.sample(0.5) < 0.3);
        assert!((b.opacity_curve.sample(1.0) - 1.0).abs() < 1e-5);
        assert!((b.opacity_curve.sample(0.0)).abs() < 1e-5);
        // slow_tracking 4 → 4/5.
        assert!((b.stabilization - 0.8).abs() < 1e-5);
    }

    #[test]
    fn stroke_opacity_is_never_folded_into_the_curve() {
        // The wet-layer invariant: `coverage_at` must not carry `opacity`, or
        // overlapping dabs start compounding again.
        let b = from_myb(AIRBRUSH).expect("import");
        assert!((b.coverage_at(1.0) - 1.0).abs() < 1e-5);
        assert!(b.opacity < 1.0);
    }

    #[test]
    fn every_imported_brush_is_within_umbers_limits() {
        for json in [PEN, ERASER, AIRBRUSH] {
            let b = from_myb(json).expect("import");
            assert!((Brush::MIN_SIZE..=Brush::MAX_SIZE).contains(&b.size));
            assert!((0.0..=1.0).contains(&b.opacity));
            assert!((0.0..=1.0).contains(&b.hardness));
            assert!((0.0..=1.0).contains(&b.min_size_ratio));
            assert!(b.stabilization < 1.0);
            assert!(b.step_at(0.0) > 0.0);
        }
    }

    #[test]
    fn a_step_mapping_does_not_produce_nan() {
        // The pen's opacity mapping has two points at x = 0.015.
        let b = from_myb(PEN).expect("import");
        assert!(b.opacity_curve.points.iter().all(|p| p.is_finite()));
        assert!(b.size_curve.points.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let json = r#"{ "version": 99, "settings": {} }"#;
        assert!(matches!(
            from_myb(json),
            Err(PresetError::UnsupportedVersion(_, 99))
        ));
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_panic() {
        assert!(from_myb("not json at all").is_err());
        assert!(from_myb("{}").is_err(), "a missing version is an error");
    }

    /// Smudge used to be the single biggest reason a brush was refused — 67 of
    /// the 68 the generator turned away. The engine renders it now, so the
    /// check must *not* flag it, and the brush must arrive with the settings
    /// that make it blend rather than with them quietly zeroed.
    #[test]
    fn a_smudge_brush_is_imported_rather_than_refused() {
        let smudger = r#"{ "version": 3, "settings": {
            "smudge": { "base_value": 1.0, "inputs": {} },
            "smudge_length": { "base_value": 0.4, "inputs": {} },
            "smudge_radius_log": { "base_value": 0.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        assert!(unsupported_features(smudger).unwrap().is_empty());

        let brush = from_myb(smudger).expect("a smudge brush now imports");
        assert!(brush.smudges());
        assert!((brush.smudge - 1.0).abs() < 1e-5);
        assert!((brush.smudge_length - 0.4).abs() < 1e-5);
        // exp(0) = 1: the log is a multiplier on the dab radius, not a radius.
        assert!((brush.smudge_radius - 1.0).abs() < 1e-5);

        assert!(unsupported_features(PEN).unwrap().is_empty());
        assert!(!from_myb(PEN).unwrap().smudges(), "a pen does not blend");
    }

    /// A chisel is not a circle. `elliptical_dab_ratio` is the single most
    /// common thing a MyPaint brush has that a round dab cannot express — 78 of
    /// the pack's 196 use it — and until the dab could be an ellipse, every one
    /// of them imported as a round brush with no line-weight variation at all.
    #[test]
    fn an_elliptical_brush_keeps_its_shape_and_its_angle() {
        let chisel = r#"{ "version": 3, "settings": {
            "elliptical_dab_ratio": { "base_value": 4.0, "inputs": {} },
            "elliptical_dab_angle": { "base_value": 45.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(chisel).expect("imports");
        assert!(brush.is_shaped());
        assert!((brush.dab_ratio - 4.0).abs() < 1e-5);
        assert!((brush.dab_angle - 45.0).abs() < 1e-5);
        // No `direction` input, so it holds its angle through a curve — that is
        // what makes a broad nib produce calligraphic thick-and-thin.
        assert!(!brush.dab_angle_follows_stroke);

        assert!(!from_myb(PEN).unwrap().is_shaped(), "a pen is round");
    }

    /// The other half of the same setting. A rake keeps its bristles across the
    /// line of travel; a nib does not. Reading one as the other is immediately
    /// visible in a curve.
    #[test]
    fn a_direction_input_makes_the_dab_follow_the_stroke() {
        let rake = r#"{ "version": 3, "settings": {
            "elliptical_dab_ratio": { "base_value": 6.0, "inputs": {} },
            "elliptical_dab_angle": { "base_value": 0.0,
                "inputs": { "direction": [[0.0, 0.0], [180.0, 180.0]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        assert!(from_myb(rake).unwrap().dab_angle_follows_stroke);
    }

    /// Scatter is what makes a spray can spray. Without it those brushes are
    /// smooth lines wearing the name of something granular.
    #[test]
    fn a_scattering_brush_keeps_its_randomness() {
        let spray = r#"{ "version": 3, "settings": {
            "offset_by_random": { "base_value": 2.5, "inputs": {} },
            "radius_by_random": { "base_value": 0.6, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(spray).expect("imports");
        assert!(brush.is_shaped());
        assert!((brush.scatter - 2.5).abs() < 1e-5);
        assert!((brush.radius_jitter - 0.6).abs() < 1e-5);
    }

    /// MyPaint documents the ratio as >= 1.0 and a few brushes state less.
    /// Below 1.0 the long and short axes swap, so the dab would come out
    /// perpendicular to the angle the file asked for.
    #[test]
    fn a_ratio_below_one_is_clamped_rather_than_inverting_the_dab() {
        let odd = r#"{ "version": 3, "settings": {
            "elliptical_dab_ratio": { "base_value": 0.25, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        assert_eq!(from_myb(odd).unwrap().dab_ratio, 1.0);
    }

    /// The other refusal: an airbrush states its rate in dabs per *second* and
    /// gives no distance term at all, so before the dab loop had a clock these
    /// imported as a single mark.
    #[test]
    fn a_timed_brush_keeps_its_rate() {
        let airbrush = r#"{ "version": 3, "settings": {
            "dabs_per_second": { "base_value": 80.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        assert!(unsupported_features(airbrush).unwrap().is_empty());

        let brush = from_myb(airbrush).expect("an airbrush now imports");
        assert!(brush.is_timed());
        assert!((brush.dabs_per_second - 80.0).abs() < 1e-5);
        assert!(
            !from_myb(PEN).unwrap().is_timed(),
            "a pen is distance-driven"
        );
    }

    /// The pack's most common shape mapping after `direction`. Without it the
    /// 29 elongated brushes that ask for a random angle stamp every dab the
    /// same way up, which reads as machined ruling rather than as grain.
    #[test]
    fn a_random_angle_mapping_becomes_dab_jitter() {
        let fringe = r#"{ "version": 3, "settings": {
            "elliptical_dab_ratio": { "base_value": 10.0, "inputs": {} },
            "elliptical_dab_angle": { "base_value": 0.0,
                "inputs": { "random": [[0.0, -180.0], [1.0, 180.0]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(fringe).expect("imports");
        assert!((brush.dab_angle_jitter - 360.0).abs() < 1e-4);
        // Random is not direction: this dab points anywhere, it does not lie
        // along the line of travel.
        assert!(!brush.dab_angle_follows_stroke);
    }

    /// MyPaint's editor writes a two-point mapping for every input a brush has
    /// ever been shown, and most of them are flat. Reading "has points" as
    /// "is driven" would give a third of the pack a jitter of zero degrees
    /// spelled as a live setting, and would have made the counts meaningless.
    #[test]
    fn a_flat_mapping_is_not_a_setting() {
        let inert = r#"{ "version": 3, "settings": {
            "elliptical_dab_ratio": { "base_value": 4.0, "inputs": {} },
            "elliptical_dab_angle": { "base_value": 30.0,
                "inputs": { "random": [[0.0, 0.0], [1.0, 0.0]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(inert).expect("imports");
        assert_eq!(brush.dab_angle_jitter, 0.0);
        assert!((brush.dab_angle - 30.0).abs() < 1e-5);
    }

    /// 16 brushes state no constant scatter at all and put the whole of it on
    /// pressure. Reading the base value alone imported them as clean lines.
    #[test]
    fn scatter_stated_only_as_a_pressure_mapping_still_arrives() {
        let dry = r#"{ "version": 3, "settings": {
            "offset_by_random": { "base_value": 0.0,
                "inputs": { "pressure": [[0.0, 0.0], [1.0, 1.4]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(dry).expect("imports");
        assert!(brush.pressure_scatter);
        assert!((brush.scatter - 1.4).abs() < 1e-5, "{}", brush.scatter);
        assert_eq!(brush.min_scatter_ratio, 0.0);
        assert!(brush.scatter_at(0.0).abs() < 1e-5);
        assert!((brush.scatter_at(1.0) - 1.4).abs() < 1e-4);
        assert!(brush.is_shaped());
    }

    /// The other direction, and the more common one: a pencil that lays a
    /// solid line when pressed and skips across the tooth when it is not.
    #[test]
    fn scatter_that_falls_with_pressure_keeps_its_shape() {
        let pencil = r#"{ "version": 3, "settings": {
            "offset_by_random": { "base_value": 1.6,
                "inputs": { "pressure": [[0.0, 0.0], [1.0, -1.4]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(pencil).expect("imports");
        assert!(brush.pressure_scatter);
        assert!((brush.scatter - 1.6).abs() < 1e-5);
        for i in 0..=4 {
            let p = i as f32 / 4.0;
            let want = 1.6 - 1.4 * p;
            let got = brush.scatter_at(p);
            assert!(
                (got - want).abs() < 0.01,
                "at pressure {p}: expected {want}, got {got}"
            );
        }
    }

    /// A mapping that dips below zero means "no scatter here". Letting it
    /// through would make `scatter` negative and the ratio nonsense.
    #[test]
    fn a_negative_scatter_mapping_clamps_to_none() {
        let odd = r#"{ "version": 3, "settings": {
            "offset_by_random": { "base_value": 0.5,
                "inputs": { "pressure": [[0.0, -3.0], [1.0, 0.0]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(odd).expect("imports");
        assert!(brush.scatter >= 0.0);
        assert!(brush.min_scatter_ratio >= 0.0);
        for i in 0..=4 {
            assert!(brush.scatter_at(i as f32 / 4.0) >= 0.0);
        }
    }

    /// The largest single dynamic in the pack: 69 of 196 brushes soften their
    /// edge under a light hand, and reading only the base made every one of
    /// them rule lines.
    #[test]
    fn hardness_follows_pressure_when_the_brush_asks() {
        let pencil = r#"{ "version": 3, "settings": {
            "hardness": { "base_value": 0.9,
                "inputs": { "pressure": [[0.0, -0.6], [1.0, 0.0]] } },
            "opaque": { "base_value": 1.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(pencil).expect("imports");
        assert!(brush.pressure_hardness);
        assert!((brush.hardness - 0.9).abs() < 1e-5);
        for i in 0..=4 {
            let p = i as f32 / 4.0;
            let want = 0.9 - 0.6 * (1.0 - p);
            let got = brush.hardness_at(p);
            assert!(
                (got - want).abs() < 0.01,
                "at pressure {p}: expected {want}, got {got}"
            );
        }
        // A brush with no mapping keeps its flat edge, and `hardness_at` must
        // return exactly the setting rather than a curve that nearly does.
        let pen = from_myb(PEN).expect("import");
        assert!(!pen.pressure_hardness);
        assert_eq!(pen.hardness_at(0.0), pen.hardness);
        assert_eq!(pen.hardness_at(1.0), pen.hardness);
    }

    /// MyPaint clamps hardness to 0..1 and several brushes state mappings that
    /// overshoot. Letting the peak through would produce a hardness above 1,
    /// which the shader reads as a divide-by-nothing edge.
    #[test]
    fn an_overshooting_hardness_mapping_stays_inside_the_range() {
        let loud = r#"{ "version": 3, "settings": {
            "hardness": { "base_value": 0.8,
                "inputs": { "pressure": [[0.0, -0.8], [1.0, 1.7]] } },
            "opaque": { "base_value": 1.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let brush = from_myb(loud).expect("imports");
        assert!((0.0..=1.0).contains(&brush.hardness));
        for i in 0..=4 {
            assert!((0.0..=1.0).contains(&brush.hardness_at(i as f32 / 4.0)));
        }
    }

    #[test]
    fn the_mapping_holds_its_end_values() {
        let points = [(0.2, 1.0), (0.8, 2.0)];
        assert_eq!(piecewise(&points, 0.0), 1.0);
        assert_eq!(piecewise(&points, 1.0), 2.0);
        assert!((piecewise(&points, 0.5) - 1.5).abs() < 1e-5);
        assert_eq!(piecewise(&[], 0.5), 0.0);
        assert_eq!(piecewise(&[(0.0, 5.0)], 0.5), 0.0);
    }
}
