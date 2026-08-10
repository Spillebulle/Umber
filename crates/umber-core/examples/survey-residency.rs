//! What fraction of a real layer actually holds anything.
//!
//! ```sh
//! cargo run --release -p umber-core --example survey-residency -- ~/Desktop
//! cargo run --release -p umber-core --example survey-residency -- --slices --only valorant ~/Desktop
//! ```
//!
//! Written to settle one number. Umber gives every layer a full canvas-sized
//! slice, so a layer is 400 MB at 20000×5000 whatever is painted on it, and one
//! real Clip Studio document — 54 layers at that size — asks for 21.6 GB and is
//! refused. `docs/perf/tiled-layer-storage.md` and
//! `docs/perf/import-and-limits.md` both propose paying per 256-square tile
//! instead, and **both name the same unknown as the thing that decides whether
//! the work is worth doing at all**: how much of a real layer is empty. At 80%
//! covered tiling buys nothing; at 15% it is transformative. Neither document
//! could answer it, so nobody could tell which.
//!
//! **This decodes nothing**, which is the whole reason it can be run over 1.8 GB
//! of somebody's real work in seconds. A `.clip` stores each layer as a grid of
//! 256-square blocks with a present/absent word on every one, so occupancy is a
//! property of the container's framing rather than of the pixels — see
//! `umber_core::docimport::residency`, which is where the reading lives and
//! where its limits are argued. `survey-documents` is the sibling that *does*
//! decode, and it costs 12.3 GB for one of these files.
//!
//! Three readings to hold apart, and the module docs have the full argument:
//!
//! - **stored** is blocks the file holds. An upper bound: Clip Studio writes a
//!   block where the artist touched the canvas, not where paint survived, and
//!   telling those apart needs the inflate this avoids. The error is in the
//!   safe direction — it can only make tiling look *worse* than it is.
//! - **covered** is canvas tiles those blocks reach once placed and clipped,
//!   which is what a tiled Umber would allocate. It is not `stored`: a layer
//!   sits at its own offset, so one block can straddle four canvas tiles, and
//!   one hanging off the page costs nothing.
//! - **occupancy** is `covered` over the canvas tiles a dense slice would take.
//!   That ratio, summed over every slice of every document, is the headline.
//!
//! Only `.clip` is read. It is the format the real documents are in and its
//! block *is* the proposed tile; `.kra` could be measured the same way and
//! `.ora` could not, because it stores one trimmed PNG per layer.

use std::io::Write;
use std::path::{Path, PathBuf};

use umber_core::docimport::residency::{self, DocumentResidency, SliceResidency};

const TILE_BYTES: u64 = 256 * 256 * 4;

fn gigabytes(bytes: u64) -> String {
    format!("{:.2}GB", bytes as f64 / 1e9)
}

fn percent(fraction: f64) -> String {
    format!("{:.1}%", fraction * 100.0)
}

fn main() {
    let mut only: Option<String> = None;
    let mut slices = false;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => only = args.next(),
            "--slices" => slices = true,
            _ => roots.push(PathBuf::from(arg)),
        }
    }
    if roots.is_empty() {
        eprintln!("usage: survey-residency [--only <substring>] [--slices] <file-or-directory>...");
        std::process::exit(2);
    }

    let mut files = collect(&roots);
    if let Some(pattern) = &only {
        files.retain(|p| short(p).to_lowercase().contains(&pattern.to_lowercase()));
    }
    files.sort();

    println!(
        "{:<44} {:>11} {:>4} {:>4} {:>5} {:>7} {:>8} {:>8} {:>7} {:>9} {:>9}",
        "file",
        "canvas",
        "ent",
        "fld",
        "slice",
        "cvtiles",
        "stored",
        "covered",
        "occ",
        "dense",
        "tiled"
    );
    println!("{}", "-".repeat(134));

    // Held per slice rather than per document, because the distribution is the
    // point: one fully covered background among thirty sparse layers moves a
    // mean and changes no conclusion.
    let mut every_slice: Vec<f64> = Vec::new();
    let mut total_covered = 0u64;
    let mut total_canvas = 0u64;
    let mut total_dense = 0u64;
    let mut total_tiled = 0u64;
    let mut skipped: Vec<(String, String, String)> = Vec::new();
    let mut refused = 0usize;
    let mut read = 0usize;

    for path in &files {
        let name = short(path);
        // Printed before the read, so a file that takes the process down with
        // it is named rather than merely absent — `survey-documents`' rule.
        print!("{name:<44} ");
        let _ = std::io::stdout().flush();

        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(e) => {
                refused += 1;
                println!("{:>11}  UNREADABLE: {e}", "-");
                continue;
            }
        };
        let doc = match residency::clip_studio(&bytes) {
            Ok(doc) => doc,
            Err(e) => {
                refused += 1;
                println!("{:>11}  REFUSED: {e}", "-");
                continue;
            }
        };
        read += 1;

        let canvas_tiles = doc.canvas_tiles();
        let stored: usize = doc.slices.iter().map(|s| s.stored).sum();
        let covered: usize = doc.slices.iter().map(|s| s.covered).sum();
        println!(
            "{:>5}x{:<5} {:>4} {:>4} {:>5} {canvas_tiles:>7} {stored:>8} {covered:>8} {:>7} \
             {:>9} {:>9}",
            doc.size.x,
            doc.size.y,
            doc.entries,
            doc.folders,
            doc.slices.len(),
            doc.occupancy().map_or("-".into(), percent),
            gigabytes(doc.dense_bytes()),
            gigabytes(doc.tiled_bytes()),
        );

        for slice in &doc.slices {
            every_slice.push(slice.occupancy(canvas_tiles));
        }
        total_covered += covered as u64;
        total_canvas += (canvas_tiles * doc.slices.len()) as u64;
        total_dense += doc.dense_bytes();
        total_tiled += doc.tiled_bytes();
        for (layer, reason) in &doc.skipped {
            skipped.push((name.clone(), layer.clone(), reason.clone()));
        }
        if slices {
            report_slices(&doc);
        }
    }

    println!("{}", "-".repeat(134));
    println!("{} files; {read} surveyed, {refused} refused", files.len());
    // What could not be measured is printed **first and unconditionally**,
    // because it is the reading that says how much of the corpus the headline
    // below actually covers — and because a run with nothing measurable at all
    // is exactly the run whose only content is this list.
    report_skipped(&skipped);
    if total_canvas == 0 {
        println!();
        println!("nothing measurable");
        return;
    }

    println!();
    println!("THE HEADLINE");
    println!(
        "  {total_covered} tiles held of {total_canvas} a dense store allocates = {}",
        percent(total_covered as f64 / total_canvas as f64)
    );
    println!(
        "  {} dense, {} tiled, a factor of {:.1}",
        gigabytes(total_dense),
        gigabytes(total_tiled),
        total_dense as f64 / total_tiled.max(1) as f64
    );
    println!("  one tile is {} bytes", TILE_BYTES);

    println!();
    println!("DISTRIBUTION over {} slices", every_slice.len());
    every_slice.sort_by(f64::total_cmp);
    for (label, at) in [
        ("min", 0.0),
        ("p10", 0.10),
        ("p25", 0.25),
        ("median", 0.50),
        ("p75", 0.75),
        ("p90", 0.90),
        ("p99", 0.99),
        ("max", 1.0),
    ] {
        println!("  {label:>6}  {}", percent(quantile(&every_slice, at)));
    }
    let mean = every_slice.iter().sum::<f64>() / every_slice.len() as f64;
    println!("  {:>6}  {}", "mean", percent(mean));
    // The shape, said in counts rather than left to be read off the quantiles:
    // "how many layers are essentially full" and "how many are essentially
    // empty" are the two questions the design actually asks.
    let over = |bound: f64| every_slice.iter().filter(|v| **v >= bound).count();
    let under = |bound: f64| every_slice.iter().filter(|v| **v <= bound).count();
    println!("  >=95% covered: {}", over(0.95));
    println!("  >=50% covered: {}", over(0.50));
    println!("  <=25% covered: {}", under(0.25));
    println!("  <=10% covered: {}", under(0.10));
    println!("  <= 5% covered: {}", under(0.05));
}

/// Layers no figure above covers, grouped by cause.
///
/// Grouped rather than listed: a document of twenty vector layers must not
/// produce twenty lines of one sentence. What it must not do either is stay
/// quiet — a layer missing from the sample is a layer missing from the
/// occupancy, and the whole value of the headline is knowing what it is over.
fn report_skipped(skipped: &[(String, String, String)]) {
    if skipped.is_empty() {
        return;
    }
    println!();
    println!("NOT MEASURED ({} slices)", skipped.len());
    let mut reasons: Vec<(&str, usize)> = Vec::new();
    for (_, _, reason) in skipped {
        match reasons.iter_mut().find(|(r, _)| *r == reason.as_str()) {
            Some((_, n)) => *n += 1,
            None => reasons.push((reason, 1)),
        }
    }
    reasons.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    for (reason, n) in reasons {
        println!("  {n:>4}  {}", truncate(reason, 96));
    }
}

/// Every slice of one document, for the `--only` case where somebody has a
/// single file in front of them and wants to know which layer is the dense one.
fn report_slices(doc: &DocumentResidency) {
    let canvas_tiles = doc.canvas_tiles();
    let mut rows: Vec<&SliceResidency> = doc.slices.iter().collect();
    rows.sort_by(|a, b| {
        b.occupancy(canvas_tiles)
            .total_cmp(&a.occupancy(canvas_tiles))
    });
    for slice in rows {
        println!(
            "     {:<38} {:>5}x{:<5} grid {:>3}x{:<3} stored {:>5} covered {:>5} {:>7}  fill {}",
            format!(
                "{}{}",
                truncate(&slice.layer, 32),
                if slice.mask { " (mask)" } else { "" }
            ),
            slice.bitmap.x,
            slice.bitmap.y,
            slice.grid.0,
            slice.grid.1,
            slice.stored,
            slice.covered,
            percent(slice.occupancy(canvas_tiles)),
            slice.fill,
        );
    }
}

/// Nearest-rank on a sorted slice. Deliberately not interpolating: these are
/// counts of tiles and an invented value between two real layers is a figure
/// nobody could go and check.
fn quantile(sorted: &[f64], at: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = ((sorted.len() - 1) as f64 * at).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn collect(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in roots {
        if !root.is_dir() {
            files.push(root.clone());
            continue;
        }
        let Ok(entries) = std::fs::read_dir(root) else {
            eprintln!("could not read {}", root.display());
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("clip"))
            {
                files.push(path);
            }
        }
    }
    files
}

fn short(path: &Path) -> String {
    truncate(path.file_name().and_then(|n| n.to_str()).unwrap_or("?"), 43)
}

fn truncate(text: &str, len: usize) -> String {
    text.chars().take(len).collect()
}
