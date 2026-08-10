//! What fraction of a real layer actually holds anything.
//!
//! ```sh
//! cargo run --release -p umber-core --example survey-residency -- ~/Desktop
//! cargo run --release -p umber-core --example survey-residency -- --contents ~/Desktop
//! cargo run --release -p umber-core --example survey-residency -- --contents --slices --only valorant ~/Desktop
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
//! # Two readings, and the gap between them is the point
//!
//! A `.clip` stores each layer as a grid of 256-square blocks with a
//! present/absent word on every one, so the first reading needs **no decode at
//! all** and runs over 1.8 GB of somebody's real work in seconds.
//!
//! It also **over-reports**. Clip Studio writes a block where the artist
//! touched the canvas, not where paint survived, so a stored block can be
//! entirely transparent and a tiled store would not back that tile. `--contents`
//! inflates each block, asks whether one texel differs from what an absent block
//! would hold, and throws it away — bounded work with one block live at a time,
//! and **never a canvas buffer**, which is the part of "do not decode" that
//! actually mattered. Both figures are reported and so is the gap; if they agree
//! every future survey can be the cheap one, and if they do not then the cheap
//! one is not admissible evidence about storage.
//!
//! `survey-documents` is the sibling that decodes into canvas buffers, and it
//! costs 12.3 GB for one of these files. This never does that under either flag.
//!
//! Three readings to hold apart, and `umber_core::docimport::residency` has the
//! full argument:
//!
//! - **covered** is canvas tiles the stored blocks reach once placed and
//!   clipped. It is not the block count: a layer sits at its own offset, so one
//!   block can straddle four canvas tiles, and one hanging off the page costs
//!   nothing.
//! - **live** is the same over the blocks that hold something, and is what a
//!   tiled Umber would actually allocate.
//! - **occupancy** is either of those over the canvas tiles a dense slice takes.
//!   Summed over every slice of every document, that is the headline.
//!
//! Only `.clip` is read. It is the format the real documents are in and its
//! block *is* the proposed tile; `.kra` could be measured the same way and
//! `.ora` could not, because it stores one trimmed PNG per layer.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use umber_core::docimport::residency::{self, DocumentResidency, Reading, SliceResidency};

fn gigabytes(bytes: u64) -> String {
    format!("{:.2}GB", bytes as f64 / 1e9)
}

fn percent(fraction: f64) -> String {
    format!("{:.1}%", fraction * 100.0)
}

fn maybe(value: Option<f64>) -> String {
    value.map_or_else(|| "-".to_string(), percent)
}

fn main() {
    let mut only: Option<String> = None;
    let mut slices = false;
    let mut reading = Reading::Presence;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => only = args.next(),
            "--slices" => slices = true,
            "--contents" => reading = Reading::Contents,
            _ => roots.push(PathBuf::from(arg)),
        }
    }
    if roots.is_empty() {
        eprintln!(
            "usage: survey-residency [--contents] [--only <substring>] [--slices] \
             <file-or-directory>..."
        );
        std::process::exit(2);
    }

    let mut files = collect(&roots);
    if let Some(pattern) = &only {
        files.retain(|p| short(p).to_lowercase().contains(&pattern.to_lowercase()));
    }
    files.sort();

    println!(
        "reading: {}",
        match reading {
            Reading::Presence => "block presence only, no decode",
            Reading::Contents => "every stored block decoded and tested against its fill",
        }
    );
    println!(
        "{:<44} {:>11} {:>4} {:>4} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7} {:>9} {:>9}",
        "file",
        "canvas",
        "ent",
        "fld",
        "slice",
        "cvtiles",
        "cover",
        "occ",
        "live",
        "liveocc",
        "dense",
        "tiled"
    );
    println!("{}", "-".repeat(140));

    // Held per slice rather than per document, because the distribution is the
    // point: one fully covered background among thirty sparse layers moves a
    // mean and changes no conclusion.
    let mut by_presence: Vec<f64> = Vec::new();
    let mut by_contents: Vec<f64> = Vec::new();
    let mut total_covered = 0u64;
    let mut total_live = 0u64;
    let mut total_dense_tiles = 0u64;
    let mut total_dense = 0u64;
    let mut total_tiled = 0u64;
    let mut total_live_bytes = 0u64;
    let mut decoded_documents = 0usize;
    let mut skipped: Vec<(String, String, String)> = Vec::new();
    let mut refused = 0usize;
    let mut read = 0usize;
    let started = Instant::now();

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
        let doc = match residency::clip_studio(&bytes, reading) {
            Ok(doc) => doc,
            Err(e) => {
                refused += 1;
                println!("{:>11}  REFUSED: {e}", "-");
                continue;
            }
        };
        read += 1;

        println!(
            "{:>5}x{:<5} {:>4} {:>4} {:>5} {:>7} {:>7} {:>7} {:>7} {:>7} {:>9} {:>9}",
            doc.size.x,
            doc.size.y,
            doc.entries,
            doc.folders,
            doc.slices.len(),
            doc.canvas_tiles(),
            doc.covered(),
            maybe(doc.occupancy()),
            doc.live_covered()
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            maybe(doc.live_occupancy()),
            gigabytes(doc.dense_bytes()),
            gigabytes(doc.live_bytes().unwrap_or_else(|| doc.tiled_bytes())),
        );

        let canvas_tiles = doc.canvas_tiles();
        for slice in &doc.slices {
            by_presence.push(slice.occupancy(canvas_tiles));
            if let Some(live) = slice.live_occupancy(canvas_tiles) {
                by_contents.push(live);
            }
        }
        total_covered += doc.covered() as u64;
        total_dense_tiles += doc.dense_tiles() as u64;
        total_dense += doc.dense_bytes();
        total_tiled += doc.tiled_bytes();
        if let (Some(live), Some(bytes)) = (doc.live_covered(), doc.live_bytes()) {
            total_live += live as u64;
            total_live_bytes += bytes;
            decoded_documents += 1;
        }
        for (layer, reason) in &doc.skipped {
            skipped.push((name.clone(), layer.clone(), reason.clone()));
        }
        if slices {
            report_slices(&doc);
        }
    }

    println!("{}", "-".repeat(140));
    println!(
        "{} files; {read} surveyed, {refused} refused, in {:.1}s",
        files.len(),
        started.elapsed().as_secs_f64()
    );
    // What could not be measured is printed **first and unconditionally**,
    // because it is the reading that says how much of the corpus the headline
    // below actually covers — and because a run with nothing measurable at all
    // is exactly the run whose only content is this list.
    report_skipped(&skipped);
    if total_dense_tiles == 0 {
        println!();
        println!("nothing measurable");
        return;
    }

    println!();
    println!("THE HEADLINE");
    println!(
        "  presence: {total_covered} tiles of {total_dense_tiles} a dense store allocates = {}",
        percent(total_covered as f64 / total_dense_tiles as f64)
    );
    if decoded_documents > 0 {
        println!(
            "  contents: {total_live} tiles of {total_dense_tiles} = {}   ({decoded_documents} \
             of {read} documents decoded)",
            percent(total_live as f64 / total_dense_tiles as f64)
        );
        // Stated as a ratio because that is the form the question was asked in:
        // "is the cheap reading wrong by 2x?"
        println!(
            "  the gap:  presence over-reports by {:.2}x",
            total_covered as f64 / total_live.max(1) as f64
        );
    } else {
        println!("  contents: not measured (pass --contents)");
    }
    println!(
        "  {} dense, {} tiled by presence{}",
        gigabytes(total_dense),
        gigabytes(total_tiled),
        if decoded_documents > 0 {
            format!(
                ", {} tiled by contents, a factor of {:.1}",
                gigabytes(total_live_bytes),
                total_dense as f64 / total_live_bytes.max(1) as f64
            )
        } else {
            format!(
                ", a factor of {:.1}",
                total_dense as f64 / total_tiled.max(1) as f64
            )
        }
    );

    report_distribution("DISTRIBUTION by presence", &mut by_presence);
    if !by_contents.is_empty() {
        report_distribution("DISTRIBUTION by contents", &mut by_contents);
    }
}

fn report_distribution(heading: &str, every: &mut [f64]) {
    println!();
    println!("{heading}, over {} slices", every.len());
    every.sort_by(f64::total_cmp);
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
        println!("  {label:>6}  {}", percent(quantile(every, at)));
    }
    let mean = every.iter().sum::<f64>() / every.len().max(1) as f64;
    println!("  {:>6}  {}", "mean", percent(mean));
    // The shape, said in counts rather than left to be read off the quantiles:
    // "how many layers are essentially full" and "how many are essentially
    // empty" are the two questions the design actually asks.
    let over = |bound: f64| every.iter().filter(|v| **v >= bound).count();
    let under = |bound: f64| every.iter().filter(|v| **v <= bound).count();
    println!("  >=95% covered: {}", over(0.95));
    println!("  >=50% covered: {}", over(0.50));
    println!("  <=25% covered: {}", under(0.25));
    println!("  <=10% covered: {}", under(0.10));
    println!("  <= 5% covered: {}", under(0.05));
    println!(
        "     0% covered: {}",
        every.iter().filter(|v| **v == 0.0).count()
    );
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
            "     {:<38} {:>5}x{:<5} grid {:>3}x{:<3} stored {:>5} cover {:>5} {:>7}  live {:>5} \
             {:>5} {:>7}  fill {}",
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
            slice
                .live
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            slice
                .live_covered
                .map_or_else(|| "-".to_string(), |n| n.to_string()),
            maybe(slice.live_occupancy(canvas_tiles)),
            slice.fill,
        );
    }
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
