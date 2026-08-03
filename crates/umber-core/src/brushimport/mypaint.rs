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
//! This importer evaluates that expression rather than reading base values.
//! [`MybFile::eval`] is the whole of it: every setting is read through it, with
//! the inputs held at the values a real stroke would produce.
//!
//! # Where each input goes
//!
//! | Input | Fate |
//! |---|---|
//! | `pressure` | read as a curve onto size, opacity, hardness and scatter; a [`Modulation`] onto anything else |
//! | `speed1`, `speed2` | [`Modulation`]s, on MyPaint's own log-speed scale |
//! | `stroke` | [`Modulation`]s; the ramp's length comes from `stroke_duration_logarithmic` |
//! | `random` | [`Modulation`]s, plus the two dedicated jitters below |
//! | `direction` | "the dab follows the stroke" on the angle, a [`Modulation`] elsewhere |
//! | `brush_radius` | **folded into the base value** — it is `radius_logarithmic`'s base, a constant |
//! | `viewzoom` | folded in at zero: Umber's brushes are zoom-independent by design |
//! | `custom`, `tilt_*`, `attack_angle`, `barrel_rotation`, `gridmap_*` | held at their neutral, which is where a desktop with a mouse leaves them |
//!
//! Holding an input at its neutral is not the same as ignoring the mapping:
//! `mapping(neutral)` is still added, so a brush whose whole tilt mapping sits
//! at −0.3 arrives 0.3 narrower, exactly as MyPaint would draw it on the same
//! machine.
//!
//! # What survives the conversion, and what does not
//!
//! | MyPaint | Umber | Notes |
//! |---|---|---|
//! | `radius_logarithmic` | `size`, `min_size_ratio`, `size_curve`, `Size` modulations | exact, see below |
//! | `hardness` | `hardness`, `min_hardness_ratio`, `hardness_curve`, `Hardness` modulations | the falloff *shape* differs |
//! | `opaque` × `opaque_multiply` | `opacity`, `opacity_curve`, `Opacity` modulations | see below |
//! | `dabs_per_actual_radius`, `dabs_per_basic_radius` | `spacing` | |
//! | `eraser` | `mode` | a threshold, not the fraction MyPaint blends |
//! | `slow_tracking` | `stabilization` | ordering preserved, feel differs |
//! | `smudge`, `smudge_length`, `smudge_radius_log` | `smudge`, `smudge_length`, `smudge_radius`, `Smudge` modulations | the sample lags a frame or two |
//! | `dabs_per_second` | `dabs_per_second` | direct |
//! | `elliptical_dab_ratio` | `dab_ratio`, `Ratio` modulations | |
//! | `elliptical_dab_angle` | `dab_angle`, `dab_angle_follows_stroke`, `dab_angle_jitter`, `Angle` modulations | a `direction` input becomes "follows the stroke", a `random` one becomes jitter |
//! | `offset_by_random` | `scatter`, `min_scatter_ratio`, `scatter_curve`, `Scatter` modulations | |
//! | `offset_by_speed` | `speed_offset` | a directed lead, not scatter |
//! | `radius_by_random` | `radius_jitter` | |
//! | `stroke_duration_logarithmic`, `stroke_holdtime` | `stroke_span`, `stroke_hold` | |
//! | `change_color_h` | `Hue` modulations | the *variation*, not the constant — see below |
//! | `change_color_v` + `change_color_l` | `Value` modulations | HSL lightness read as HSV value |
//! | `change_color_hsv_s` + `change_color_hsl_s` | `Saturation` modulations | likewise |
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
//! - **`custom_input`** and everything it drives. MyPaint's `custom` input is a
//!   low-passed copy of a setting that is itself mapped, so supporting it means
//!   supporting a second evaluation pass with its own filter. 74 mappings in
//!   the pack read it, but two thirds of those drive `offset_angle`,
//!   `smudge_bucket` and the rest of the Anti-Art offset machinery, which Umber
//!   has no equivalent for at all.
//! - **Tilt.** 28 mappings, and desktop reports tilt as `(0, 0)` regardless —
//!   see the pressure section of `CLAUDE.md`. Held at neutral, which is what
//!   MyPaint renders on the same machine.
//! - **`paint_mode`** — MyPaint 2's spectral pigment mixing, a different colour
//!   model rather than a brush setting. 19 brushes ask for it.
//! - **`offset_x/y`, `offset_angle*`, `offset_multiplier`, `gridmap_*`,
//!   `smudge_bucket`, `smudge_transparency`, `smudge_length_log`** — the
//!   Anti-Art extensions, present in 19 brushes and needing a dab that can be
//!   thrown to a computed place with its own colour bucket.
//! - **`colorize`, `lock_alpha`, `posterize`, `restore_color`** — all four
//!   change how a dab *composites* rather than what it is, which is the commit
//!   shader's business rather than the importer's. No brush in the pack sets
//!   any of them to a live value.
//! - **`eraser` as a fraction.** MyPaint scales a dab's target alpha by
//!   `1 - eraser`, so a brush can erase a bit; Umber's `mode` is a switch. Five
//!   brushes map it onto pressure and import as whichever side of 0.5 their
//!   base lands.
//! - **`opaque_linearize`** — MyPaint uses it to compensate for dabs
//!   compounding as they overlap. Umber's wet layer takes a `max` of coverage,
//!   so there is nothing to compensate for. 123 brushes set it and every one of
//!   them is *correct* to ignore.
//! - **`anti_aliasing`** — a minimum edge fadeout in pixels, which Umber's dab
//!   shader applies unconditionally and sizes from the dab's short axis. 100
//!   brushes set it; nothing is lost.
//! - **`color_h/s/v` and `restore_color`** — the colour the brush was saved
//!   with. 52 brushes carry one and it is simply whatever was on the palette
//!   that day; MyPaint only restores it when `restore_color` is set, which two
//!   brushes do.
//! - **`tracking_noise`, `slow_tracking_per_dab`, `direction_filter`,
//!   `snap_to_pixel`, `stroke_threshold`, `pressure_gain_log`,
//!   `speed*_gamma`, `speed*_slowness`** — either no equivalent or, for the
//!   speed constants, left at MyPaint's defaults by every brush in the pack and
//!   therefore constants here too (see [`crate::dynamics`]).
//! - **The alpha correction on `radius_by_random`.** MyPaint dims a dab that
//!   randomness made larger, by the square of the ratio, so a jittered stroke
//!   keeps its average density. Umber's `max` coverage has no per-dab density
//!   to keep.
//! - **Opacity build-up.** MyPaint composites each dab, so a low-opacity brush
//!   darkens as a stroke crosses itself. Umber takes a `max` of coverage across
//!   the whole stroke and applies opacity once at commit — that is the
//!   wet-layer design in `CLAUDE.md`.
//!
//! A mapping is evaluated exactly as `mypaint_mapping_calculate` does,
//! extrapolation of the end segments included — see [`piecewise`] for why that
//! detail is worth a paragraph of its own.

use std::collections::{BTreeMap, HashMap};

use serde::Deserialize;

use crate::brush::{Brush, BrushMode};
use crate::curve::ResponseCurve;
use crate::dynamics::{DabInput, DabTarget, Modulation, Modulations};
use crate::layer::BlendMode;
use crate::preset::PresetError;

/// `.myb` versions this understands. Version 3 is what MyPaint 1.2 onwards
/// writes and what every brush in the vendored packs uses; version 2 is the
/// same JSON with fewer settings. Versions below 2 are a line-based text format
/// that no maintained pack still ships, so they are refused rather than guessed
/// at.
const SUPPORTED_VERSIONS: std::ops::RangeInclusive<u32> = 2..=3;

/// Inputs that reach a target through the modulation table rather than through
/// a dedicated field. Pressure is absent because size, opacity, hardness and
/// scatter already read it as a curve; it is added back per target below where
/// there is no such field.
const EXTRA_INPUTS: [DabInput; 4] = [
    DabInput::Speed,
    DabInput::SlowSpeed,
    DabInput::Stroke,
    DabInput::Random,
];

/// Convert the contents of a `.myb` file into a brush.
pub fn from_myb(json: &str) -> Result<Brush, PresetError> {
    let file: MybFile =
        serde_json::from_str(json).map_err(|e| PresetError::Malformed(None, e.to_string()))?;
    if !SUPPORTED_VERSIONS.contains(&file.version) {
        return Err(PresetError::UnsupportedVersion(None, file.version));
    }

    // `brush_radius` is the brush's own base radius fed back as an input, so it
    // is a *constant* — libmypaint reads it as `BASEVAL(RADIUS_LOGARITHMIC)`.
    // Thirteen brushes map it, some over a range of seven log units, and every
    // one of them was importing at the wrong size because the contribution was
    // simply dropped.
    let env = Env::resting(file.base("radius_logarithmic"));
    let mut mods: Vec<Modulation> = Vec::new();

    // --- size ---------------------------------------------------------------
    //
    // `radius_logarithmic` is the natural log of the dab radius in pixels, and
    // the pressure mapping is an offset *in log space*, so the radius at
    // pressure p is exp(base + map(p)). Reading the base value as a radius
    // directly is the classic mistake: it would make a 2.6 px pen 0.96 px wide.
    let radius_at = |p: f32| {
        file.eval("radius_logarithmic", &env.with(DabInput::Pressure, p))
            .exp()
    };
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
    let varies = file.is_driven("radius_logarithmic", DabInput::Pressure) && span > r_max * 0.01;

    let (min_size_ratio, size_curve) = if varies {
        (
            (r_min / r_max).clamp(0.01, 1.0),
            normalised_curve(&radii, r_min, r_max),
        )
    } else {
        (Brush::default().min_size_ratio, ResponseCurve::LINEAR)
    };

    // A `random` mapping on the radius is what makes a spatter brush splotchy —
    // `deevad/splash` swings 1.6 log units, a 2.3× change in dab radius from
    // one stamp to the next — and it arrives here rather than as `radius_jitter`
    // because MyPaint draws it from the same uniform every other `random`
    // mapping on the brush reads, not from an independent gaussian.
    for input in EXTRA_INPUTS {
        mods.extend(modulation(
            &file,
            "radius_logarithmic",
            DabTarget::Size,
            input,
            &env,
        ));
    }

    // --- hardness -----------------------------------------------------------
    //
    // The third pressure dynamic, and by count the most used: 69 of the pack's
    // 196 brushes map hardness onto pressure, more than map scatter or shape.
    // Reading only the base value made every one of them stamp the same edge
    // whatever the hand was doing, which is most of the difference between a
    // pencil that feathers and one that rules lines.
    let hardness_at = |p: f32| {
        file.eval("hardness", &env.with(DabInput::Pressure, p))
            .clamp(0.0, 1.0)
    };
    let hardnesses: Vec<f32> = sample_points().map(hardness_at).collect();
    let h_max = hardnesses.iter().copied().fold(f32::MIN, f32::max);
    let h_min = hardnesses.iter().copied().fold(f32::MAX, f32::min);
    // An absolute threshold, not a relative one: a brush whose hardness runs
    // 0.02..0.05 varies by 150% and is soft mush at both ends.
    let hardness_varies =
        file.is_driven("hardness", DabInput::Pressure) && h_max > 0.0 && h_max - h_min > 0.02;
    let (min_hardness_ratio, hardness_curve) = if hardness_varies {
        (
            (h_min / h_max).clamp(0.0, 1.0),
            normalised_curve(&hardnesses, h_min, h_max),
        )
    } else {
        (Brush::default().min_hardness_ratio, ResponseCurve::LINEAR)
    };
    for input in EXTRA_INPUTS {
        mods.extend(modulation(
            &file,
            "hardness",
            DabTarget::Hardness,
            input,
            &env,
        ));
    }

    // --- opacity ------------------------------------------------------------
    //
    // libmypaint's own arithmetic, from `prepare_and_draw_dab`:
    //
    //     opaque = MAX(0, opaque);
    //     opaque = CLAMP(opaque * opaque_multiply, 0, 1);
    //
    // Note what is *not* there: neither setting is first clamped to the range
    // the editor shows for it. A brush whose `opaque` reaches 1.5 really does
    // reach full coverage at two thirds of its multiplier, and clamping it to
    // 1.0 on the way past would under-paint it.
    //
    // Reading `opaque`'s base value alone — which this used to do — shipped
    // three brushes completely invisible, because they state a base of about
    // 2.5e-05 and put the whole of their opacity on a pressure mapping.
    let alpha = |env: &Env| {
        (file.eval("opaque", env).max(0.0) * file.eval("opaque_multiply", env)).clamp(0.0, 1.0)
    };
    let alphas: Vec<f32> = sample_points()
        .map(|p| alpha(&env.with(DabInput::Pressure, p)))
        .collect();
    let a_max = alphas.iter().copied().fold(f32::MIN, f32::max);
    let a_min = alphas.iter().copied().fold(f32::MAX, f32::min);
    let peak_pressure = sample_points()
        .zip(&alphas)
        .fold(
            (1.0f32, f32::MIN),
            |acc, (p, a)| {
                if *a > acc.1 { (p, *a) } else { acc }
            },
        )
        .0;

    let opacity_varies = (file.is_driven("opaque", DabInput::Pressure)
        || file.is_driven("opaque_multiply", DabInput::Pressure))
        && a_max > 0.0
        && a_min < a_max * 0.99;
    let opacity_curve = if opacity_varies {
        let mut points = [0.0f32; ResponseCurve::N];
        for (point, a) in points.iter_mut().zip(&alphas) {
            *point = (a / a_max).clamp(0.0, 1.0);
        }
        ResponseCurve { points }
    } else {
        ResponseCurve::LINEAR
    };

    // A non-pressure input on either half of the product is carried as a
    // *factor* rather than an offset, because that is the shape opacity has
    // here: `Brush::opacity` is the peak and per-dab coverage scales it. The
    // peak has to grow to make room for the boost, or a brush that only reaches
    // full opacity at speed would be normalised down to never reaching it.
    let mut opacity = a_max;
    if a_max > 0.0 {
        let at_peak = env.with(DabInput::Pressure, peak_pressure);
        for input in EXTRA_INPUTS {
            if !file.is_driven("opaque", input) && !file.is_driven("opaque_multiply", input) {
                continue;
            }
            let values: Vec<f32> = samples(input)
                .map(|x| alpha(&at_peak.with(input, x)))
                .collect();
            let g_max = values.iter().copied().fold(f32::MIN, f32::max);
            if g_max <= 0.0 {
                continue;
            }
            let factors: Vec<f32> = values.iter().map(|v| v / g_max).collect();
            if let Some(m) = build(DabTarget::Opacity, input, &factors) {
                mods.push(m);
                opacity *= g_max / a_max;
            }
        }
    }
    let opacity = opacity.clamp(0.0, 1.0);

    // --- spacing ------------------------------------------------------------
    //
    // MyPaint states dab density as dabs per radius travelled, summed over a
    // term scaled by the *current* radius and one scaled by the *base* radius.
    // Umber has a single spacing expressed as a fraction of the diameter, so
    // the two terms are added as though the radii were equal — true at full
    // pressure, and increasingly wrong at light pressure for the few brushes
    // that use `dabs_per_basic_radius`.
    let per_radius =
        file.eval("dabs_per_actual_radius", &env) + file.eval("dabs_per_basic_radius", &env);
    let spacing = if per_radius > 0.0 {
        (1.0 / (2.0 * per_radius)).clamp(0.01, 0.5)
    } else {
        Brush::default().spacing
    };

    // `dabs_per_second` carries straight across — Umber's dab loop now has a
    // time term of its own. A brush with *no* distance term is an airbrush and
    // depends on this entirely; one with both gets both, as MyPaint does.
    let dabs_per_second = file.eval("dabs_per_second", &env).clamp(0.0, 300.0);

    // --- smudge -------------------------------------------------------------
    //
    // MyPaint samples the canvas under the dab and mixes it into the colour it
    // deposits; Umber does the same, a frame or two behind because its read is
    // asynchronous. `smudge_radius_log` is a natural log like `radius_
    // logarithmic`, and is a multiplier on the dab radius rather than a radius
    // in pixels — reading it as pixels would make every blender canvas-wide.
    let smudge = file.eval("smudge", &env).clamp(0.0, 1.0);
    let smudge_length = file.eval("smudge_length", &env).clamp(0.0, 0.99);
    let smudge_radius = file.eval("smudge_radius_log", &env).exp().clamp(0.25, 8.0);
    // 42 brushes put colour pickup on pressure — an oil brush that mixes when
    // you lean on it and lays fresh paint when you do not. With only the base
    // value read, every one of them was one or the other for the whole stroke.
    for input in [DabInput::Pressure]
        .into_iter()
        .chain(EXTRA_INPUTS)
        .chain([DabInput::Direction])
    {
        mods.extend(modulation(&file, "smudge", DabTarget::Smudge, input, &env));
    }

    // --- dab shape ----------------------------------------------------------
    //
    // MyPaint's dab is an ellipse whose *long* axis is the radius and whose
    // short axis is `radius / elliptical_dab_ratio`, tilted by
    // `elliptical_dab_angle` degrees. Umber's is the same, so these carry
    // across directly. The ratio is documented as >= 1.0 and a few brushes
    // state slightly less, which would turn the dab inside out.
    let dab_ratio = file.eval("elliptical_dab_ratio", &env).clamp(1.0, 20.0);
    // 46 brushes vary the ratio and 15 of them state a round base, so before
    // this they arrived as perfect circles. `random` (16 brushes) is a bristle
    // clump that changes shape stamp to stamp; `speed1` (14) is a brush that
    // flattens as it is dragged.
    for input in [DabInput::Pressure]
        .into_iter()
        .chain(EXTRA_INPUTS)
        .chain([DabInput::Direction])
    {
        mods.extend(modulation(
            &file,
            "elliptical_dab_ratio",
            DabTarget::Ratio,
            input,
            &env,
        ));
    }

    let dab_angle = file.eval("elliptical_dab_angle", &env).rem_euclid(360.0);

    // A brush whose angle is driven by the `direction` input turns to follow
    // the stroke — a rake or a fan. One with a fixed angle is a broad nib, and
    // holding its angle through a curve is what makes calligraphy thick and
    // thin. Reading a rake as a nib, or the reverse, is immediately visible.
    let dab_angle_follows_stroke = file.is_driven("elliptical_dab_angle", DabInput::Direction);

    // A `random` mapping on the angle is the third case, and the pack's most
    // common shape mapping after direction: 31 brushes ask for it and 29 of
    // those have an elongated dab, so ignoring it turned a watercolour fringe,
    // a charcoal and a grain brush into combs — every stamp lying the same way
    // down the stroke. MyPaint's `random` input runs 0..1, so the span of the
    // mapping *is* the full width of the rotation, in degrees.
    let dab_angle_jitter = file
        .span("elliptical_dab_angle", "random")
        .clamp(0.0, 360.0);

    // `direction` and `random` are excluded here: both already have a dedicated
    // reading above, and taking them twice would turn a rake into a rake that
    // also swings.
    for input in [
        DabInput::Pressure,
        DabInput::Speed,
        DabInput::SlowSpeed,
        DabInput::Stroke,
    ] {
        mods.extend(modulation(
            &file,
            "elliptical_dab_angle",
            DabTarget::Angle,
            input,
            &env,
        ));
    }

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
    let scatter_at = |p: f32| {
        file.eval("offset_by_random", &env.with(DabInput::Pressure, p))
            .clamp(0.0, 8.0)
    };
    let scatters: Vec<f32> = sample_points().map(scatter_at).collect();
    let s_max = scatters.iter().copied().fold(f32::MIN, f32::max);
    let s_min = scatters.iter().copied().fold(f32::MAX, f32::min);
    let scatter = s_max;
    // Absolute, because the interesting case is a brush that scatters by 0.4
    // radii at one end and not at all at the other — no relative threshold can
    // see that without also firing on rounding noise near zero.
    let pressure_scatter = file.is_driven("offset_by_random", DabInput::Pressure)
        && s_max > 0.0
        && s_max - s_min > 0.01;
    let (min_scatter_ratio, scatter_curve) = if pressure_scatter {
        (s_min / s_max, normalised_curve(&scatters, s_min, s_max))
    } else {
        (Brush::default().min_scatter_ratio, ResponseCurve::LINEAR)
    };
    for input in EXTRA_INPUTS {
        mods.extend(modulation(
            &file,
            "offset_by_random",
            DabTarget::Scatter,
            input,
            &env,
        ));
    }

    let radius_jitter = file.eval("radius_by_random", &env).clamp(0.0, 3.0);

    // `offset_by_speed` throws the dab along the smoothed velocity, a tenth of
    // a second's worth per unit — a *directed* lead, not a spray. Reading it as
    // scatter is the obvious approximation and gets the character exactly
    // backwards: a trailing brush would become confetti.
    let speed_offset = file.eval("offset_by_speed", &env).clamp(-10.0, 10.0);

    // --- colour -------------------------------------------------------------
    //
    // Only the *variation* is taken, never the constant part: `modulation`
    // subtracts the value at the input's neutral, so a brush that permanently
    // shifts the hue by a fixed amount arrives painting the colour the user
    // picked. That is a deliberate departure. MyPaint's brushes carry their own
    // colour and a constant shift is part of it; in Umber the palette is the
    // user's, and a brush that silently repaints their choice would read as a
    // bug rather than as a feature.
    for input in [DabInput::Pressure]
        .into_iter()
        .chain(EXTRA_INPUTS)
        .chain([DabInput::Direction])
    {
        mods.extend(modulation(
            &file,
            "change_color_h",
            DabTarget::Hue,
            input,
            &env,
        ));
        // HSL's lightness read as HSV's value, and HSL's saturation as HSV's.
        // They are not the same axis — a fully saturated hue is L 0.5 and V 1 —
        // so the *amount* of a `change_color_l` shift is approximate while its
        // direction and its timing are exact. 14 brushes drift lightness along
        // the stroke and this is what makes that visible at all.
        mods.extend(sum_modulation(
            &file,
            &["change_color_v", "change_color_l"],
            DabTarget::Value,
            input,
            &env,
        ));
        mods.extend(sum_modulation(
            &file,
            &["change_color_hsv_s", "change_color_hsl_s"],
            DabTarget::Saturation,
            input,
            &env,
        ));
    }

    // --- the rest -----------------------------------------------------------
    let mode = if file.eval("eraser", &env) >= 0.5 {
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
    let slow = file.eval("slow_tracking", &env).max(0.0);
    let stabilization = (slow / (slow + 1.0)).clamp(0.0, 0.95);

    // The `stroke` input's ramp: `exp(stroke_duration_logarithmic)` radii of
    // travel to reach 1, then `stroke_holdtime` more before it wraps.
    let stroke_span = file
        .eval("stroke_duration_logarithmic", &env)
        .exp()
        .clamp(0.1, 10_000.0);
    let stroke_hold = file.eval("stroke_holdtime", &env).clamp(0.0, 10.0);

    // Heaviest first, so that a brush with more live mappings than the table
    // holds keeps the ones that change the mark most. Nothing in the shipped
    // pack reaches the cap — the busiest uses six of eight — but a hand-tuned
    // brush from elsewhere might.
    mods.sort_by(|a, b| b.weight().total_cmp(&a.weight()));
    let modulations: Modulations = mods.into_iter().collect();

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
        // A `.myb` has no per-brush blend mode of this kind — MyPaint's own
        // `Eraser` setting is the paint/erase switch above and nothing else.
        // So there is nothing to read and nothing is invented: an import that
        // *gains* a behaviour nobody asked for is worse than one that loses
        // something, because a loss is at least reported. This is not a
        // `dropped_features` entry either, for the same reason: nothing was
        // dropped.
        blend: BlendMode::Normal,
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
        speed_offset,
        stroke_span,
        stroke_hold,
        modulations,
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
    if file.base("colorize") >= 0.5 {
        reasons.push("colorize");
    }
    if file.base("lock_alpha") >= 0.5 {
        reasons.push("lock_alpha");
    }
    Ok(reasons)
}

/// The five pressures a [`ResponseCurve`] samples at.
fn sample_points() -> impl Iterator<Item = f32> {
    (0..ResponseCurve::N).map(ResponseCurve::x_of)
}

/// The five points across an input's domain that a modulation is sampled at.
fn samples(input: DabInput) -> impl Iterator<Item = f32> {
    let (lo, hi) = input.domain();
    (0..ResponseCurve::N).map(move |i| lo + (hi - lo) * ResponseCurve::x_of(i))
}

/// Turn one `(setting, input)` mapping into a modulation.
///
/// The value taken is the mapping's **contribution** — what it adds on top of
/// what the base value already accounts for — so the setting is evaluated
/// across the input's domain and the value at the input's neutral is
/// subtracted. That is exactly MyPaint's `+ mapping(x)`, and it is what makes
/// the neutral contribution stay in the base field where it belongs instead of
/// being counted twice.
fn modulation(
    file: &MybFile,
    setting: &str,
    target: DabTarget,
    input: DabInput,
    env: &Env,
) -> Option<Modulation> {
    sum_modulation(file, &[setting], target, input, env)
}

/// The same, for a target two MyPaint settings both feed.
fn sum_modulation(
    file: &MybFile,
    settings: &[&str],
    target: DabTarget,
    input: DabInput,
    env: &Env,
) -> Option<Modulation> {
    if !settings.iter().any(|s| file.is_driven(s, input)) {
        return None;
    }
    let total = |env: &Env| settings.iter().map(|s| file.eval(s, env)).sum::<f32>();
    let neutral = total(env);
    let values: Vec<f32> = samples(input)
        .map(|x| total(&env.with(input, x)) - neutral)
        .collect();
    build(target, input, &values)
}

/// Wrap five sampled outputs as a modulation, or reject it as too faint to be
/// worth one of the eight slots.
fn build(target: DabTarget, input: DabInput, values: &[f32]) -> Option<Modulation> {
    let low = values.iter().copied().fold(f32::MAX, f32::min);
    let high = values.iter().copied().fold(f32::MIN, f32::max);
    if !low.is_finite() || !high.is_finite() {
        return None;
    }
    let m = Modulation {
        target,
        input,
        low,
        high,
        curve: normalised_curve(values, low, high),
    };
    (m.weight() >= 1.0).then_some(m)
}

/// Rescale five sampled values onto the curve's `0..=1`.
///
/// Umber states every pressure dynamic as `peak × (min_ratio + (1 - min_ratio)
/// × curve(p))`, so the curve carries only the *shape* and the two ratios carry
/// the range. Written once because size, hardness, scatter and every
/// modulation all do it, and four copies of the same normalisation is four
/// chances to get one of them backwards.
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

/// Where every MyPaint input sits while a setting is being read.
///
/// Two are resolved to their true constant value — `brush_radius` is the
/// brush's own base radius and `viewzoom` is zero at 100% — and the rest sit at
/// [`DabInput::neutral`], MyPaint's own idea of a typical value.
#[derive(Clone, Copy)]
struct Env {
    pressure: f32,
    speed1: f32,
    speed2: f32,
    stroke: f32,
    direction: f32,
    random: f32,
    brush_radius: f32,
}

impl Env {
    fn resting(brush_radius: f32) -> Self {
        Self {
            pressure: DabInput::Pressure.neutral(),
            speed1: DabInput::Speed.neutral(),
            speed2: DabInput::SlowSpeed.neutral(),
            stroke: DabInput::Stroke.neutral(),
            direction: DabInput::Direction.neutral(),
            random: DabInput::Random.neutral(),
            brush_radius,
        }
    }

    fn get(&self, name: &str) -> f32 {
        match name {
            "pressure" => self.pressure,
            "speed1" => self.speed1,
            "speed2" => self.speed2,
            "stroke" => self.stroke,
            "direction" => self.direction,
            "random" => self.random,
            "brush_radius" => self.brush_radius,
            // Everything else — tilt, `custom`, `attack_angle`, `viewzoom`,
            // the gridmap pair — reads zero on a desktop with a mouse, which is
            // what MyPaint would read there too. The mapping is still evaluated
            // *at* zero, so its contribution is kept rather than dropped.
            _ => 0.0,
        }
    }

    fn with(mut self, input: DabInput, x: f32) -> Self {
        match input {
            DabInput::Pressure => self.pressure = x,
            DabInput::Speed => self.speed1 = x,
            DabInput::SlowSpeed => self.speed2 = x,
            DabInput::Stroke => self.stroke = x,
            DabInput::Direction => self.direction = x,
            DabInput::Random => self.random = x,
        }
        self
    }
}

#[derive(Deserialize)]
struct MybFile {
    version: u32,
    #[serde(default)]
    settings: HashMap<String, MybSetting>,
}

impl MybFile {
    /// A setting's base value, or MyPaint's default when the file omits it.
    ///
    /// Real `.myb` files write the whole table, so the defaults only matter for
    /// hand-written fragments — but zero is the wrong answer for a dozen of
    /// them, and "the fixture in the test file behaves differently from a real
    /// brush" is a bad way to find that out.
    fn base(&self, name: &str) -> f32 {
        match self.settings.get(name) {
            Some(s) => s.base_value,
            None => default_base(name),
        }
    }

    /// MyPaint's `value = base_value + Σ mapping_i(input_i)`, evaluated at a
    /// given set of inputs. Every setting this importer reads goes through
    /// here, which is why a `brush_radius` or `tilt` mapping contributes
    /// without needing a case of its own.
    fn eval(&self, name: &str, env: &Env) -> f32 {
        let Some(setting) = self.settings.get(name) else {
            return default_base(name);
        };
        let mut value = setting.base_value;
        for (input, points) in &setting.inputs {
            value += piecewise(points, env.get(input));
        }
        value
    }

    /// How far a mapping's output travels from end to end.
    ///
    /// MyPaint's editor writes a two-point mapping for every input a brush has
    /// ever touched, most of them flat — 24 of the 55 brushes that "map"
    /// `elliptical_dab_ratio` map it to a constant zero. A flat mapping
    /// contributes nothing, so measuring the span rather than the presence of
    /// points is what separates a real setting from an editor artefact.
    fn span(&self, name: &str, input: &str) -> f32 {
        let Some(points) = self.settings.get(name).and_then(|s| s.inputs.get(input)) else {
            return 0.0;
        };
        if points.len() < 2 {
            return 0.0;
        }
        let lo = points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
        let hi = points.iter().map(|p| p.1).fold(f32::MIN, f32::max);
        (hi - lo).max(0.0)
    }

    fn is_driven(&self, name: &str, input: DabInput) -> bool {
        self.span(name, input.myb_name()) > 0.0
    }
}

/// MyPaint's default for a setting a file leaves out, from
/// `libmypaint/brushsettings.json`. Only the non-zero ones are listed; every
/// other setting defaults to zero, which is what the fallthrough gives.
fn default_base(name: &str) -> f32 {
    match name {
        "opaque"
        | "anti_aliasing"
        | "gridmap_scale_x"
        | "gridmap_scale_y"
        | "paint_mode"
        | "offset_by_speed_slowness"
        | "elliptical_dab_ratio" => 1.0,
        "opaque_linearize" => 0.9,
        "radius_logarithmic" | "dabs_per_actual_radius" | "direction_filter" => 2.0,
        "hardness" => 0.8,
        "speed1_slowness" => 0.04,
        "speed2_slowness" => 0.8,
        "speed1_gamma" | "speed2_gamma" | "stroke_duration_logarithmic" => 4.0,
        "smudge_length" => 0.5,
        "elliptical_dab_angle" => 90.0,
        "posterize_num" => 0.05,
        _ => 0.0,
    }
}

#[derive(Deserialize)]
struct MybSetting {
    #[serde(default)]
    base_value: f32,
    /// A `BTreeMap` rather than a `HashMap`, and that is load-bearing rather
    /// than tidy. [`MybFile::eval`] *sums* one mapping per input, and
    /// floating-point addition is not associative — so with Rust's randomly
    /// seeded hasher the same brush evaluated in two processes differed in the
    /// last bit. That made `builtin-brushes.ron` irreproducible: regenerating
    /// with nothing at all behind it still rewrote a hundred lines, which is
    /// exactly the noise that stops a generated file's diff being worth
    /// reading. Ordering by the input's name fixes the order of the sum.
    #[serde(default)]
    inputs: BTreeMap<String, Vec<(f32, f32)>>,
}

/// Evaluate MyPaint's piecewise-linear mapping, line for line with
/// `mypaint_mapping_calculate`.
///
/// The part that is easy to get wrong is the ends. libmypaint picks the segment
/// whose right-hand point is the first one past `x`, falling back to the first
/// or last segment, and then **extrapolates** along it. It does not hold the
/// end value. The difference is not academic: `classic/pen` states its speed
/// mapping over `0..1` while the speed input runs to 4, so holding makes a fast
/// flick thin the nib by 14% where MyPaint thins it by 45%.
///
/// Extrapolation is bounded by the input's domain, which
/// [`DabInput::normalise`] clamps, so no mapping can run away.
///
/// A segment with equal x or equal y contributes its left value flat — that is
/// libmypaint's own guard, and it is also what stops a step mapping (two points
/// sharing an x, which is how a brush spells an on/off threshold) dividing by
/// zero. An empty or single-point mapping contributes nothing.
fn piecewise(points: &[(f32, f32)], x: f32) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }
    let (mut x0, mut y0) = points[0];
    let (mut x1, mut y1) = points[1];
    for &(px, py) in &points[2..] {
        if x <= x1 {
            break;
        }
        (x0, y0) = (x1, y1);
        (x1, y1) = (px, py);
    }
    if (x1 - x0).abs() < f32::EPSILON || (y1 - y0).abs() < f32::EPSILON {
        return y0;
    }
    (y1 * (x - x0) + y0 * (x1 - x)) / (x1 - x0)
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

    /// Rebuild a brush's radius the way [`crate::stroke::StrokeBuilder`] does:
    /// the pressure curve, times the exponential of every `Size` modulation.
    /// Written out here rather than borrowed from the stroke builder so the two
    /// have to agree by construction rather than by hope.
    fn radius_of(b: &Brush, pressure: f32, speed1: f32) -> f32 {
        let mut r = b.radius_at(pressure);
        for m in b.modulations.as_slice() {
            if m.target != DabTarget::Size {
                continue;
            }
            let x = match m.input {
                DabInput::Speed | DabInput::SlowSpeed => speed1,
                _ => m.input.neutral(),
            };
            r *= m.at_raw(x).exp();
        }
        r
    }

    #[test]
    fn a_pen_imports_with_a_believable_shape() {
        let b = from_myb(PEN).expect("import");

        // radius_logarithmic is a natural log, and the pen states two mappings
        // on it: +0.5 over pressure and -0.15 over speed. `size` is the radius
        // at full pressure and typical speed, doubled — exp(0.96 - 0.075 + 0.5)
        // × 2 = 7.99 px, not 0.96, not 1.92, and not the 8.61 that reading the
        // pressure term alone would give.
        assert!((b.size - 7.99).abs() < 0.05, "size was {}", b.size);
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

    /// The shipped library is generated once and committed, so the generator
    /// has to produce the same bytes twice. It did not: `eval` sums one mapping
    /// per input, floating-point addition is not associative, and iterating a
    /// `HashMap` visits the inputs in an order that Rust reseeds every process
    /// — so a regeneration with no change behind it rewrote a hundred lines in
    /// the last decimal place. A test cannot see the seed change from inside
    /// one process, so what is pinned here is the property that fixes it: the
    /// inputs are visited in a fixed order.
    #[test]
    fn a_settings_inputs_are_summed_in_a_fixed_order() {
        let file: MybFile = serde_json::from_str(
            r#"{"version": 3, "settings": { "radius_logarithmic": {
                "base_value": 1.0, "inputs": {
                    "speed1": [[0.0, 0.1], [4.0, 0.2]],
                    "pressure": [[0.0, 0.0], [1.0, 0.5]],
                    "random": [[0.0, -0.1], [1.0, 0.1]],
                    "stroke": [[0.0, 0.0], [1.0, 0.3]] } } } }"#,
        )
        .expect("parse");
        let order: Vec<&str> = file.settings["radius_logarithmic"]
            .inputs
            .keys()
            .map(String::as_str)
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "the inputs must iterate in a settled order");
    }

    #[test]
    fn the_imported_radius_matches_mypaint_at_every_sample() {
        // The whole point of the size/min_ratio/curve triple *and* of the
        // modulation table: rebuilding the radius the way the stroke builder
        // does must land back on MyPaint's own
        //
        //     exp(base + map_pressure(p) + map_speed1(s))
        //
        // at every one of the points the curves are sampled at, for both
        // inputs at once. This is the test that would have caught the speed
        // term being dropped, and it did not exist before.
        let b = from_myb(PEN).expect("import");
        for i in 0..=4 {
            let p = i as f32 / 4.0;
            for j in 0..=4 {
                let s = j as f32;
                let expected = (0.96 + 0.5 * p - 0.15 * s).exp();
                let got = radius_of(&b, p, s);
                assert!(
                    (got - expected).abs() < 0.02,
                    "at pressure {p}, speed {s}: expected {expected}, got {got}"
                );
            }
        }
    }

    /// `brush_radius` is libmypaint's `BASEVAL(RADIUS_LOGARITHMIC)` — the
    /// brush's own base radius fed back as an input, and therefore a constant
    /// for the whole stroke. Thirteen brushes in the pack map it, some over
    /// seven log units, and dropping the contribution imported every one of
    /// them at the wrong size. It needs no runtime support at all; it just has
    /// to be evaluated.
    #[test]
    fn a_brush_radius_mapping_is_folded_into_the_size() {
        let json = r#"{ "version": 3, "settings": {
            "radius_logarithmic": { "base_value": 2.0, "inputs": {
                "brush_radius": [[-2.0, 0.0], [6.0, 8.0]] } } } }"#;
        let b = from_myb(json).expect("imports");
        // The input reads 2.0, a quarter of the way from -2 to 6, so the
        // mapping contributes +4 and the radius is exp(6), not exp(2).
        assert!((b.size - 2.0 * 6f32.exp()).abs() < 1.0, "size {}", b.size);
        // And it must not have cost a modulation slot: it cannot vary.
        assert!(b.modulations.is_empty());
    }

    /// The fault that shipped three brushes completely invisible. MyPaint
    /// evaluates `opaque` as a full expression like every other setting, and
    /// several brushes state a base of about 2.5e-05 with the whole of their
    /// opacity on a pressure mapping.
    #[test]
    fn opaque_is_evaluated_rather_than_read_off_its_base() {
        let json = r#"{ "version": 3, "settings": {
            "opaque": { "base_value": 0.0000254, "inputs": {
                "pressure": [[0.0, 0.0], [1.0, 1.0]] } },
            "opaque_multiply": { "base_value": 0.0, "inputs": {
                "pressure": [[0.0, 0.0], [1.0, 1.0]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        assert!(b.opacity > 0.99, "opacity was {}", b.opacity);
        assert!(b.pressure_opacity);
        // alpha(p) = p * p, so a light touch is nearly nothing and full
        // pressure is solid.
        assert!(b.coverage_at(0.5) < 0.3, "{}", b.coverage_at(0.5));
        assert!((b.coverage_at(1.0) - 1.0).abs() < 1e-4);
    }

    /// libmypaint's exact arithmetic, from `prepare_and_draw_dab`:
    ///
    /// ```c
    /// opaque = MAX(0.0, SETTING(OPAQUE));
    /// opaque = CLAMP(opaque * opaque_fac, 0.0, 1.0);
    /// ```
    ///
    /// Neither setting is first clamped to the range its editor shows. A brush
    /// whose `opaque` reaches 1.5 really does reach full coverage at two thirds
    /// of its multiplier, and clamping on the way past would under-paint it —
    /// `classic/long_grass` is exactly this shape and shipped at 0.748.
    #[test]
    fn only_the_product_is_clamped_not_each_half() {
        let json = r#"{ "version": 3, "settings": {
            "opaque": { "base_value": 1.1, "inputs": {
                "pressure": [[0.0, 0.4], [1.0, 0.4]] } },
            "opaque_multiply": { "base_value": 0.68, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        // 1.5 × 0.68 = 1.02, clamped to 1.0. Clamping `opaque` to 1.0 first
        // would give 0.68 instead.
        assert!((from_myb(json).unwrap().opacity - 1.0).abs() < 1e-4);
    }

    /// A speed mapping is written on MyPaint's own log-speed scale, so it is
    /// only meaningful alongside `speed1_gamma`. This pins the whole chain:
    /// the domain the mapping is sampled over, the curve, and the arithmetic
    /// the stroke builder uses to put it back together.
    #[test]
    fn a_speed_mapping_reproduces_mypaints_value_at_every_sample() {
        let json = r#"{ "version": 3, "settings": {
            "hardness": { "base_value": 0.5, "inputs": {
                "speed1": [[0.0, 0.0], [4.0, 0.4]] } },
            "opaque": { "base_value": 1.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        let m = b
            .modulations
            .as_slice()
            .iter()
            .find(|m| m.target == DabTarget::Hardness)
            .expect("a hardness modulation");
        assert_eq!(m.input, DabInput::Speed);
        for i in 0..=4 {
            let s = i as f32;
            // MyPaint: hardness = 0.5 + 0.1 * s. Umber holds the value at the
            // input's neutral (speed 0.5 → +0.05) in the field, so the
            // modulation carries the difference.
            let want = 0.5 + 0.1 * s;
            let got = b.hardness_at(0.4) + m.at_raw(s);
            assert!((got - want).abs() < 1e-3, "at speed {s}: {want} vs {got}");
        }
    }

    /// `radius_logarithmic <- random` is the splotchy-spray brush. It arrives
    /// as a modulation rather than as `radius_jitter` because MyPaint draws it
    /// from the *same* uniform every other `random` mapping on the brush reads,
    /// where `radius_jitter` is an independent gaussian.
    #[test]
    fn a_random_radius_mapping_becomes_a_size_modulation() {
        let json = r#"{ "version": 3, "settings": {
            "radius_logarithmic": { "base_value": 2.0, "inputs": {
                "random": [[0.0, -0.98], [1.0, 0.98]] } } } }"#;
        let b = from_myb(json).expect("imports");
        let m = b.modulations.as_slice()[0];
        assert_eq!((m.target, m.input), (DabTarget::Size, DabInput::Random));
        // exp(1.96) is a 7× swing in radius from one stamp to the next.
        assert!((m.low + 0.98).abs() < 1e-3 && (m.high - 0.98).abs() < 1e-3);
        assert_eq!(b.radius_jitter, 0.0, "not confused with radius_by_random");
    }

    /// 46 brushes vary their ellipticity and 15 state a round base, so before
    /// this they arrived as perfect circles whatever else they did.
    #[test]
    fn a_ratio_driven_by_an_input_no_longer_arrives_round() {
        let json = r#"{ "version": 3, "settings": {
            "elliptical_dab_ratio": { "base_value": 1.0, "inputs": {
                "random": [[0.0, 0.0], [1.0, 4.0]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        assert!(b.is_shaped(), "a ratio that varies is a shaped dab");
        assert!(b.dab_has_angle(), "and it has an angle worth showing");
        let m = b.modulations.as_slice()[0];
        assert_eq!((m.target, m.input), (DabTarget::Ratio, DabInput::Random));
        // Neutral is 0.5, so the base already carries +2 and the modulation
        // runs -2..+2 around it. Their sum is MyPaint's 0..4.
        assert!((b.dab_ratio - 3.0).abs() < 1e-3, "{}", b.dab_ratio);
        assert!((b.dab_ratio + m.low - 1.0).abs() < 1e-3);
        assert!((b.dab_ratio + m.high - 5.0).abs() < 1e-3);
    }

    /// 42 brushes state colour pickup entirely as a pressure mapping — an oil
    /// brush that mixes when you lean on it. Reading the base alone made every
    /// one of them deposit flat paint for the whole stroke.
    #[test]
    fn smudge_stated_only_as_a_mapping_still_makes_the_brush_blend() {
        let json = r#"{ "version": 3, "settings": {
            "smudge": { "base_value": 0.0, "inputs": {
                "pressure": [[0.0, 0.0], [1.0, 0.9]] } },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        assert!(b.smudges(), "the field is nearly zero but the brush blends");
        assert!(b.colours_dabs(), "so the stroke needs its colour scratch");
        let m = b.modulations.as_slice()[0];
        assert_eq!(m.target, DabTarget::Smudge);
        assert!((b.smudge + m.high - 0.9).abs() < 1e-3);
    }

    /// `offset_by_speed` is a *directed* lead along the smoothed velocity, not
    /// a spray. Importing it as scatter would turn a trailing brush into
    /// confetti, which is why it has a field of its own.
    #[test]
    fn offset_by_speed_becomes_a_lead_not_scatter() {
        let json = r#"{ "version": 3, "settings": {
            "offset_by_speed": { "base_value": 0.8, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        assert!((b.speed_offset - 0.8).abs() < 1e-5);
        assert!(b.leads_with_speed());
        assert_eq!(b.scatter, 0.0, "a lead is not a spray");
    }

    /// The colour modulations take the *variation*, never the constant part.
    /// MyPaint's brushes carry their own colour and a fixed hue shift is part
    /// of it; here the palette belongs to the user, and a brush that silently
    /// repainted their choice would read as a bug.
    #[test]
    fn colour_dynamics_take_the_variation_and_not_the_constant() {
        let json = r#"{ "version": 3, "settings": {
            "change_color_h": { "base_value": 0.25, "inputs": {
                "random": [[0.0, -0.05], [1.0, 0.05]] } },
            "change_color_v": { "base_value": 0.4, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        assert_eq!(b.modulations.len(), 1, "the constant shift is not a slot");
        let m = b.modulations.as_slice()[0];
        assert_eq!((m.target, m.input), (DabTarget::Hue, DabInput::Random));
        assert!((m.low + 0.05).abs() < 1e-4 && (m.high - 0.05).abs() < 1e-4);
        assert!(b.colours_dabs(), "a hue wobble needs the colour scratch");
    }

    /// The `stroke` input's ramp is stated as a log of how many dab radii it
    /// takes. MyPaint's default of 4 is about 55 radii, and a brush that
    /// shortens it is asking for a cycle you can see inside one mark.
    #[test]
    fn the_stroke_ramp_comes_from_its_own_setting() {
        let json = r#"{ "version": 3, "settings": {
            "stroke_duration_logarithmic": { "base_value": 1.0, "inputs": {} },
            "stroke_holdtime": { "base_value": 2.0, "inputs": {} },
            "radius_logarithmic": { "base_value": 2.0, "inputs": {} } } }"#;
        let b = from_myb(json).expect("imports");
        assert!((b.stroke_span - std::f32::consts::E).abs() < 1e-3);
        assert!((b.stroke_hold - 2.0).abs() < 1e-5);
        // A file that says nothing gets MyPaint's default of exp(4).
        let plain = from_myb(PEN).expect("import");
        assert!((plain.stroke_span - 54.598).abs() < 0.01);
    }

    /// A brush that reads nothing but pressure must arrive with an empty table,
    /// because an empty table is the fast path: no random draw, no filters, no
    /// per-dab evaluation. Most of the pack is on it and has to stay there.
    #[test]
    fn a_plain_brush_carries_no_modulations_at_all() {
        let b = from_myb(ERASER).expect("import");
        assert!(b.modulations.is_empty());
        assert!(!b.is_modulated());
        assert!(!b.leads_with_speed());
        assert!(!b.colours_dabs());
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
    fn the_mapping_extrapolates_its_end_segments_like_libmypaint() {
        // Not "holds the end value" — libmypaint carries the end segment's
        // slope onwards, and a brush that states a speed mapping over 0..1
        // while the input runs to 4 is relying on exactly that.
        let points = [(0.2, 1.0), (0.8, 2.0)];
        assert!((piecewise(&points, 0.5) - 1.5).abs() < 1e-5);
        assert!((piecewise(&points, 0.0) - 0.6667).abs() < 1e-3);
        assert!((piecewise(&points, 1.0) - 2.3333).abs() < 1e-3);
        // A flat segment contributes its value and nothing more, which is what
        // keeps a step mapping — two points sharing an x — finite.
        assert_eq!(piecewise(&[(0.0, 3.0), (1.0, 3.0)], 9.0), 3.0);
        assert_eq!(piecewise(&[(0.5, 0.0), (0.5, 1.0)], 0.9), 0.0);
        assert_eq!(piecewise(&[], 0.5), 0.0);
        assert_eq!(piecewise(&[(0.0, 5.0)], 0.5), 0.0);
    }

    /// The three-segment case, where the loop has to pick the right segment
    /// rather than the first or the last.
    #[test]
    fn the_mapping_picks_the_segment_x_falls_in() {
        let points = [(0.0, 0.0), (0.5, 1.0), (1.0, 0.0)];
        assert!((piecewise(&points, 0.25) - 0.5).abs() < 1e-5);
        assert!((piecewise(&points, 0.75) - 0.5).abs() < 1e-5);
        assert!((piecewise(&points, 0.5) - 1.0).abs() < 1e-5);
    }
}
