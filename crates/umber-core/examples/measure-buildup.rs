//! What build-up does to a pressure-opacity curve, in numbers.
//!
//! ```sh
//! cargo run -p umber-core --example measure-buildup                 # the sweep
//! cargo run -p umber-core --example measure-buildup -- brush.sut    # one file
//! ```
//!
//! With no argument it prints the two figures `tip::stack_depth` and
//! `tip::per_dab_for_stroke` are documented by — which shipped presets build up
//! and therefore take the conversion, and how accurately the conversion round
//! trips against `tip::dab_stack_alpha` over every spacing and hardness a brush
//! can carry. **Re-run it before quoting either**, for the reason
//! `measure-history.rs` says so about its own.
//!
//! With a `.sut` or `.sutg` it prints what that file's sub-tools convert to, and
//! the three readings of the mark their opacity curve produces: under the `max`
//! blend, under build-up as it was before the conversion existed, and under
//! build-up now. That is the shape a report about an imported brush's opacity
//! arrives in, and the three columns are what tell an import bug from an engine
//! one.

use std::path::PathBuf;

use glam::Vec2;
use umber_core::brush::Brush;
use umber_core::brushimport::clipstudio;
use umber_core::curve::ResponseCurve;
use umber_core::input::InputPoint;
use umber_core::stroke::StrokeBuilder;

fn main() {
    let Some(arg) = std::env::args().nth(1) else {
        survey_builtin();
        sweep_conversion();
        return;
    };
    let path: PathBuf = arg.into();
    let bytes = std::fs::read(&path).expect("read");
    let file = clipstudio::from_sut(&bytes).expect("parse");

    for tool in &file.tools {
        let b = &tool.brush;
        println!("=== {} ===", tool.name);
        println!("  size            {:.3}", b.size);
        println!("  opacity         {:.6}", b.opacity);
        println!("  spacing         {:.4}", b.spacing);
        println!("  hardness        {:.3}", b.hardness);
        println!("  build_up        {}", b.build_up);
        println!(
            "  grain           {:.4}  scale {:.1}",
            b.grain, b.grain_scale
        );
        println!("  pressure_size   {}", b.pressure_size);
        println!("  min_size_ratio  {:.4}", b.min_size_ratio);
        println!("  pressure_opacity {}", b.pressure_opacity);
        println!("  opacity_curve   {:?}", b.opacity_curve.points);
        println!("  opacity_curve preset {:?}", b.opacity_curve.preset_name());
        println!("  pressure_hardness {}", b.pressure_hardness);
        println!("  hardness_curve  {:?}", b.hardness_curve.points);
        println!("  min_hardness_ratio {:.4}", b.min_hardness_ratio);
        println!("  modulations:");
        for m in b.modulations.as_slice() {
            println!(
                "    {:?} <- {:?}  low {:.4} high {:.4} curve {:?}",
                m.target, m.input, m.low, m.high, m.curve.points
            );
        }
        print!("  coverage_at   ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(" p={p:.2}:{:.6}", b.coverage_at(p));
        }
        println!();
        print!("  x opacity     ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(" p={p:.2}:{:.6}", b.coverage_at(p) * b.opacity);
        }
        println!();
        print!("  radius_at     ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(" p={p:.2}:{:.3}", b.radius_at(p));
        }
        println!();
        println!(
            "  curve knots x: {:?}",
            (0..ResponseCurve::N)
                .map(ResponseCurve::x_of)
                .collect::<Vec<_>>()
        );
        print!("  mark, this build              ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(" p={p:.2}:{:.4}", mark_of(b, p, false) * b.opacity);
        }
        println!();
        print!("  mark, before the conversion   ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(" p={p:.2}:{:.4}", mark_of(b, p, true) * b.opacity);
        }
        println!();
        print!("  mark, under `max` (0.0.7/8)   ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(" p={p:.2}:{:.4}", b.coverage_at(p) * b.opacity);
        }
        println!();
    }
    for (i, tool) in file.tools.iter().enumerate() {
        println!(
            "tool {i} {} tip {:?} paper {:?}",
            tool.name,
            tool.tip.as_ref().map(|m| (m.width(), m.height())),
            tool.paper.as_ref().map(|m| (m.width(), m.height()))
        );
        if let Some(paper) = &tool.paper {
            let texels = paper.coverage();
            let mean = texels.iter().map(|t| f64::from(*t)).sum::<f64>() / texels.len() as f64;
            let peak = texels.iter().copied().max().unwrap_or(0);
            println!(
                "   paper mean {:.3} peak {peak} -> grain_coverage {:?}",
                mean / 255.0,
                umber_core::tip::grain_coverage(paper, tool.brush.grain, tool.brush.spacing)
            );
        }
        if let Some(tip) = &tool.tip {
            println!(
                "   tip -> stroke_coverage {:?}",
                umber_core::tip::stroke_coverage(tip, tool.brush.spacing)
            );
        }
    }

    println!("\ndropped: {:?}", clipstudio::dropped_features(&bytes));
}

/// What a straight stroke of this brush at this pressure actually leaves on a
/// point of its own centre line, excluding the stroke's opacity.
///
/// Driven through the **real** `StrokeBuilder`, so it reads the dabs the canvas
/// would get rather than a model of them, and accumulated with `dab.wgsl`'s own
/// arithmetic — the antialiasing margin included, because a pixel of falloff is a
/// third of a 3 px dab and leaving it out is how the first draft of this
/// measurement agreed with a conversion that was wrong.
///
/// `raw` asks for the mark the brush made *before* the conversion existed: the
/// dab carries `coverage_at` itself and build-up compounds it.
///
/// `store` rounds the accumulator to the `R8Unorm` scratch after every dab, which
/// is what the GPU does and what exact arithmetic hides: a per-dab figure below
/// half a level moves the accumulator by nothing at all, so a conversion that
/// asks for one paints an invisible stroke. Pass `false` for the exact reading
/// and `true` for the one the artist sees.
fn mark_of(brush: &Brush, pressure: f32, raw: bool) -> f32 {
    mark_stored(brush, pressure, raw, false)
}

fn mark_stored(brush: &Brush, pressure: f32, raw: bool, store: bool) -> f32 {
    let mut builder = StrokeBuilder::new();
    let flat = Brush {
        stabilization: 0.0,
        ..*brush
    };
    builder.begin(
        flat,
        [1.0; 3],
        InputPoint::new(Vec2::new(0.0, 0.0), pressure, 0.0),
    );
    // Long enough that the midpoint is covered from both sides by every dab that
    // could reach it. A stroke shorter than the dab is wide reads as fainter than
    // its own curve under build-up, and that is true of the head of every
    // building stroke rather than anything the conversion decides.
    let length = (brush.size * 8.0).max(400.0);
    builder.extend(InputPoint::new(Vec2::new(length, 0.0), pressure, 0.2));
    let dabs: Vec<_> = builder.drain_pending().collect();

    // A dab's own centre half way along, and not an arbitrary point: a soft
    // dab's falloff has begun a quarter of a radius out, so a `max` stroke read
    // between two centres is legitimately below its own coverage.
    let mid = dabs[dabs.len() / 2];
    let at = Vec2::new(mid.pos[0], mid.pos[1]);
    let mut alpha = 0.0f32;
    for dab in &dabs {
        let d = (at - Vec2::new(dab.pos[0], dab.pos[1])).length() / dab.radius;
        if d >= 1.0 {
            continue;
        }
        let aa = (1.0 / dab.radius.max(1.0)).clamp(0.001, 0.5);
        let inner = dab.hardness.clamp(0.0, 1.0 - aa);
        let t = if inner >= 1.0 {
            0.0
        } else {
            ((d - inner) / (1.0 - inner)).clamp(0.0, 1.0)
        };
        let carried = if raw {
            brush.coverage_at(pressure)
        } else {
            dab.coverage
        };
        let cov = carried * (1.0 - t * t * (3.0 - 2.0 * t));
        if brush.build_up {
            alpha += cov * (1.0 - alpha);
        } else {
            alpha = alpha.max(cov);
        }
        if store {
            alpha = (alpha * 255.0).round() / 255.0;
        }
    }
    alpha
}

/// How far a building stroke's mark lands from the curve that asked for it, over
/// the brushes a painter could actually build.
///
/// Through the real dab generator, at each of a row of pressures, so the reading
/// includes the antialiasing margin and every clamp on the way. The figures
/// `tip::per_dab_for_stroke` quotes come from here.
fn sweep_conversion() {
    println!("size  spacing  hardness  worst error over pressure 0..1");
    let mut overall = 0.0f32;
    let mut overall_at = (0.0, 0.0, 0.0);
    let mut blanks = 0usize;
    let mut floors = 0usize;
    for size in [2.0, 6.0, 20.0, 200.0, 1000.0] {
        for spacing in [0.01, 0.02, 0.05, 0.1, 0.2, 0.3, 0.5] {
            for hardness in [0.0, 0.5, 0.81, 1.0] {
                let brush = Brush {
                    size,
                    spacing,
                    hardness,
                    stabilization: 0.0,
                    pressure_size: false,
                    pressure_opacity: true,
                    opacity_curve: ResponseCurve::LINEAR,
                    build_up: true,
                    ..Brush::default()
                };
                let mut worst = 0.0f32;
                let mut at = 0.0f32;
                let mut blank = 0usize;
                let mut floored = 0usize;
                for step in 0..=20 {
                    let p = step as f32 / 20.0;
                    let want = brush.coverage_at(p);
                    // Whether the scratch's own floor is what decides this mark,
                    // rather than the conversion: a stack this deep cannot make a
                    // mark fainter than one level built up through it, and the
                    // error there is the target's, not the arithmetic's.
                    let off = brush.off_heading(Vec2::X);
                    let depth = umber_core::tip::stack_depth(
                        brush.step_at(p, off),
                        brush.reach_at(p, off),
                        brush.hardness_at(p),
                        brush.radius_at(p),
                    );
                    let floor = 1.0 - (1.0 - umber_core::tip::SCRATCH_LEVEL).powf(depth);
                    if want > floor {
                        let error = (mark_of(&brush, p, false) - want).abs();
                        if error > worst {
                            worst = error;
                            at = p;
                        }
                    } else if want > 0.0 {
                        floored += 1;
                    }
                    // A mark the curve asked for and the scratch could not hold
                    // is a stroke that paints nothing at all.
                    if want > 0.002 && mark_stored(&brush, p, false, true) <= 0.0 {
                        blank += 1;
                    }
                }
                blanks += blank;
                floors += floored;
                if worst > overall {
                    overall = worst;
                    overall_at = (size, spacing, hardness);
                }
                println!(
                    "{size:>5.0} {spacing:>7.2} {hardness:>9.2}  {worst:.5} ({:.1} levels of 255) at pressure {at:.2}",
                    worst * 255.0
                );
            }
        }
    }
    println!(
        "\nworst overall {overall:.5} ({:.1} levels of 255) at size/spacing/hardness {overall_at:?}",
        overall * 255.0
    );
    println!("marks the curve asked for and the 8-bit scratch could not hold: {blanks}");
    println!("marks below the faintest a stack that deep can make, so pinned at it: {floors}");
}

/// Which shipped presets build up, and which of those have anything below full
/// per-dab coverage for a reason the *artist* set rather than a tip or a paper.
fn survey_builtin() {
    for preset in umber_core::preset::builtin() {
        let b = &preset.brush;
        if !b.build_up {
            continue;
        }
        let modulated = b
            .modulations
            .as_slice()
            .iter()
            .any(|m| matches!(m.target, umber_core::dynamics::DabTarget::Opacity));
        // Only a brush whose per-dab coverage drops below 1.0 for a reason the
        // artist set takes the conversion at all; the rest are untouched, which
        // is what the `1.0` fixed point is for.
        let takes_it = b.pressure_opacity || modulated;
        println!(
            "{:<48} pressure_opacity {:<5} opacity-modulation {:<5} spacing {:.3} hardness {:.3} grain {:.2}  {}",
            preset.id,
            b.pressure_opacity,
            modulated,
            b.spacing,
            b.hardness,
            b.grain,
            if takes_it { "CONVERTED" } else { "unchanged" },
        );
        if !takes_it {
            continue;
        }
        print!("    asked / before / now, on an 8-bit scratch ");
        for p in [0.05, 0.1, 0.25, 0.5, 1.0] {
            print!(
                " p={p:.2}:{:.3}/{:.3}/{:.3}",
                b.coverage_at(p),
                mark_stored(b, p, true, true),
                mark_stored(b, p, false, true),
            );
        }
        println!();
    }
}
