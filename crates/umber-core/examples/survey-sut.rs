//! Report what a Clip Studio `.sut` or `.sutg` arrives as, sub-tool by
//! sub-tool.
//!
//! ```sh
//! cargo run -p umber-core --example survey-sut -- path/to/Sketch.sut
//! ```
//!
//! Written to answer one report — "passing over an area I have already painted
//! in the same stroke does not layer" — which is a question about
//! [`umber_core::Brush::build_up`] and therefore about what the importer
//! *measured*. Everything that decides that is on one row here: the tip's peak
//! agreement, the paper's mean agreement, the spacing both are measured at, and
//! the opacity the two rules would reach.
//!
//! Kept rather than thrown away for `survey-mypaint`'s reason: a figure nobody
//! can re-derive is a figure that goes stale. The measurement is the whole
//! answer to "setting or bug", and re-running it is how the next report of the
//! same shape gets settled in one command.

use std::path::PathBuf;

use umber_core::brushimport::clipstudio;
use umber_core::tip::{self, TipMask};

fn mean(tile: &TipMask) -> f32 {
    let texels = tile.coverage();
    if texels.is_empty() {
        return 0.0;
    }
    let total: u64 = texels.iter().map(|t| u64::from(*t)).sum();
    total as f32 / (texels.len() as f32 * 255.0)
}

fn peak(tile: &TipMask) -> f32 {
    f32::from(tile.coverage().iter().copied().max().unwrap_or(0)) / 255.0
}

/// Whether a tile is a stencil: only ever fully in or fully out.
///
/// The boundary case `needs_build_up` answers no for, and worth printing
/// because a dark stencil and a faint wash look identical in the mean alone.
fn stencil(tile: &TipMask) -> bool {
    tile.coverage().iter().all(|t| *t == 0 || *t == 255)
}

/// Mean coverage a grain leaves after `passes` sweeps of the same stroke over
/// the same pixels, under whichever rule the brush was imported with.
///
/// This is the number the report "passing over an area I have already painted
/// in the same stroke does not layer" is actually about, and it is
/// [`tip::grain_coverage`]'s own arithmetic with the depth multiplied: a pass
/// puts `1 / spacing` dabs over a point, and the grain is anchored to the
/// document so every one of them is scaled by the same texel. Under `max` the
/// answer is the tile and does not move with `passes` at all, which is the
/// designed behaviour and exactly what a painter reads as "it does not layer".
fn grain_after(tile: &TipMask, strength: f32, spacing: f32, passes: u32, build_up: bool) -> f32 {
    let deep = (f32::from(passes as u16) / spacing.clamp(0.01, 1.0))
        .round()
        .max(1.0) as i32;
    let texels = tile.coverage();
    if texels.is_empty() {
        return 1.0;
    }
    let strength = f64::from(strength.clamp(0.0, 1.0));
    let mut total = 0.0f64;
    for texel in texels {
        let t = f64::from(*texel) / 255.0;
        let bite = 1.0 - strength * (1.0 - t);
        total += if build_up {
            1.0 - (1.0 - bite).powi(deep)
        } else {
            bite
        };
    }
    (total / texels.len() as f64) as f32
}

fn main() {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: survey-sut <file.sut|file.sutg>");
        std::process::exit(2);
    };
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        }
    };
    let file = match clipstudio::from_sut(&bytes) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            std::process::exit(1);
        }
    };

    println!("{} — {} sub-tool(s)", path.display(), file.tools.len());
    if !file.dropped.is_empty() {
        println!("file-wide losses: {:?}", file.dropped);
    }

    for tool in &file.tools {
        let b = &tool.brush;
        println!("\n=== {} ===", tool.name);
        println!(
            "  build_up {:<5} opacity {:.4}  spacing {:.4}  size {:.1}  hardness {:.3}  blend {:?}  mode {:?}",
            b.build_up, b.opacity, b.spacing, b.size, b.hardness, b.blend, b.mode
        );
        println!(
            "  pressure: size {} opacity {} hardness {} scatter {}  min_size_ratio {:.3}",
            b.pressure_size,
            b.pressure_opacity,
            b.pressure_hardness,
            b.pressure_scatter,
            b.min_size_ratio
        );
        let curve: Vec<String> = (0..umber_core::curve::ResponseCurve::N)
            .map(|i| {
                format!(
                    "{:.3}",
                    b.opacity_curve
                        .sample(umber_core::curve::ResponseCurve::x_of(i))
                )
            })
            .collect();
        println!("  opacity_curve [{}]", curve.join(" "));

        match &tool.tip {
            Some(mask) => {
                let measured = tip::stroke_coverage(mask, b.spacing);
                println!(
                    "  tip {}x{}  peak {:.3}  mean {:.3}  stencil {}",
                    mask.width(),
                    mask.height(),
                    peak(mask),
                    mean(mask),
                    stencil(mask)
                );
                println!(
                    "      stroke_coverage: under_max {:.4}  under_build_up {:.4}  agreement {:.4}  needs_build_up {}  usable {}",
                    measured.under_max,
                    measured.under_build_up,
                    measured.agreement(),
                    measured.needs_build_up(),
                    measured.is_usable()
                );
            }
            None => println!("  tip none"),
        }

        println!(
            "  grain strength {:.4}  grain_scale {:.1}",
            b.grain, b.grain_scale
        );
        match &tool.paper {
            Some(tile) => {
                let measured = tip::grain_coverage(tile, b.grain, b.spacing);
                println!(
                    "  paper {}x{}  peak {:.3}  mean {:.3}  stencil {}",
                    tile.width(),
                    tile.height(),
                    peak(tile),
                    mean(tile),
                    stencil(tile)
                );
                println!(
                    "      grain_coverage: under_max {:.4}  under_build_up {:.4}  agreement {:.4}  needs_build_up {}",
                    measured.under_max,
                    measured.under_build_up,
                    measured.agreement(),
                    measured.needs_build_up()
                );
                let passes: Vec<String> = [1, 2, 4, 8]
                    .iter()
                    .map(|n| {
                        format!(
                            "{n}:{:.4}",
                            grain_after(tile, b.grain, b.spacing, *n, b.build_up)
                        )
                    })
                    .collect();
                println!(
                    "      mean coverage after N passes over the same pixels ({}): {}",
                    if b.build_up { "build-up" } else { "max" },
                    passes.join("  ")
                );
            }
            None => println!("  paper none"),
        }

        for m in b.modulations.as_slice() {
            println!(
                "  modulation {:?} <- {:?}  low {:.3} high {:.3}",
                m.target, m.input, m.low, m.high
            );
        }
        if !tool.dropped.is_empty() {
            println!("  dropped: {:?}", tool.dropped);
        }
    }
}
