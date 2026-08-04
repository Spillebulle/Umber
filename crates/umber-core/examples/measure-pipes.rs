//! What every GIMP brush pipe in the fetched packs actually is, and what a
//! cell array would cost.
//!
//! ```sh
//! pwsh tools/fetch-brushes.ps1          # or: sh tools/fetch-brushes.sh
//! cargo run --release -p umber-core --example measure-pipes
//! ```
//!
//! `.gih` is the largest single refusal in the shipped library, and every
//! argument about it turns on numbers: which selection rules occur at all, how
//! wide a pipe gets, and how much texture an array of cells would need. Those
//! numbers are quoted in `brushimport::gih`'s module docs and in
//! `docs/brush-pipes.md`, so **re-run this before changing any of them** — the
//! same rule `measure-undo.rs` and `measure-history.rs` state for theirs.
//!
//! Two of the answers are load-bearing and neither is obvious:
//!
//! - **No pipe anywhere is `angular`.** That is the whole reason the one
//!   collapse Umber's dab could reproduce natively is not built: there is
//!   nothing to check it against.
//! - **Memory is not what stands in the way of a cell array.** The widest pipe
//!   in the packs is under a megabyte, which is nothing beside the
//!   canvas-sized stroke scratch. What stands in the way is that a tip is
//!   bound once per pass and named by a single `BrushPreset::tip`.
//!
//! It is deliberately an example rather than a test, for `measure-stamp.rs`'s
//! reason: `assets/brushes/` is git-ignored, so a test would have nothing to
//! run against.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use umber_core::brushimport::gih::{self, GihPipe, Selection};

fn main() {
    let root = repo_root().join("assets/brushes");
    if !root.exists() {
        eprintln!(
            "no {} — run tools/fetch-brushes.ps1 (or .sh) first",
            root.display()
        );
        std::process::exit(1);
    }

    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();

    println!(
        "{:<46} {:>5} {:>5} {:>11} {:>10}  rules",
        "pipe", "cells", "reach", "cell", "array kB"
    );

    let mut pipes = 0usize;
    let mut by_rule: BTreeMap<String, usize> = BTreeMap::new();
    let mut widest = 0usize;
    let mut largest_array = 0usize;
    let mut total_cells = 0usize;
    let mut identical = 0usize;
    let mut dimensions_over_one = 0usize;
    let mut trimmed = 0usize;

    for (label, bytes) in files.into_iter().flat_map(read_pipes) {
        let pipe = match gih::from_gih(&bytes) {
            Ok(pipe) => pipe,
            Err(e) => {
                println!("{:<46} {e}", label);
                continue;
            }
        };
        let report = Report::of(&pipe);

        pipes += 1;
        total_cells += report.cells;
        widest = widest.max(report.cells);
        largest_array = largest_array.max(report.array_bytes);
        if pipe.rules.len() > 1 {
            dimensions_over_one += 1;
        }
        // The two exact collapses, told apart the way `from_gih` decides them:
        // pinned is a statement about the header, uniform about the pixels.
        if pipe.written > pipe.cells.len() {
            if pipe
                .rules
                .iter()
                .all(|rule| *rule == Some(Selection::Constant))
            {
                trimmed += 1;
            } else {
                identical += 1;
            }
        }
        *by_rule.entry(report.rules.clone()).or_default() += 1;

        println!(
            "{:<46} {:>5} {:>5} {:>11} {:>10.0}  {}",
            label,
            report.cells,
            report.reach,
            report.cell_size,
            report.array_bytes as f32 / 1024.0,
            report.rules,
        );
    }

    println!("\n{pipes} pipes, {total_cells} cells");
    println!("  widest                     {widest} cells");
    println!(
        "  largest cell array         {:.0} kB (every cell padded into the common box)",
        largest_array as f32 / 1024.0
    );
    println!("  more than one dimension    {dimensions_over_one}");
    println!("  collapsed: every cell the same brush   {identical}");
    println!("  collapsed: nothing walks               {trimmed}");
    println!("\nby rule:");
    for (rules, count) in &by_rule {
        println!("  {rules:<40} {count:>4}");
    }

    // The one figure the angular argument rests on, printed on its own so a
    // future run cannot leave it buried in a table.
    let angular = by_rule
        .iter()
        .filter(|(rules, _)| rules.contains("angular"))
        .map(|(_, n)| n)
        .sum::<usize>();
    println!("\nangular pipes: {angular}");
}

/// What one pipe costs and what it is.
struct Report {
    /// Cells the file holds.
    cells: usize,
    /// Cells the pipe can actually reach — what the import yields, and less
    /// than `cells` for either of the two exact collapses.
    reach: usize,
    cell_size: String,
    /// What a `texture_2d_array` of the cells would take, with every cell
    /// padded into the box that holds the largest — the shape the dab pass
    /// would need, since an array's layers share one size.
    array_bytes: usize,
    rules: String,
}

impl Report {
    fn of(pipe: &GihPipe) -> Self {
        let width = pipe.cells.iter().map(|c| c.tip.width()).max().unwrap_or(0);
        let height = pipe.cells.iter().map(|c| c.tip.height()).max().unwrap_or(0);
        let ragged = pipe
            .cells
            .iter()
            .any(|c| c.tip.width() != width || c.tip.height() != height);

        Self {
            cells: pipe.written,
            reach: pipe.cells.len(),
            cell_size: format!("{width}x{height}{}", if ragged { "*" } else { "" }),
            // Off `written` rather than the reach: what a cell array would have
            // to hold is the file's own cells, before either collapse.
            array_bytes: pipe.written * width as usize * height as usize,
            rules: describe(&pipe.rules),
        }
    }
}

/// One line naming every dimension's rule, so the table can be grouped by it.
fn describe(rules: &[Option<Selection>]) -> String {
    rules
        .iter()
        .map(|rule| match rule {
            Some(Selection::Constant) => "constant",
            Some(Selection::Incremental) => "incremental",
            Some(Selection::Random) => "random",
            Some(Selection::Angular) => "angular",
            Some(Selection::Velocity) => "velocity",
            Some(Selection::Pressure) => "pressure",
            Some(Selection::XTilt) => "xtilt",
            Some(Selection::YTilt) => "ytilt",
            Some(Selection::Unknown) => "?",
            None => "(unstated)",
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// Every pipe in one file: a loose `.gih` is one, and a `.bundle` is a zip that
/// may hold several as the tips of its presets.
fn read_pipes(path: PathBuf) -> Vec<(String, Vec<u8>)> {
    let name = |extra: &str| {
        let stem = path.file_name().unwrap_or_default().to_string_lossy();
        format!("{stem}{extra}")
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };

    if path.extension().is_some_and(|e| e == "gih") {
        return vec![(name(""), bytes)];
    }

    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return Vec::new();
    };
    // Names first, then contents: an entry borrows the archive, so the two
    // passes cannot be one.
    let mut inside = Vec::new();
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i)
            && entry.name().to_ascii_lowercase().ends_with(".gih")
        {
            inside.push(entry.name().to_string());
        }
    }

    inside
        .into_iter()
        .filter_map(|entry| {
            let mut file = archive.by_name(&entry).ok()?;
            let mut out = Vec::new();
            std::io::copy(&mut file, &mut out).ok()?;
            let leaf = entry.rsplit('/').next().unwrap_or(&entry).to_string();
            Some((name(&format!(":{leaf}")), out))
        })
        .collect()
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| e == "gih" || e == "bundle")
        {
            out.push(path);
        }
    }
}

/// The example runs from the crate directory under `cargo run -p`, so walk up
/// until the workspace manifest turns up rather than guessing a depth.
fn repo_root() -> PathBuf {
    let mut dir = std::env::current_dir().expect("current directory");
    loop {
        if dir.join("Cargo.toml").exists() && dir.join("crates").is_dir() {
            return dir;
        }
        if !dir.pop() {
            panic!("could not find the workspace root from the current directory");
        }
    }
}
