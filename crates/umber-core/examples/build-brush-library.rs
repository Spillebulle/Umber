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
//! # What is refused, and why the generator is fussier than the importer
//!
//! A brush the *user* imports is theirs, and a usable approximation of it beats
//! a refusal. A brush Umber **ships under somebody's name** is a claim about
//! that person's work, so the rule here is the opposite: anything that would
//! paint unlike the original is left out. Two things trigger that.
//!
//! - **It lost something on the way in.** `Imported::dropped` names it, per
//!   brush, and a non-empty list is a refusal. That is the same check
//!   `mypaint::unsupported_features` used to be, generalised to every format.
//! - **It needs a bitmap tip.** The shipped library is a single embedded RON
//!   and there is nowhere in it for a mask — `BrushPreset::tip` resolves
//!   against the *user's* library only. Shipping stamps needs a generated
//!   `assets/tips/` and an `include_bytes!` table beside this file, and the
//!   measurement that decides which stamps reproduce faithfully under a
//!   `max`-coverage stroke. See `docs/brush-sources.md`, which has the numbers.
//!
//! The counts for both are printed on every run, because they are the honest
//! answer to "why is my favourite brush not in here".

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use umber_core::brushimport::{self, Imported};
use umber_core::preset::{self, BrushPreset, Credit};
use umber_core::style;

/// Where each pack lives after a fetch, and what to say about it.
struct Pack {
    /// Directory under `assets/brushes/`.
    dir: &'static str,
    /// Prefix for generated preset ids.
    id_prefix: &'static str,
    licence: &'static str,
    source: &'static str,
    /// Per-subdirectory author. MyPaint's pack is sorted into one directory
    /// per artist, and its `Licenses.dep5` attributes them individually, so the
    /// credit has to be per-directory rather than per-pack.
    ///
    /// No category here: what a brush *is* comes from `umber_core::style`,
    /// which reads the brush, not the folder it arrived in.
    authors: &'static [(&'static str, &'static str)],
    fallback_author: &'static str,
}

/// Every row here must have a matching entry in `tools/fetch-brushes.ps1` and
/// its `.sh` twin, and the licences must agree — `assets/brushes/LICENSES.md`
/// is generated from those and is the record that has actually been checked
/// against the downloads.
const PACKS: &[Pack] = &[
    Pack {
        dir: "mypaint",
        id_prefix: "mypaint",
        licence: "CC0-1.0",
        source: "https://github.com/mypaint/mypaint-brushes",
        // (subdirectory, author) — taken from the pack's Licenses.dep5.
        authors: &[
            ("classic", "MyPaint Development Team"),
            ("experimental", "MyPaint Development Team"),
            ("deevad", "David Revoy"),
            ("ramon", "Ramón Miranda"),
            ("tanda", "Marcelo \"Tanda\" Cerviño"),
            ("kaerhon_v1", "Guillaume Loussarévian"),
            ("Dieterle", "Brien Dieterle"),
        ],
        fallback_author: "MyPaint Development Team",
    },
    Pack {
        dir: "deevad",
        id_prefix: "deevad",
        licence: "CC0-1.0",
        source: "https://www.davidrevoy.com/article1060/krita-brushes-2025-01-bundle",
        authors: &[],
        fallback_author: "David Revoy (Deevad)",
    },
    Pack {
        dir: "raghukamath",
        id_prefix: "raghukamath",
        licence: "CC0-1.0",
        source: "https://gitlab.com/raghukamath/krita-brush-presets",
        authors: &[],
        fallback_author: "Raghavendra Kamath",
    },
    // CC-BY rather than CC0, which is why the credit matters here in a way it
    // does not for the others: `every_shipped_preset_is_usable_and_attributed`
    // is what stops one of these shipping without an author.
    Pack {
        dir: "gdquest",
        id_prefix: "gdquest",
        licence: "CC-BY-4.0",
        source: "https://github.com/GDQuest/krita-free-brushes",
        authors: &[],
        fallback_author: "GDquest (Nathan Lovato)",
    },
    // Every brush in this one is a bitmap stamp, so every one of them is
    // refused below with "needs a bitmap tip". It is listed anyway, and
    // deliberately: the run then *prints* that 269 brushes were fetched and
    // none could ship, which is a far better record than the pack silently not
    // being here. It is importable today through Import brushes…
    Pack {
        dir: "rubberduck",
        id_prefix: "rubberduck",
        licence: "CC0-1.0",
        source: "https://opengameart.org/content/60-free-gimp-krita-brushes",
        authors: &[],
        fallback_author: "rubberduck",
    },
];

/// Extensions the packs actually contain. `.abr` is readable and no shipped
/// pack is one, so listing it here would only slow the walk down.
const EXTENSIONS: &[&str] = &["myb", "gbr", "gpb", "gih", "vbr", "kpp", "bundle"];

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
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();

    for pack in PACKS {
        let dir = brush_root.join(pack.dir);
        if !dir.exists() {
            eprintln!("skipping {}: not downloaded", pack.dir);
            continue;
        }

        let mut files = Vec::new();
        collect(&dir, &mut files);
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

            let found = match brushimport::read_file(&file) {
                Ok(found) => found,
                Err(e) => {
                    failed.push(format!("{}: {e}", file.display()));
                    continue;
                }
            };
            let label = |name: &str| format!("{}/{name}", pack.dir);

            for Imported {
                mut preset,
                tip,
                dropped,
            } in found
            {
                // Two refusals, both about not misrepresenting somebody's work.
                // See the module docs.
                if tip.is_some() {
                    skipped
                        .entry("needing a bitmap tip".to_string())
                        .or_default()
                        .push(label(&preset.name));
                    continue;
                }
                if !dropped.is_empty() {
                    skipped
                        .entry(dropped.join(", "))
                        .or_default()
                        .push(label(&preset.name));
                    continue;
                }

                let author = pack
                    .authors
                    .iter()
                    .find(|(sub, _)| *sub == group)
                    .map(|(_, author)| *author)
                    .unwrap_or(pack.fallback_author);

                // The pack's own directory layout is part of the id: two
                // artists both shipping a "Charcoal" is normal, and the ids
                // have to stay distinct without renaming either. A pack that is
                // one flat directory — every Krita one — needs a counter
                // instead, because two of its presets can slug the same.
                let stem = preset::slug(&preset.name);
                let base = if group.is_empty() || group == pack.dir {
                    format!("{}/{stem}", pack.id_prefix)
                } else {
                    format!("{}/{}/{stem}", pack.id_prefix, preset::slug(&group))
                };
                let n = seen.entry(base.clone()).or_insert(0);
                *n += 1;
                preset.id = if *n == 1 { base } else { format!("{base}-{n}") };

                // Grouped by the mark it makes, not by who drew it. The pack's
                // own layout is still the source of *credit*, which travels on
                // the brush and is shown on every row of the browser.
                preset.category = style::classify(&preset.name, &preset.brush).to_string();
                // A `.bundle` states its own author in `meta.xml`, and the
                // reader already put it on the preset. The table here is the
                // fallback and the cross-check, not a replacement — but the
                // licence must be the one the fetch script verified, because
                // that is the one somebody actually read.
                preset.credit = Some(Credit {
                    author: preset
                        .credit
                        .as_ref()
                        .map(|c| c.author.clone())
                        .filter(|a| !a.is_empty())
                        .unwrap_or_else(|| author.to_string()),
                    licence: pack.licence.to_string(),
                    source: pack.source.to_string(),
                });
                preset.tip = None;
                presets.push(preset);
            }
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
// Every brush below carries its licence in its own `credit`; see
// assets/brushes/LICENSES.md for the packs and docs/brushes.md for what the
// conversion keeps and what it drops.
";
    fs::write(&out, format!("{header}{body}\n")).expect("write the library");

    println!("{} brushes -> {}", presets.len(), out.display());

    // Printed so the classification can actually be checked. A rule that sends
    // half the library into one collection is not something a test catches —
    // it is a judgement, and judgements need to be looked at.
    println!("\nby collection:");
    for category in style::Style::ALL {
        let members: Vec<&str> = presets
            .iter()
            .filter(|p| p.category == category)
            .map(|p| p.name.as_str())
            .collect();
        println!(
            "  {:26} {:3}  {}",
            category,
            members.len(),
            members.join(", ")
        );
    }

    println!("\nby pack:");
    for pack in PACKS {
        let prefix = format!("{}/", pack.id_prefix);
        let kept = presets.iter().filter(|p| p.id.starts_with(&prefix)).count();
        println!("  {:14} {kept:4} shipped ({})", pack.dir, pack.licence);
    }

    println!();
    for (reason, names) in &skipped {
        println!("skipped {} for {reason}:", names.len());
        // The list is what makes the count checkable, but 269 stamp names is a
        // wall rather than a record.
        let shown: Vec<&str> = names.iter().take(12).map(String::as_str).collect();
        println!(
            "  {}{}",
            shown.join(", "),
            if names.len() > shown.len() {
                format!(", … and {} more", names.len() - shown.len())
            } else {
                String::new()
            }
        );
    }
    for problem in &failed {
        println!("failed: {problem}");
    }
}

/// Recursively gather every brush file a pack might hold.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| {
            EXTENSIONS.contains(&e.to_string_lossy().to_ascii_lowercase().as_str())
        }) {
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
