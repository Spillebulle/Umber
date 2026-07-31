//! Turn the downloaded brush packs into the library Umber ships.
//!
//! ```sh
//! pwsh tools/fetch-brushes.ps1          # or: sh tools/fetch-brushes.sh
//! cargo run -p umber-core --example build-brush-library
//! ```
//!
//! Writes `crates/umber-core/assets/builtin-brushes.ron`, which
//! [`umber_core::preset::builtin`] embeds with `include_str!`. Running it is a
//! deliberate, occasional act rather than a build script: the packs are not in
//! the repository, so a `build.rs` would make a clean checkout unbuildable, and
//! a generated file that lands in a commit is a file whose diff can be read.
//!
//! Brushes that would misrepresent themselves are refused rather than
//! converted — see `mypaint::unsupported_features`. A smudge brush imported
//! into an engine with no colour pickup is not a smudge brush, it is a lie with
//! a familiar name.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use umber_core::brushimport::{display_name, mypaint};
use umber_core::preset::{self, BrushPreset, Credit, slug};

/// Where each pack lives after a fetch, and what to say about it.
struct Pack {
    /// Directory under `assets/brushes/`.
    dir: &'static str,
    /// Prefix for generated preset ids.
    id_prefix: &'static str,
    licence: &'static str,
    source: &'static str,
    /// Per-subdirectory author and picker category. MyPaint's pack is sorted
    /// into one directory per artist, and its `Licenses.dep5` attributes them
    /// individually, so the credit has to be per-directory rather than
    /// per-pack.
    authors: &'static [(&'static str, &'static str, &'static str)],
    fallback_author: &'static str,
    fallback_category: &'static str,
}

const PACKS: &[Pack] = &[Pack {
    dir: "mypaint",
    id_prefix: "mypaint",
    licence: "CC0-1.0",
    source: "https://github.com/mypaint/mypaint-brushes",
    // (subdirectory, author, category) — taken from the pack's Licenses.dep5.
    authors: &[
        ("classic", "MyPaint Development Team", "MyPaint — classic"),
        (
            "experimental",
            "MyPaint Development Team",
            "MyPaint — experimental",
        ),
        ("deevad", "David Revoy", "MyPaint — David Revoy"),
        ("ramon", "Ramón Miranda", "MyPaint — Ramón Miranda"),
        (
            "tanda",
            "Marcelo \"Tanda\" Cerviño",
            "MyPaint — Marcelo Cerviño",
        ),
        (
            "kaerhon_v1",
            "Guillaume Loussarévian",
            "MyPaint — Guillaume Loussarévian",
        ),
        ("Dieterle", "Brien Dieterle", "MyPaint — Brien Dieterle"),
    ],
    fallback_author: "MyPaint Development Team",
    fallback_category: "MyPaint",
}];

fn main() {
    let repo_root = repo_root();
    let brush_root = repo_root.join("assets/brushes");
    if !brush_root.exists() {
        eprintln!(
            "no {} — run tools/fetch-brushes.ps1 (or .sh) first",
            brush_root.display()
        );
        std::process::exit(1);
    }

    let mut presets: Vec<BrushPreset> = Vec::new();
    let mut skipped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut failed: Vec<String> = Vec::new();

    for pack in PACKS {
        let dir = brush_root.join(pack.dir);
        if !dir.exists() {
            eprintln!("skipping {}: not downloaded", pack.dir);
            continue;
        }

        let mut files = Vec::new();
        collect(&dir, "myb", &mut files);
        // `read_dir` order is whatever the filesystem feels like, and this file
        // is committed — sorting is what keeps the diff meaningful.
        files.sort();

        for file in files {
            let relative = file.strip_prefix(&dir).unwrap_or(&file);
            let group = relative
                .parent()
                .and_then(|p| p.file_name())
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let stem = relative
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();

            let text = match fs::read_to_string(&file) {
                Ok(t) => t,
                Err(e) => {
                    failed.push(format!("{}: {e}", file.display()));
                    continue;
                }
            };

            match mypaint::unsupported_features(&text) {
                Ok(reasons) if !reasons.is_empty() => {
                    skipped
                        .entry(reasons.join(", "))
                        .or_default()
                        .push(format!("{group}/{stem}"));
                    continue;
                }
                Err(e) => {
                    failed.push(format!("{}: {e}", file.display()));
                    continue;
                }
                Ok(_) => {}
            }

            let brush = match mypaint::from_myb(&text) {
                Ok(b) => b,
                Err(e) => {
                    failed.push(format!("{}: {e}", file.display()));
                    continue;
                }
            };

            let (author, category) = pack
                .authors
                .iter()
                .find(|(sub, _, _)| *sub == group)
                .map(|(_, author, category)| (*author, *category))
                .unwrap_or((pack.fallback_author, pack.fallback_category));

            presets.push(BrushPreset {
                // The pack's own directory layout is part of the id: two
                // artists both shipping a "Charcoal" is normal, and the ids
                // have to stay distinct without renaming either.
                id: format!("{}/{}/{}", pack.id_prefix, slug(&group), slug(&stem)),
                name: display_name(&stem),
                category: category.to_string(),
                credit: Some(Credit {
                    author: author.to_string(),
                    licence: pack.licence.to_string(),
                    source: pack.source.to_string(),
                }),
                brush,
            });
        }
    }

    if presets.is_empty() {
        eprintln!("no brushes converted; is assets/brushes/ populated?");
        std::process::exit(1);
    }

    let out = repo_root.join("crates/umber-core/assets/builtin-brushes.ron");
    let body = preset::to_ron(&presets).expect("serialise the library");
    let header = "\
// Generated by `cargo run -p umber-core --example build-brush-library`.
// Do not edit by hand — regenerate instead. Umber's own presets live in
// `umber_defaults` in `src/preset.rs`, not here.
//
// Every brush below is CC0; see assets/brushes/LICENSES.md for the packs and
// docs/brushes.md for what the conversion keeps and what it drops.
";
    fs::write(&out, format!("{header}{body}\n")).expect("write the library");

    println!("{} brushes -> {}", presets.len(), out.display());
    for (reason, names) in &skipped {
        println!("skipped {} for {reason}:", names.len());
        println!("  {}", names.join(", "));
    }
    for problem in &failed {
        println!("failed: {problem}");
    }
}

/// Recursively gather files with a given extension.
fn collect(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, extension, out);
        } else if path.extension().is_some_and(|e| e == extension) {
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
