//! Fingerprint what a folder of documents *imports as*, one line per layer.
//!
//! ```sh
//! cargo run --release -p umber-core --example hash-layers -- ~/Desktop
//! ```
//!
//! Written for one question and it is the only one it answers: **did a change
//! to the readers move a pixel?** Run it before the change and after it and
//! `diff` the two outputs. Anything that comes back identical is a document
//! whose every layer arrived byte for byte as it did.
//!
//! The hash is over the **assembled canvas**, not over whatever the reader
//! happened to hand back — a canvas-sized buffer in one build and a sequence of
//! pieces in another are the same picture, and the picture is what must not
//! move. That is also why this is not a test: the comparison is against a
//! *different build*, which nothing inside one build can perform.
//!
//! FNV-1a rather than a crate, because the only property wanted is that two
//! different pictures do not agree, and a hash nobody has to install is a hash
//! somebody will actually run.

use std::io::Write;
use std::path::{Path, PathBuf};

use glam::UVec2;
use umber_core::docimport::{self, ImportedLayer};

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The layer as a canvas, however this build chose to carry it.
///
/// **This is the line that differs between the two builds being compared**, and
/// it is deliberately the only one: before the piece contract it was
/// `layer.pixels.clone()`.
fn canvas_of(layer: &ImportedLayer, size: UVec2) -> Vec<u8> {
    let stride = size.x as usize * 4;
    let mut out = vec![0u8; stride * size.y as usize];
    for piece in &layer.pixels {
        for row in 0..piece.rect.height as usize {
            let src = row * piece.rect.width as usize * 4;
            let dst = (piece.rect.y as usize + row) * stride + piece.rect.x as usize * 4;
            let len = piece.rect.width as usize * 4;
            out[dst..dst + len].copy_from_slice(&piece.bytes[src..src + len]);
        }
    }
    out
}

fn mask_of(layer: &ImportedLayer, size: UVec2) -> Option<Vec<u8>> {
    let mask = layer.mask.as_ref()?;
    let stride = size.x as usize * 4;
    let mut out = vec![0u8; stride * size.y as usize];
    for piece in mask {
        for row in 0..piece.rect.height as usize {
            let src = row * piece.rect.width as usize * 4;
            let dst = (piece.rect.y as usize + row) * stride + piece.rect.x as usize * 4;
            let len = piece.rect.width as usize * 4;
            out[dst..dst + len].copy_from_slice(&piece.bytes[src..src + len]);
        }
    }
    Some(out)
}

fn main() {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut only: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--only" => only = args.next(),
            _ => roots.push(PathBuf::from(arg)),
        }
    }
    if roots.is_empty() {
        eprintln!("usage: hash-layers [--only <substring>] <file-or-directory>...");
        std::process::exit(2);
    }

    let mut files = collect(&roots);
    if let Some(pattern) = &only {
        files.retain(|p| short(p).to_lowercase().contains(&pattern.to_lowercase()));
    }
    files.sort();

    for path in &files {
        let name = short(path);
        match docimport::import(path) {
            Ok(doc) => {
                println!("{name}\tcanvas {}x{}", doc.size.x, doc.size.y);
                for (i, layer) in doc.layers.iter().enumerate() {
                    if layer.folder {
                        println!("{name}\t{i}\tfolder\t{}", layer.name);
                        continue;
                    }
                    let pixels = canvas_of(layer, doc.size);
                    let mask = mask_of(layer, doc.size);
                    println!(
                        "{name}\t{i}\t{:016x}\t{}\t{}",
                        fnv1a(&pixels),
                        mask.map_or("-".to_string(), |m| format!("{:016x}", fnv1a(&m))),
                        layer.name
                    );
                }
                // Every warning, verbatim: a change that silently dropped one
                // or invented one would otherwise not show up in the diff.
                for w in &doc.warnings {
                    println!("{name}\twarning\t{w}");
                }
            }
            Err(e) => println!("{name}\tREFUSED\t{e}"),
        }
        let _ = std::io::stdout().flush();
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
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?")
        .to_string()
}
