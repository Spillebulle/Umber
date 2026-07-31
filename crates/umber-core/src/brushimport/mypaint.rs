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
//! | `hardness` | `hardness` | direct; the falloff *shape* differs |
//! | `opaque` × `opaque_multiply` | `opacity`, `opacity_curve` | see below |
//! | `dabs_per_actual_radius`, `dabs_per_basic_radius` | `spacing` | |
//! | `eraser` | `mode` | |
//! | `slow_tracking` | `stabilization` | ordering preserved, feel differs |
//!
//! Dropped, and where that shows:
//!
//! - **`elliptical_dab_ratio` / `elliptical_dab_angle`** — Umber's dab is a
//!   circle. Flat, chisel and calligraphy brushes import as round ones and lose
//!   their line-weight variation entirely. This is the largest single loss;
//!   about a quarter of the MyPaint set is elliptical.
//! - **`offset_by_random`, `radius_by_random`, `offset_by_speed`** — no scatter
//!   or jitter, so spray, splatter and "bulk" brushes come out as smooth lines.
//! - **`smudge`, `smudge_length`, `smudge_radius_log`** — Umber's dab pass
//!   writes coverage into a scratch texture and never reads the layer, so a
//!   brush cannot pick colour up off the canvas. A smudge brush imported here
//!   would paint solid colour instead of blending, which is why the library
//!   generator refuses to ship them.
//! - **`colorize`, `lock_alpha`, `change_color_*`** — no per-dab colour
//!   modulation; the stroke is one colour by construction (the scratch texture
//!   is `R8Unorm` coverage, which is a deliberate 4× bandwidth saving).
//! - **`dabs_per_second`** — Umber's dab loop is driven by distance, so a brush
//!   that keeps depositing paint while the pen is stationary has no equivalent.
//! - **`opaque_linearize`** — MyPaint uses it to compensate for dabs
//!   compounding as they overlap. Umber's wet layer takes a `max` of coverage,
//!   so there is nothing to compensate for.
//! - **`tracking_noise`, `direction_filter`, `snap_to_pixel`, `anti_aliasing`,
//!   `stroke_*`, `custom_input`, `speed*`, `pressure_gain_log`** — no
//!   equivalent, and no visible loss for most brushes.
//!
//! Inputs other than `pressure` (`speed1`, `speed2`, `random`, `stroke`,
//! `direction`, `tilt`, …) are ignored. Umber's `Brush` has exactly two
//! pressure-driven parameters, so there is nowhere to put them.

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
        let mut points = [0.0f32; ResponseCurve::N];
        for (i, r) in radii.iter().enumerate() {
            points[i] = ((r - r_min) / span).clamp(0.0, 1.0);
        }
        ((r_min / r_max).clamp(0.01, 1.0), ResponseCurve { points })
    } else {
        (Brush::default().min_size_ratio, ResponseCurve::LINEAR)
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
    // that use `dabs_per_basic_radius`. `dabs_per_second` has no equivalent at
    // all and is ignored.
    let per_radius =
        file.setting("dabs_per_actual_radius").base + file.setting("dabs_per_basic_radius").base;
    let spacing = if per_radius > 0.0 {
        (1.0 / (2.0 * per_radius)).clamp(0.01, 0.5)
    } else {
        Brush::default().spacing
    };

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
        hardness: file.setting("hardness").base.clamp(0.0, 1.0),
        opacity,
        spacing,
        pressure_size: varies,
        pressure_opacity: opacity_varies,
        size_curve,
        opacity_curve,
        stabilization,
        mode,
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
    // Half a smudge still reads as a blending brush; below that the colour
    // pickup is a texture effect rather than the point of the brush.
    if file.setting("smudge").base >= 0.5 {
        reasons.push("smudge");
    }
    if file.setting("colorize").base >= 0.5 {
        reasons.push("colorize");
    }
    if file.setting("lock_alpha").base >= 0.5 {
        reasons.push("lock_alpha");
    }
    // A dab drawn only every so many milliseconds, with no distance term, would
    // import as a solid line at Umber's default spacing.
    let per_radius =
        file.setting("dabs_per_actual_radius").base + file.setting("dabs_per_basic_radius").base;
    if per_radius <= 0.0 && file.setting("dabs_per_second").base > 0.0 {
        reasons.push("dabs_per_second only");
    }
    Ok(reasons)
}

/// The five pressures a [`ResponseCurve`] samples at.
fn sample_points() -> impl Iterator<Item = f32> {
    (0..ResponseCurve::N).map(ResponseCurve::x_of)
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
                pressure: s.inputs.get("pressure").map(Vec::as_slice).unwrap_or(&[]),
            },
            None => Setting {
                base: 0.0,
                has_pressure: false,
                pressure: &[],
            },
        }
    }
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

    #[test]
    fn smudge_brushes_are_flagged_as_unrenderable() {
        let smudger = r#"{ "version": 3, "settings": {
            "smudge": { "base_value": 1.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        assert_eq!(unsupported_features(smudger).unwrap(), vec!["smudge"]);
        assert!(unsupported_features(PEN).unwrap().is_empty());
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
