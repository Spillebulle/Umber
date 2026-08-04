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
        "{:<56} {:>5} {:>5} {:>11} {:>10}  rules",
        "pipe", "cells", "reach", "cell", "array kB"
    );

    let mut pipes = 0usize;
    let mut by_rule: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_pack: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    let mut widest = 0usize;
    let mut largest_array = 0usize;
    let mut total_cells = 0usize;
    let mut identical = 0usize;
    let mut dimensions_over_one = 0usize;
    let mut trimmed = 0usize;

    for (label, bytes) in files.into_iter().flat_map(|path| read_pipes(&root, path)) {
        let pipe = match gih::from_gih(&bytes) {
            Ok(pipe) => pipe,
            Err(e) => {
                println!("{:<56} {e}", label.name);
                continue;
            }
        };
        let report = Report::of(&pipe);

        pipes += 1;
        total_cells += report.cells;
        widest = widest.max(report.cells);
        largest_array = largest_array.max(report.array_bytes.unwrap_or(0));
        // Per pack, because the shipped library's accounting is per pack: it is
        // the cells in *rubberduck's* pipes that decide how much of the `.gih`
        // refusal a mask licence would still hold back whatever the engine did.
        let pack = by_pack.entry(label.pack.clone()).or_default();
        pack.0 += 1;
        pack.1 += report.cells;
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
            "{:<56} {:>5} {:>5} {:>11} {:>10}  {}",
            label.name,
            report.cells,
            report.reach,
            report.cell_size,
            match report.array_bytes {
                Some(bytes) => format!("{:.0}", bytes as f32 / 1024.0),
                // A collapsed pipe would never be a cell array, and the cells
                // it dropped are not here to be measured. See `Report`.
                None => "—".to_string(),
            },
            report.rules,
        );
    }

    println!("\n{pipes} pipes, {total_cells} cells");
    println!("  widest                                 {widest} cells");
    println!(
        "  largest cell array                     {:.0} kB (every cell padded into the common box)",
        largest_array as f32 / 1024.0
    );
    println!("  more than one dimension                {dimensions_over_one}");
    println!("  collapsed: every cell the same brush   {identical}");
    println!("  collapsed: nothing walks               {trimmed}");
    println!("\nby rule:");
    for (rules, count) in &by_rule {
        println!("  {rules:<40} {count:>4}");
    }
    println!("\nby pack (pipes, cells):");
    for (pack, (pipes, cells)) in &by_pack {
        println!("  {pack:<40} {pipes:>4} {cells:>6}");
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
    ///
    /// `None` for a **collapsed** pipe, and that is the honest answer rather
    /// than a gap: `GihPipe::cells` is what the pipe can reach, so the cells a
    /// collapse dropped are not here to be measured — and a pipe that collapses
    /// would never be a cell array in the first place. Reporting the box over
    /// the survivors and multiplying by the file's count would be a figure
    /// built from two different sets, which is the kind of number that survives
    /// because it looks deliberate.
    array_bytes: Option<usize>,
    rules: String,
}

impl Report {
    fn of(pipe: &GihPipe) -> Self {
        let width = pipe.cells.iter().map(|c| c.tip.width()).max().unwrap_or(0);
        let height = pipe.cells.iter().map(|c| c.tip.height()).max().unwrap_or(0);
        // A pipe whose cells differ in size is legal and is what an array's
        // shared layer size has to be padded for. None of the 55 is one, which
        // is worth knowing and is why the mark is printed rather than assumed.
        let ragged = pipe
            .cells
            .iter()
            .any(|c| c.tip.width() != width || c.tip.height() != height);
        let whole = pipe.cells.len() == pipe.written;

        Self {
            cells: pipe.written,
            reach: pipe.cells.len(),
            cell_size: format!("{width}x{height}{}", if ragged { "*" } else { "" }),
            array_bytes: whole.then(|| pipe.written * width as usize * height as usize),
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

/// Where a pipe came from: the pack directory, and something to print.
struct Label {
    pack: String,
    name: String,
}

/// Every pipe in one file: a loose `.gih` is one, and a `.bundle` is a zip that
/// may hold several as the tips of its presets.
fn read_pipes(root: &Path, path: PathBuf) -> Vec<(Label, Vec<u8>)> {
    // The first component under `assets/brushes` is the pack, which is the unit
    // the licence decision is made in.
    let pack = path
        .strip_prefix(root)
        .ok()
        .and_then(|rest| rest.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = |extra: &str| {
        let stem = path.file_name().unwrap_or_default().to_string_lossy();
        Label {
            pack: pack.clone(),
            name: format!("{stem}{extra}"),
        }
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };

    if path.extension().is_some_and(|e| e == "gih") {
        return vec![(label(""), bytes)];
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
            Some((label(&format!(":{leaf}")), out))
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
