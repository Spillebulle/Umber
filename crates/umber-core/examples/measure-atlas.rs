//! What a document costs the tile atlas, against what it cost a dense array.
//!
//! ```sh
//! cargo run --release -p umber-core --example measure-atlas -- ~/Desktop
//! cargo run --release -p umber-core --example measure-atlas -- --only valorant ~/Desktop
//! ```
//!
//! `survey-residency` answers "how much of a real layer holds anything", which
//! is the question that decided whether the tile atlas was worth building. This
//! answers the one after it: **how many pages does the renderer actually
//! reserve**, which is what decides whether a given document opens on a given
//! card.
//!
//! It is deliberately the *same arithmetic* `App::install_import` runs, over the
//! *same* piece set — `Opened::uploads` — rather than a second estimate beside
//! it. A figure computed a second way is a figure that can be right about a
//! model nobody ships.
//!
//! Three numbers per document:
//!
//! - **dense** is what the layer array cost before there was an atlas: one
//!   canvas-sized slice per painted layer and per mask.
//! - **paged** is what phase 1 cost: the same count, but a *page* — the canvas
//!   rounded up to whole tiles — which is a loss of between 3.5% and 64%
//!   depending on how far the canvas is from a multiple of 256.
//! - **atlas** is what this build reserves: the tiles the pieces reach, rounded
//!   up to whole pages.
//!
//! The tile count is an **upper bound on what will be backed**, not an estimate,
//! because a `.clip` states which blocks the artist *touched* rather than where
//! paint survived — measured at 1.13× corpus-wide and 1.58× worst case by
//! `survey-residency`. That is the right direction for a reservation.
//!
//! Only `.clip` and `.ora` are read here, which is what the real documents are
//! in. Reading is the ordinary import path, so this costs whatever opening the
//! document costs; it is a measurement, not something on any hot path.

use std::path::{Path, PathBuf};

use umber_core::docimport;
use umber_core::tile::Grid;

fn gigabytes(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.2} GB", bytes as f64 / (1u64 << 30) as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64)
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut only: Option<String> = None;
    let mut root: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => only = args.next().map(|s| s.to_lowercase()),
            _ => root = Some(PathBuf::from(arg)),
        }
    }
    let Some(root) = root else {
        eprintln!("usage: measure-atlas [--only <substring>] <directory>");
        return;
    };

    let mut files: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("the directory should be readable")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()),
                Some("clip") | Some("ora")
            )
        })
        .filter(|p| match &only {
            Some(needle) => name_of(p).to_lowercase().contains(needle),
            None => true,
        })
        .collect();
    files.sort();

    let (mut dense_all, mut paged_all, mut atlas_all) = (0u64, 0u64, 0u64);
    println!(
        "{:<44} {:>11} {:>10} {:>10} {:>10} {:>7}",
        "document", "canvas", "dense", "paged", "atlas", "of dense"
    );
    for path in &files {
        let opened = match docimport::import(path) {
            Ok(o) => o,
            Err(e) => {
                println!("{:<44} refused: {e}", name_of(path));
                continue;
            }
        };
        let doc = opened.open();
        let size = doc.document.size;
        let grid = Grid::new(glam::UVec2::new(size.x, size.y));

        let mut tiles = 0usize;
        for upload in &doc.uploads {
            let mut seen: Vec<(u32, u32)> = upload
                .pieces
                .iter()
                .flat_map(|p| grid.tiles_over(p.rect))
                .collect();
            seen.sort_unstable();
            seen.dedup();
            tiles += seen.len();
        }
        let pages = (tiles as u64).div_ceil(u64::from(grid.tiles_per_page().max(1)));
        let slices = doc.uploads.len() as u64;
        let page = grid.page_size();
        let page_bytes = u64::from(page.x) * u64::from(page.y) * 4;
        let canvas_bytes = u64::from(size.x) * u64::from(size.y) * 4;

        let dense = slices * canvas_bytes;
        let paged = slices * page_bytes;
        let atlas = pages * page_bytes;
        dense_all += dense;
        paged_all += paged;
        atlas_all += atlas;

        println!(
            "{:<44} {:>11} {:>10} {:>10} {:>10} {:>6.1}%",
            truncate(&name_of(path), 44),
            format!("{}x{}", size.x, size.y),
            gigabytes(dense),
            gigabytes(paged),
            gigabytes(atlas),
            atlas as f64 / dense.max(1) as f64 * 100.0,
        );
    }

    println!();
    println!(
        "total   dense {}   paged {}   atlas {}   ({:.1}% of dense)",
        gigabytes(dense_all),
        gigabytes(paged_all),
        gigabytes(atlas_all),
        atlas_all as f64 / dense_all.max(1) as f64 * 100.0,
    );
}

fn name_of(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n - 1).collect::<String>() + "…"
    }
}
