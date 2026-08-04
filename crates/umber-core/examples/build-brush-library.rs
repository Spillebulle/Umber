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
//! - **Its pack's masks may not be redistributed.** See below.
//!
//! The counts for both are printed on every run, because they are the honest
//! answer to "why is my favourite brush not in here".
//!
//! # Bitmap tips ship now
//!
//! This used to refuse every brush with a mask, because the shipped library is
//! a single embedded RON and there is nowhere in a text file for a bitmap. The
//! mask goes beside it instead: written to `crates/umber-core/assets/tips/` and
//! named from `BrushPreset::tip`, which resolves against `tip::builtin` before
//! the user's library. Masks are **deduplicated by content**, so two brushes
//! cut from one stamp share a file and a single GPU upload — which is the whole
//! reason the field holds a name rather than a picture.
//!
//! The engine had to be able to paint them first, and now can: a tip keeps its
//! own proportions, turns with the stroke or rolls per dab, and builds up when
//! it is too faint for a `max` to reach the mark its author drew. Each of those
//! is measured or tested rather than assumed — `docs/brushes.md` says where.
//!
//! # And so do paper textures
//!
//! The same arrangement, one directory over: a Krita preset's pattern is
//! levelled at import, written to `assets/patterns/` beside Umber's own three
//! tiles, and named from `BrushPreset::paper`, which `Editor::paper_tile`
//! resolves against the user's library and then against `tip::pattern`. This
//! used to be a refusal on the reasoning that the shipped library had nowhere
//! to put a picture, which was true of the RON and false of the lookup.
//!
//! Only Krita's **Multiply** texturing mode comes across, because that is the
//! one whose arithmetic Umber's `mix(1.0, tile, strength)` already is; the
//! other four the packs use are named as losses by `brushimport::kpp` and
//! refused here like any other loss. See that module's own section.
//!
//! # Shipping a mask is redistributing artwork
//!
//! Shipping a brush's *settings* is a description of somebody's work. Shipping
//! its *mask* is the work itself, inside the binary and inside this repository,
//! so it needs the licence rule at the top of `docs/brush-sources.md` to be met
//! in full: **verified from the download's own files**. [`Pack::ship_tips`] is
//! that decision, per pack, and it is not the same question as whether the pack
//! may be converted at all.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use umber_core::brushimport::{self, Imported};
use umber_core::preset::{self, BrushPreset, Credit};
use umber_core::style;
use umber_core::tip::TipMask;

#[path = "common/table.rs"]
mod table;
use table::write_table;

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
    /// Whether this pack's **masks** may be embedded in the binary.
    ///
    /// A separate decision from whether the pack is converted at all, and a
    /// stricter one. Converting a `.gbr` into settings on a machine is a local
    /// act; shipping the bitmap is redistributing the artwork, in every release
    /// on every platform, so it needs a licence verified from inside the
    /// download — the rule at the top of `docs/brush-sources.md`. A pack that
    /// says `false` still converts, still imports, and still has every one of
    /// its stamps available through **Import brushes…**; what it does not do is
    /// travel inside Umber.
    ship_tips: bool,
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
        // A `.myb` is always a round dab, so there is no mask here either way.
        ship_tips: true,
    },
    Pack {
        dir: "deevad",
        id_prefix: "deevad",
        licence: "CC0-1.0",
        source: "https://www.davidrevoy.com/article1060/krita-brushes-2025-01-bundle",
        authors: &[],
        fallback_author: "David Revoy (Deevad)",
        // CC0 stated in the bundle's own `meta.xml`, which is a licence
        // statement inside the download.
        ship_tips: true,
    },
    Pack {
        dir: "raghukamath",
        id_prefix: "raghukamath",
        licence: "CC0-1.0",
        source: "https://gitlab.com/raghukamath/krita-brush-presets",
        authors: &[],
        fallback_author: "Raghavendra Kamath",
        // CC0 stated in the repository's `LICENSE` and `README.md`, both of
        // which are in any archive of it.
        ship_tips: true,
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
        // CC-BY-4.0 stated in the repository's `README.md`. Attribution is a
        // condition rather than an obstacle: every preset carries a `Credit`
        // and the browser prints it on the row, which is what CC-BY asks for
        // and what `every_shipped_preset_is_usable_and_attributed` enforces.
        ship_tips: true,
    },
    // Every brush in this one is a bitmap stamp and none of them ships, so the
    // run prints that 269 were fetched and 269 were refused — a far better
    // record than the pack silently not being here. Every one of them imports
    // today through Import brushes….
    Pack {
        dir: "rubberduck",
        id_prefix: "rubberduck",
        licence: "CC0-1.0",
        source: "https://opengameart.org/content/60-free-gimp-krita-brushes",
        authors: &[],
        fallback_author: "rubberduck",
        // **The one pack whose masks are not shipped**, and the reason is the
        // licence rather than the size. Its CC0 is declared on the OpenGameArt
        // submission page and nowhere inside the download — the recorded
        // exception in `docs/brush-sources.md`, made so the pack could be
        // fetched and read. Redistributing 17 masks of somebody's artwork is a
        // larger claim than converting them locally, and it is not one this
        // project makes on evidence it could not check. The cost is exactly 17
        // brushes and 1.2 MB; flipping this to `true` is all it would take, and
        // that is a decision for whoever owns the project rather than a default.
        ship_tips: false,
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
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let tips_dir = repo_root.join("crates/umber-core/assets/tips");
    let mut tips = BitmapLibrary::open(&tips_dir);
    // The papers go in the directory Umber's own three tiles are already in,
    // and `BitmapLibrary` owns only what a *pack* put there — so `tooth`, `canvas`
    // and `grit` are `build-bitmaps.rs`'s and are left alone, exactly as
    // Umber's own stamps are in `tips/`.
    let papers_dir = repo_root.join("assets/patterns");
    let mut papers = BitmapLibrary::open(&papers_dir);

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

            // A container reports its own refusals separately, because a
            // `.bundle` of forty-six brushes must not be lost to the one
            // written by a paint engine Umber does not have. Collected here or
            // the run's report counts a `.kpp` refused on its own and passes
            // over the identical preset sitting inside an archive — which is
            // six of the thirteen in these packs, and a refusal table that
            // undercounts is a measuring instrument that lies.
            for reason in brushimport::refusals(&file) {
                failed.push(format!("{}: {reason}", file.display()));
            }
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
                paper,
                dropped,
            } in found
            {
                // Two refusals, both about not misrepresenting somebody's work.
                // See the module docs. What was lost on the way in is asked
                // first, because it is the more informative answer: a brush
                // that both carries a mask and dropped something is refused for
                // the thing it dropped whatever happens to its mask.
                if !dropped.is_empty() {
                    skipped
                        .entry(dropped.join(", "))
                        .or_default()
                        .push(label(&preset.name));
                    refusals.push(Refusal {
                        pack: pack.dir,
                        reasons: dropped.iter().map(|r| r.to_string()).collect(),
                    });
                    continue;
                }
                // A paper travels the way a mask does: beside the library
                // rather than in it, named from the preset. It used to be
                // refused here, on the reasoning that `BrushPreset::paper`
                // names a picture in the *user's* library and the shipped one
                // is an embedded RON — which was true of the RON and false of
                // the lookup: `Editor::paper_tile` already falls through to
                // `tip::pattern`, so a tile in `assets/patterns/` resolves for
                // a shipped brush exactly as a tip in `assets/tips/` does.
                //
                // The redistribution rule is the mask's, and for the same
                // reason: a pattern is somebody's artwork rather than a
                // description of it, so a pack whose licence was not verified
                // for its bitmaps may not ship its papers either.
                if paper.is_some() && !pack.ship_tips {
                    skipped
                        .entry(NOT_REDISTRIBUTED.to_string())
                        .or_default()
                        .push(label(&preset.name));
                    refusals.push(Refusal {
                        pack: pack.dir,
                        reasons: vec![NOT_REDISTRIBUTED.to_string()],
                    });
                    continue;
                }
                // A stamp too faint for eight-bit coverage to accumulate is a
                // brush that paints nothing however hard it is pressed, and
                // `tip::stroke_coverage` is the same measurement `build_up` is
                // decided by four lines further down every reader. Measured at
                // the densest spacing the engine is ever asked for rather than
                // at this brush's own, because that is the *loosest* reading —
                // closer dabs build higher — so a mask refused here could not
                // have made a mark at any spacing.
                //
                // It is `every_shipped_tip_decodes_and_makes_a_mark`'s rule,
                // applied where the file is written instead of only where it is
                // read: that test fails the build on a mask this generator has
                // already committed, which is a red suite pointing at an asset
                // rather than at the decision that produced it. Reached first
                // by Raghukamath's "Pack01 Clouds", whose stamp peaks at an
                // alpha of 67 and which only became eligible at all once a
                // coloured `.gih` cell stopped being refused for its colour.
                if let Some(mask) = &tip
                    && !umber_core::tip::stroke_coverage(mask, 0.1).is_usable()
                {
                    skipped
                        .entry("a stamp too faint to make a mark".to_string())
                        .or_default()
                        .push(label(&preset.name));
                    continue;
                }
                if tip.is_some() && !pack.ship_tips {
                    skipped
                        .entry(NOT_REDISTRIBUTED.to_string())
                        .or_default()
                        .push(label(&preset.name));
                    refusals.push(Refusal {
                        pack: pack.dir,
                        reasons: vec![NOT_REDISTRIBUTED.to_string()],
                    });
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
                // The mask travels beside the library rather than in it, named
                // from the preset. Deduplicated by content: two brushes cut
                // from one stamp get one file, one embedded copy and one GPU
                // upload, which is what `BrushPreset::tip` holding a name
                // rather than a picture is for.
                preset.tip = tip.map(|mask| tips.store(pack.id_prefix, &preset.id, mask));
                // And the paper the same way, into the directory Umber's own
                // three tiles already live in. Deduplicated by content like the
                // masks, which matters more here: four of Revoy's presets bite
                // through one 280-texel tile, and three of Raghukamath's
                // through one 512-texel one — but only where the *levelled*
                // tiles agree, since brightness, inversion and the cutoffs are
                // baked in, so two brushes tweaking one pattern differently
                // correctly get two files.
                preset.paper = paper.map(|tile| papers.store(pack.id_prefix, &preset.id, tile));
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

    // The masks, and the table that embeds them. Written after the presets
    // rather than as they are found, so a run that failed half way cannot leave
    // `assets/tips/` describing a library that was never written.
    let written = tips.finish();
    write_table(
        &repo_root.join("crates/umber-core/src/tip_table.rs"),
        "tip",
        "TIPS",
        "../assets/tips",
        &tips_dir,
    );
    let papers_written = papers.finish();
    write_table(
        &repo_root.join("crates/umber-core/src/pattern_table.rs"),
        "pattern",
        "PATTERNS",
        "../../../assets/patterns",
        &papers_dir,
    );

    println!("{} brushes -> {}", presets.len(), out.display());
    println!(
        "{} masks, {:.0} kB -> {}",
        written.count,
        written.bytes as f32 / 1024.0,
        tips_dir.display()
    );
    println!(
        "{} papers, {:.0} kB -> {}",
        papers_written.count,
        papers_written.bytes as f32 / 1024.0,
        papers_dir.display()
    );

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
        let mine = || presets.iter().filter(|p| p.id.starts_with(&prefix));
        let kept = mine().count();
        let stamped = mine().filter(|p| p.tip.is_some()).count();
        let papered = mine().filter(|p| p.paper.is_some()).count();
        println!(
            "  {:14} {kept:4} shipped, {stamped:3} of them stamps, {papered:3} papered ({}{})",
            pack.dir,
            pack.licence,
            if pack.ship_tips {
                ""
            } else {
                ", masks not redistributed"
            }
        );
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

    report_refusals(&refusals);
}

/// One brush the generator would not ship, and what it named.
///
/// Kept beside `skipped` rather than derived from it because that map is keyed
/// by the *combination* of reasons — which is what makes its listing checkable
/// against a single brush, and useless for "how many would this fix unlock".
/// Fifty-two combinations of fifteen reasons is not a table anybody can add up
/// by eye, and both figures this file's callers want are per **reason**.
struct Refusal {
    pack: &'static str,
    reasons: Vec<String>,
}

/// The one refusal that is not about rendering. Named once so the report and
/// the refusal cannot disagree about its wording.
const NOT_REDISTRIBUTED: &str = "a mask this project does not redistribute";

/// Print the refusals as reason × pack, with the count each fix would unlock.
///
/// The **alone** column is the one to read: a brush refused for one reason is a
/// brush that ships the day that reason goes away, where one naming three is
/// three pieces of work. Without it the totals mislead badly in both
/// directions — 267 brushes name a `.gih` pipe's sequencing and 257 of them
/// need nothing else, while eighteen of the twenty naming mirrored dabs name
/// something else as well.
///
/// Printed on every run for the reason the classification table is: a count
/// nobody can re-derive goes stale, and the answer to "why is my favourite
/// brush not in here" is a number somebody has to be able to check.
fn report_refusals(refusals: &[Refusal]) {
    let mut rows: BTreeMap<&str, (Vec<usize>, usize)> = BTreeMap::new();
    for refusal in refusals {
        // `refusal.pack` is a `Pack::dir`, so this cannot miss — and filing an
        // unrecognised pack under the first one silently would be a column of
        // somebody else's brushes.
        let pack = PACKS
            .iter()
            .position(|p| p.dir == refusal.pack)
            .expect("a refusal names a pack this run walked");
        for reason in &refusal.reasons {
            let row = rows
                .entry(reason.as_str())
                .or_insert_with(|| (vec![0; PACKS.len()], 0));
            row.0[pack] += 1;
            if refusal.reasons.len() == 1 {
                row.1 += 1;
            }
        }
    }

    // Heaviest first, which is the order the table in `docs/brushes.md` is in.
    // The map is keyed alphabetically because it has to be keyed by something;
    // printing that order would leave the record and the run in two orders
    // nobody can check one against the other by eye. `sort_by_key` is stable,
    // so ties keep the map's order and the output does not move between runs.
    let mut ranked: Vec<_> = rows.iter().collect();
    ranked.sort_by_key(|(_, (per_pack, _))| std::cmp::Reverse(per_pack.iter().sum::<usize>()));

    println!("\nrefused, by reason and pack:");
    print!("  {:<54}", "");
    for pack in PACKS {
        print!(" {:>11}", pack.dir);
    }
    println!(" {:>6} {:>6}", "total", "alone");
    for (reason, (per_pack, alone)) in ranked {
        print!("  {reason:<54}");
        for count in per_pack {
            print!(" {count:>11}");
        }
        println!(" {:>6} {alone:>6}", per_pack.iter().sum::<usize>());
    }
    println!("  {} brushes refused in all", refusals.len());
}

/// The shipped masks, collected as the packs are read and written out at the
/// end.
///
/// It owns exactly the files in `assets/tips/` that a *pack* put there —
/// anything whose name starts with a pack's id prefix. Umber's own stamps are
/// `build-bitmaps.rs`'s and are left alone. Owning them means **deleting the
/// stale ones**: a brush that stops shipping, or a mask that changes name,
/// would otherwise leave a file behind that the table would go on embedding
/// for ever, and megabytes of binary nobody could account for is exactly the
/// kind of rot a generated directory invites.
struct BitmapLibrary {
    dir: PathBuf,
    /// Mask contents to the name allotted to it. Deduplication is by the whole
    /// mask, which is the only definition that makes two brushes "cut from the
    /// same stamp" the same thing.
    by_content: BTreeMap<Vec<u8>, String>,
    /// Name to the PNG that will be written under it. A `BTreeMap` so the
    /// writing order is the sorted order, whatever order the packs were read
    /// in — this directory is committed.
    files: BTreeMap<String, Vec<u8>>,
}

/// What [`BitmapLibrary::finish`] wrote, for the run's own report.
struct BitmapsWritten {
    count: usize,
    bytes: usize,
}

impl BitmapLibrary {
    fn open(dir: &Path) -> Self {
        fs::create_dir_all(dir).expect("create the tips directory");
        Self {
            dir: dir.to_path_buf(),
            by_content: BTreeMap::new(),
            files: BTreeMap::new(),
        }
    }

    /// Record `mask` and answer the name a preset should carry.
    ///
    /// The name is derived from the **first** preset to use the mask, and the
    /// packs are walked in a fixed order over sorted files, so it is stable
    /// across runs — which a committed directory needs as much as a committed
    /// file does.
    fn store(&mut self, prefix: &str, preset_id: &str, mask: TipMask) -> String {
        let png = mask.to_png().expect("encode a mask");
        if let Some(name) = self.by_content.get(&png) {
            return name.clone();
        }

        // A preset id is `<prefix>/<slug>` and a file name cannot hold the
        // slash. The prefix is kept rather than dropped: it is what tells this
        // library which files are its to delete, and it keeps two packs' brushes
        // of the same name apart.
        let base = format!("{prefix}-{}", preset_id.rsplit('/').next().unwrap_or(""));
        let mut name = base.clone();
        let mut n = 1;
        while self.files.contains_key(&name) {
            n += 1;
            name = format!("{base}-{n}");
        }
        self.files.insert(name.clone(), png.clone());
        self.by_content.insert(png, name.clone());
        name
    }

    /// Delete the masks a previous run left, write this run's, and report.
    fn finish(self) -> BitmapsWritten {
        let owned = |name: &str| PACKS.iter().any(|p| name.starts_with(p.id_prefix));
        for entry in fs::read_dir(&self.dir)
            .expect("read the tips directory")
            .flatten()
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".png") else {
                continue;
            };
            if owned(stem) && !self.files.contains_key(stem) {
                fs::remove_file(entry.path()).expect("remove a stale mask");
            }
        }

        let mut bytes = 0;
        for (name, png) in &self.files {
            bytes += png.len();
            let path = self.dir.join(format!("{name}.png"));
            // Only when it differs, so a regeneration that changes nothing does
            // not restamp the mtime of every file in a committed directory.
            if fs::read(&path).ok().as_ref() != Some(png) {
                fs::write(&path, png).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
            }
        }
        BitmapsWritten {
            count: self.files.len(),
            bytes,
        }
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
