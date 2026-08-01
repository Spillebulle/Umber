//! How much of a finer pressure axis Umber could actually paint.
//!
//! Windows' pointer API quantises pen pressure to 1024 levels whatever the
//! digitiser resolves, and a pen display advertising 8192 or 16384 is the
//! recurring reason to reach for WinTab and take the raw axis instead. This
//! measures what those extra levels would reach once they are inside the
//! engine, by sweeping the *real* `radius_at` and `coverage_at` over the
//! shipped library at both resolutions and counting the outcomes that differ.
//!
//! Run it before building anything that widens the input:
//!
//! ```sh
//! cargo run -p umber-core --example measure-pressure --release
//! ```
//!
//! The two bounds it exists to show, and neither is on the input side:
//!
//! - **Coverage is capped by the scratch texture.** It is `R8Unorm`, so a dab
//!   has 256 expressible coverages, and 1024 input levels already reach every
//!   one of them. The count is identical at 16384 — those levels carry no
//!   value the pipeline has anywhere to put. `max` and build-up both blend
//!   *in* that target, so overlap does not widen the set either.
//! - **Size is capped by the brush.** `min_size_ratio` and `Brush::size`
//!   between them decide how many diameters the axis spans, and the count of
//!   distinct whole-pixel diameters comes out the same at both resolutions.
//!
//! See the Pressure section of `CLAUDE.md` for the decision this supports.

use std::collections::HashSet;
use umber_core::preset::builtin;

/// The levels a pen reaches Umber through today, and what a 16384-level
/// digitiser would offer if the transport did not stand in the way.
const RESOLUTIONS: [u32; 2] = [1024, 16384];

fn main() {
    let presets = builtin();
    let sized = presets.iter().filter(|p| p.brush.pressure_size).count();
    let opaque = presets.iter().filter(|p| p.brush.pressure_opacity).count();
    println!(
        "{} presets: {sized} size follows pressure, {opaque} opacity",
        presets.len()
    );

    for levels in RESOLUTIONS {
        // Only the brushes that follow pressure in size say anything about the
        // size axis; averaging the fixed-size ones in would report a resolution
        // limit that is really just a brush declining to use it.
        let mut steps: Vec<f32> = Vec::new();
        let mut diameters: Vec<usize> = Vec::new();
        let mut coverages: Vec<usize> = Vec::new();
        let mut light: Vec<usize> = Vec::new();

        for p in presets.iter().filter(|p| p.brush.pressure_size) {
            let b = p.brush;
            let mut worst_step = 0.0f32;
            let mut distinct_diameter = HashSet::new();
            let mut distinct_coverage = HashSet::new();
            let mut previous: Option<f32> = None;

            for k in 0..=levels {
                let t = k as f32 / levels as f32;
                let radius = b.radius_at(t);
                if let Some(last) = previous {
                    worst_step = worst_step.max((radius - last).abs() * 2.0);
                }
                previous = Some(radius);
                distinct_diameter.insert((radius * 2.0).round() as i64);
                // Quantised exactly as the R8Unorm scratch would hold it.
                distinct_coverage.insert((b.coverage_at(t) * 255.0).round() as i64);
            }

            // The lightest twentieth of the axis, measured to a quarter pixel.
            // This is the region a finer digitiser is usually argued for — a
            // sketching hand barely touching the glass — so it is worth its own
            // number rather than being averaged into the whole sweep.
            let mut distinct_light = HashSet::new();
            for k in 0..=(levels / 20) {
                let t = k as f32 / levels as f32;
                distinct_light.insert((b.radius_at(t) * 8.0).round() as i64);
            }

            steps.push(worst_step);
            diameters.push(distinct_diameter.len());
            coverages.push(distinct_coverage.len());
            light.push(distinct_light.len());
        }

        steps.sort_by(f32::total_cmp);
        diameters.sort_unstable();
        coverages.sort_unstable();
        light.sort_unstable();

        println!("--- {levels} levels ---");
        println!(
            "  diameter change per level:      median {:.4} px   p90 {:.4} px   max {:.4} px",
            steps[steps.len() / 2],
            steps[steps.len() * 9 / 10],
            steps[steps.len() - 1],
        );
        println!(
            "  distinct whole-pixel diameters: median {}   max {}",
            diameters[diameters.len() / 2],
            diameters[diameters.len() - 1],
        );
        println!(
            "  distinct 8-bit coverages:       median {}   max {}",
            coverages[coverages.len() / 2],
            coverages[coverages.len() - 1],
        );
        println!(
            "  quarter-px diameters, lightest 5% of the axis: median {}   max {}",
            light[light.len() / 2],
            light[light.len() - 1],
        );
    }
}
