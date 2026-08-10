//! What thumbnail each document would get, and what it costs to get it.
//!
//! ```sh
//! cargo run --release -p umber-core --example survey-previews -- ~/Desktop
//! ```
//!
//! `docimport::preview` exists because a file manager must not provoke a full
//! import — `survey-documents` measures that at 12.3 GB for one file — and the
//! whole claim is that every format already carries a flattened picture. This
//! is what checks the claim against real documents rather than against the
//! specifications, and what says how long a folder of them would take to draw.
//!
//! The figure that matters is per file: a file manager drawing a folder calls
//! this once per icon, so anything here that took a noticeable fraction of a
//! second would be a folder that stutters as it scrolls.

use std::path::PathBuf;
use std::time::Instant;

use umber_core::docimport::{self, preview};

/// What a large thumbnail asks for. Windows' `IThumbnailProvider` is called
/// with 256 for the "Extra large icons" view; the freedesktop spec's "large"
/// is 256 as well.
const BOX_EDGE: u32 = 256;

fn main() {
    let roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        eprintln!("usage: survey-previews <file-or-directory>...");
        std::process::exit(2);
    }

    let readable = docimport::supported_extensions();
    let mut files: Vec<PathBuf> = Vec::new();
    for root in &roots {
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
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            if readable.contains(&ext.as_str()) {
                files.push(path);
            }
        }
    }
    files.sort();

    println!(
        "{:<44} {:>13} {:>11} {:>9}  verdict",
        "file", "embedded", "thumbnail", "ms"
    );
    println!("{}", "-".repeat(100));

    let mut worst = 0f64;
    let mut failed = 0usize;
    for path in &files {
        let name: String = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .chars()
            .take(43)
            .collect();
        let started = Instant::now();
        match preview::from_path(path) {
            Ok(full) => {
                let was = full.size;
                let small = full.fit_within(BOX_EDGE);
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                worst = worst.max(ms);
                println!(
                    "{name:<44} {:>6}x{:<6} {:>5}x{:<5} {ms:>9.1}  ok",
                    was.x, was.y, small.size.x, small.size.y
                );
            }
            Err(e) => {
                failed += 1;
                let ms = started.elapsed().as_secs_f64() * 1000.0;
                println!(
                    "{name:<44} {:>13} {:>11} {ms:>9.1}  NO PREVIEW: {e}",
                    "-", "-"
                );
            }
        }
    }
    println!("{}", "-".repeat(100));
    println!(
        "{} files, {failed} without a preview, worst {worst:.1} ms",
        files.len()
    );
}
