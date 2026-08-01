//! Measure what a stamp brush actually paints.
//!
//! ```sh
//! cargo run -p umber-core --example measure-stamp -- assets/brushes/gimp/Organic/*.gbr
//! ```
//!
//! A photographic texture stamp looks dense and is not, and the difference
//! decides whether it can be shipped: under Umber's wet-layer `max` a stroke
//! can never be stronger than the mask's brightest texel, while GIMP composites
//! every dab and builds to solid. This prints both figures per file, so the
//! claim in `docs/brush-sources.md` is reproducible rather than remembered.
//!
//! It is deliberately an example rather than a test. The packs are not in the
//! repository — `assets/brushes/` is git-ignored — so a test would either have
//! nothing to run against or would need a `.gbr` checked in.

use std::path::Path;
use umber_core::brushimport::gbr;
use umber_core::tip::stroke_coverage;

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: measure-stamp <file.gbr>...");
        eprintln!();
        eprintln!("Prints, per stamp: its size, its mean and peak coverage, and the");
        eprintln!("peak a straight stroke at its own spacing reaches under each of the");
        eprintln!("dab pass's two coverage rules.");
        std::process::exit(2);
    }

    println!(
        "{:<28} {:>9} {:>7} {:>7} {:>8} {:>9} {:>7}",
        "file", "size", "mean", "peak", "max", "build-up", "verdict"
    );

    let mut needing = 0usize;
    let mut unusable = 0usize;
    for path in &files {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                println!("{:<28} {e}", short(path));
                continue;
            }
        };
        let decoded = match gbr::from_gbr(&bytes) {
            Ok(b) => b,
            Err(e) => {
                println!("{:<28} {e}", short(path));
                continue;
            }
        };

        let spacing = decoded
            .spacing
            .unwrap_or(umber_core::Brush::default().spacing);
        let tip = decoded.tip;
        let coverage = tip.coverage();
        let mean = coverage.iter().map(|&c| c as f32).sum::<f32>() / coverage.len() as f32 / 255.0;
        let peak = coverage.iter().copied().max().unwrap_or(0) as f32 / 255.0;

        let stroke = stroke_coverage(&tip, spacing);
        let verdict = if !stroke.is_usable() {
            unusable += 1;
            "too faint"
        } else if stroke.needs_build_up() {
            needing += 1;
            "build-up"
        } else {
            "max"
        };

        println!(
            "{:<28} {:>4}x{:<4} {:>7.3} {:>7.3} {:>8.3} {:>9.3} {:>7}",
            short(path),
            tip.width(),
            tip.height(),
            mean,
            peak,
            stroke.under_max,
            stroke.under_build_up,
            verdict
        );
    }

    println!();
    println!(
        "{} of {} need build-up to paint at the strength their author drew them; \
         {} are too faint to accumulate at all.",
        needing,
        files.len(),
        unusable
    );
}

fn short(path: &str) -> String {
    let p = Path::new(path);
    let parent = p
        .parent()
        .and_then(|d| d.file_name())
        .map(|d| format!("{}/", d.to_string_lossy()))
        .unwrap_or_default();
    format!(
        "{parent}{}",
        p.file_name().unwrap_or_default().to_string_lossy()
    )
}
