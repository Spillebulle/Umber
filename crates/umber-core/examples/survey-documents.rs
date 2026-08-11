//! Report what a folder of documents arrives as, one row each.
//!
//! ```sh
//! cargo run --release -p umber-core --example survey-documents -- ~/Desktop
//! ```
//!
//! Written to settle one report — a 15000×5000 `.clip` refused with "the canvas
//! is larger than Umber can open", a canvas 1384 px *inside* `MAX_DIMENSION` —
//! which is a question about which of `check_bounds`' two size rules the file
//! actually met. Both used to answer with the same error, so the sentence named
//! a bound the document was nowhere near and the artist had nothing to act on.
//!
//! Every figure that decides it is on one row: the canvas, the entries, how many
//! of those are folders (which hold no pixels and must not be charged for a
//! canvas), and what the painted layers come to. That is what `check_bounds`
//! compares, so a refusal here reads straight back to the rule that produced it.
//!
//! Kept rather than thrown away for `survey-sut`'s reason: a figure nobody can
//! re-derive is a figure that goes stale, and this class of report will arrive
//! again. The next import refusal gets diagnosed in one command.
//!
//! **This decodes**, and the two memory columns are what it is now worth
//! running for. [`docimport::import`] used to read every layer into a
//! canvas-sized buffer — 12.3 GB for one file in the folder this was written
//! against — and the piece contract made it read the rectangles the file
//! actually holds instead. `dense` is the old figure, `held` is the new one,
//! and the ratio between them is the whole of Stage 2 measured on real work.
//! Rows are printed as they finish and each document is freed before the next
//! is read, so a run that dies names the file that killed it.
//! `--only <substring>` is how one suspect file gets read on its own.
//!
//! One reading to be careful with: the entry count is what *loaded*, not what
//! the file declared. A layer the reader refused leaves a warning and does not
//! appear, so a row with warnings may be narrower than the document really is.

use std::io::Write;
use std::path::{Path, PathBuf};

use umber_core::docimport::{self, ImportedDocument};

fn gigabytes(bytes: u64) -> String {
    format!("{:.1}GB", bytes as f64 / 1e9)
}

/// What a document *would* have cost when every reader densified: one canvas
/// per painted layer.
///
/// Folders are excluded deliberately: one holds no slot and no buffer, so
/// charging it a canvas is the bug this example was written to expose. Kept as
/// a column rather than deleted, because it is the figure `check_bounds` used
/// to compare and the one every refusal anybody remembers was stated in.
fn dense_bytes(doc: &ImportedDocument) -> u64 {
    let painted = doc.layers.iter().filter(|l| !l.folder).count();
    u64::from(doc.size.x) * u64::from(doc.size.y) * 4 * painted.max(1) as u64
}

/// What the document actually holds: the sum of every layer's pieces.
///
/// This is what `MAX_TOTAL_BYTES` is now compared against, and what the host
/// really allocates.
fn held_bytes(doc: &ImportedDocument) -> u64 {
    doc.layers.iter().map(|l| l.pixel_bytes()).sum()
}

fn main() {
    let mut only: Option<String> = None;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => only = args.next(),
            _ => roots.push(PathBuf::from(arg)),
        }
    }
    if roots.is_empty() {
        eprintln!("usage: survey-documents [--only <substring>] <file-or-directory>...");
        std::process::exit(2);
    }

    let mut files = collect(&roots);
    if let Some(pattern) = &only {
        files.retain(|p| short(p).to_lowercase().contains(&pattern.to_lowercase()));
    }
    files.sort();

    println!(
        "{:<44} {:>11} {:>4} {:>4} {:>4} {:>8} {:>8} {:>6}  verdict",
        "file", "canvas", "ent", "fld", "pix", "dense", "held", "held%"
    );
    println!("{}", "-".repeat(128));

    let mut refused = 0usize;
    let (mut dense_total, mut held_total) = (0u64, 0u64);
    for path in &files {
        let name = short(path);
        // Printed before the read, so a document that takes the process down
        // with it is named rather than merely absent from the table.
        print!("{name:<44} ");
        let _ = std::io::stdout().flush();
        match docimport::import(path) {
            Ok(doc) => {
                let entries = doc.layers.len();
                let folders = doc.layers.iter().filter(|l| l.folder).count();
                let (dense, held) = (dense_bytes(&doc), held_bytes(&doc));
                dense_total += dense;
                held_total += held;
                println!(
                    "{:>5}x{:<5} {entries:>4} {folders:>4} {:>4} {:>8} {:>8} {:>5.1}%  opens{}",
                    doc.size.x,
                    doc.size.y,
                    entries - folders,
                    gigabytes(dense),
                    gigabytes(held),
                    100.0 * held as f64 / dense.max(1) as f64,
                    note(&doc, only.is_some()),
                );
            }
            Err(e) => {
                refused += 1;
                println!(
                    "{:>11} {:>4} {:>4} {:>4} {:>8} {:>8} {:>6}  REFUSED: {e}",
                    "-", "-", "-", "-", "-", "-", "-"
                );
            }
        }
    }
    println!("{}", "-".repeat(128));
    println!(
        "{} files; {refused} refused; {} dense, {} held ({:.1}%)",
        files.len(),
        gigabytes(dense_total),
        gigabytes(held_total),
        100.0 * held_total as f64 / dense_total.max(1) as f64,
    );
}

/// Warnings summarised rather than listed: a forty-layer document with a mask
/// on every layer should not produce forty lines in a table.
///
/// `--only` is the escape hatch, and it is what makes this tool answer "why is
/// this one file unhappy" as well as "how is the folder": once the sweep is
/// down to a handful of documents, every sentence is printed in full. That is
/// the reading somebody actually wants when they have a warning in front of
/// them and no idea which layer caused it.
fn note(doc: &ImportedDocument, verbose: bool) -> String {
    match doc.warnings.len() {
        0 => String::new(),
        1 => format!(" (1 warning: {})", doc.warnings[0]),
        n if verbose => {
            let mut out = format!(" ({n} warnings)");
            for w in &doc.warnings {
                out.push_str(&format!("\n{:>4}- {w}", ""));
            }
            out
        }
        n => format!(" ({n} warnings)"),
    }
}

fn collect(roots: &[PathBuf]) -> Vec<PathBuf> {
    let readable = docimport::supported_extensions();
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
    files
}

fn short(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string();
    name.chars().take(43).collect()
}
