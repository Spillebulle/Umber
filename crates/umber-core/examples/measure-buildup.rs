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

use umber_core::brushimport::clipstudio;
use umber_core::curve::ResponseCurve;

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
        let depth = if b.build_up {
            umber_core::tip::stack_depth(b.spacing, b.hardness)
        } else {
            1.0
        };
        println!("  stack depth   {depth:.3}");
        print!("  stroke, uncompensated (0.0.9)");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            print!(
                " p={p:.2}:{:.4}",
                umber_core::tip::dab_stack_alpha(b.coverage_at(p), b.spacing, b.hardness)
                    * b.opacity
            );
        }
        println!();
        print!("  stroke, compensated (now)    ");
        for p in [0.0, 0.25, 0.5, 0.75, 1.0] {
            let dab = umber_core::tip::per_dab_for_stroke(b.coverage_at(p), depth);
            print!(
                " p={p:.2}:{:.4}",
                umber_core::tip::dab_stack_alpha(dab, b.spacing, b.hardness) * b.opacity
            );
        }
        println!();
        print!("  stroke, max (0.0.7/0.0.8)    ");
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

/// The round-trip error surface of `per_dab_for_stroke`, so the bound its guard
/// asserts is a measurement rather than a guess.
fn sweep_conversion() {
    println!("spacing  worst error over hardness 0..1 and target 0..1");
    for s in [0.01, 0.02, 0.05, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5, 0.7, 1.0] {
        let mut worst = 0.0f32;
        let mut at = (0.0f32, 0.0f32);
        for h in 0..=20 {
            let hardness = h as f32 / 20.0;
            let depth = umber_core::tip::stack_depth(s, hardness);
            for step in 0..=100 {
                let want = step as f32 / 100.0;
                let got = umber_core::tip::dab_stack_alpha(
                    umber_core::tip::per_dab_for_stroke(want, depth),
                    s,
                    hardness,
                );
                if (got - want).abs() > worst {
                    worst = (got - want).abs();
                    at = (hardness, want);
                }
            }
        }
        println!(
            "{s:>7.2}  {worst:.5} ({:.1} levels of 255)  hardness {:.2} target {:.2}",
            worst * 255.0,
            at.0,
            at.1
        );
    }
    println!("hardness  worst error (over spacing 0.02..1.0, target 0..1)  at");
    for hardness in [0.0, 0.25, 0.5, 0.55, 0.81, 0.9, 1.0] {
        let mut worst = 0.0f32;
        let mut at = (0.0f32, 0.0f32);
        for s in 1..=100 {
            let spacing = s as f32 / 100.0;
            let depth = umber_core::tip::stack_depth(spacing, hardness);
            for step in 0..=100 {
                let want = step as f32 / 100.0;
                let got = umber_core::tip::dab_stack_alpha(
                    umber_core::tip::per_dab_for_stroke(want, depth),
                    spacing,
                    hardness,
                );
                if (got - want).abs() > worst {
                    worst = (got - want).abs();
                    at = (spacing, want);
                }
            }
        }
        println!(
            "{hardness:>8.2}  {worst:.5} ({:.1} levels of 255)   spacing {:.2} target {:.2}",
            worst * 255.0,
            at.0,
            at.1
        );
    }
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
        println!(
            "{:<28} build_up  pressure_opacity {:<5} opacity-modulation {:<5} coverage_at(0) {:.4} coverage_at(1) {:.4} spacing {:.3} hardness {:.3} grain {:.2}",
            preset.id,
            b.pressure_opacity,
            modulated,
            b.coverage_at(0.0),
            b.coverage_at(1.0),
            b.spacing,
            b.hardness,
            b.grain,
        );
    }
}
