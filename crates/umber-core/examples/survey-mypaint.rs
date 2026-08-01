//! Survey the MyPaint pack: which `(setting, input)` pairs actually do
//! anything, and what the importer now makes of them.
//!
//! ```sh
//! pwsh tools/fetch-brushes.ps1
//! cargo run -p umber-core --example survey-mypaint
//! ```
//!
//! This is where the conversion table in `docs/brushes.md` comes from, and it
//! is kept rather than thrown away for the same reason the library generator
//! prints its classification: a count that nobody can re-derive is a count that
//! quietly goes stale. Two figures in this project's own docs were wrong before
//! it existed.
//!
//! The measurement that matters is the **span** of a mapping's output, not
//! whether it has control points. MyPaint's editor writes a flat two-point
//! mapping for every input a brush has ever been shown, so a third of the
//! "mappings" in the pack are editor artefacts that contribute exactly zero.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use umber_core::brushimport::mypaint;
use umber_core::dynamics::{DabInput, DabTarget};

#[derive(Deserialize)]
struct MybFile {
    #[serde(default)]
    settings: BTreeMap<String, MybSetting>,
}

#[derive(Deserialize)]
struct MybSetting {
    #[serde(default)]
    base_value: f32,
    #[serde(default)]
    inputs: BTreeMap<String, Vec<(f32, f32)>>,
}

fn span(points: &[(f32, f32)]) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }
    let lo = points.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let hi = points.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    (hi - lo).max(0.0)
}

/// MyPaint's defaults, from `libmypaint/brushsettings.json`. Only the non-zero
/// ones; without them "sets a non-default base" counts every brush in the pack
/// for `speed1_gamma` and reads as alarming rather than as inert.
fn default_base(name: &str) -> f32 {
    match name {
        "opaque"
        | "anti_aliasing"
        | "gridmap_scale_x"
        | "gridmap_scale_y"
        | "paint_mode"
        | "offset_by_speed_slowness"
        | "elliptical_dab_ratio" => 1.0,
        "opaque_linearize" => 0.9,
        "radius_logarithmic" | "dabs_per_actual_radius" | "direction_filter" => 2.0,
        "hardness" => 0.8,
        "speed1_slowness" => 0.04,
        "speed2_slowness" => 0.8,
        "speed1_gamma" | "speed2_gamma" | "stroke_duration_logarithmic" => 4.0,
        "smudge_length" => 0.5,
        "elliptical_dab_angle" => 90.0,
        "posterize_num" => 0.05,
        _ => 0.0,
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/brushes/mypaint/brushes");
    let mut files = Vec::new();
    collect(&root, &mut files);
    if files.is_empty() {
        eprintln!(
            "no .myb files under {} — run tools/fetch-brushes.ps1 first",
            root.display()
        );
        std::process::exit(1);
    }
    files.sort();
    println!("{} brushes\n", files.len());

    let mut mapped: BTreeMap<(String, String), Vec<f32>> = BTreeMap::new();
    let mut based: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    let mut named: BTreeMap<String, usize> = BTreeMap::new();

    for path in &files {
        let text = std::fs::read_to_string(path).expect("read");
        let file: MybFile = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                continue;
            }
        };
        for (setting, s) in &file.settings {
            *named.entry(setting.clone()).or_default() += 1;
            if (s.base_value - default_base(setting)).abs() > 1e-6 {
                based.entry(setting.clone()).or_default().push(s.base_value);
            }
            for (input, points) in &s.inputs {
                if span(points) > 1e-6 {
                    mapped
                        .entry((setting.clone(), input.clone()))
                        .or_default()
                        .push(span(points));
                }
            }
        }
    }

    println!("== live (setting, input) mappings, by brush count ==");
    println!(
        "{:<30} {:<17} {:>5}  {:>8} {:>8} {:>8}  fate",
        "setting", "input", "count", "min", "median", "max"
    );
    let mut rows: Vec<_> = mapped.iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    for ((setting, input), spans) in rows {
        if spans.len() < 3 {
            continue;
        }
        let mut v = spans.clone();
        v.sort_by(f32::total_cmp);
        println!(
            "{:<30} {:<17} {:>5}  {:>8.3} {:>8.3} {:>8.3}  {}",
            setting,
            input,
            v.len(),
            v[0],
            v[v.len() / 2],
            v[v.len() - 1],
            fate(setting, input)
        );
    }

    println!("\n== settings with a non-default base value ==");
    let mut rows: Vec<_> = based.iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    for (setting, values) in rows {
        if values.len() < 3 {
            continue;
        }
        let mut v = values.clone();
        v.sort_by(f32::total_cmp);
        println!(
            "{:<30} {:>5} of {:>3}  {:>9.3} .. {:<9.3}  {}",
            setting,
            v.len(),
            named.get(setting).copied().unwrap_or(0),
            v[0],
            v[v.len() - 1],
            fate(setting, "")
        );
    }

    println!("\n== inputs the pack uses at all ==");
    let mut per_input: BTreeMap<&str, usize> = BTreeMap::new();
    for ((_, input), spans) in &mapped {
        *per_input.entry(input.as_str()).or_default() += spans.len();
    }
    let mut rows: Vec<_> = per_input.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1));
    for (input, count) in rows {
        let read = DabInput::ALL.iter().any(|i| i.myb_name() == input);
        println!(
            "{input:<18} {count:>5} live mappings   {}",
            if read { "read" } else { "held at neutral" }
        );
    }

    // --- what the importer actually produces --------------------------------
    println!("\n== after import ==");
    let mut slots = [0usize; 10];
    let mut per_target: BTreeMap<&str, usize> = BTreeMap::new();
    let mut per_pair: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    let mut coloured = 0;
    let mut invisible = Vec::new();
    let mut busiest = (0usize, PathBuf::new());

    for path in &files {
        let text = std::fs::read_to_string(path).expect("read");
        let brush = match mypaint::from_myb(&text) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: {e}", path.display());
                continue;
            }
        };
        let n = brush.modulations.len();
        slots[n.min(9)] += 1;
        if n > busiest.0 {
            busiest = (n, path.clone());
        }
        for m in brush.modulations.as_slice() {
            *per_target.entry(m.target.label()).or_default() += 1;
            *per_pair
                .entry((m.target.label(), m.input.label()))
                .or_default() += 1;
        }
        if brush.colours_dabs() {
            coloured += 1;
        }
        if brush.opacity <= 0.001 {
            invisible.push(path.clone());
        }
    }

    println!("modulations per brush:");
    for (n, count) in slots.iter().enumerate() {
        if *count > 0 {
            println!("  {n} → {count} brushes");
        }
    }
    println!(
        "busiest: {} with {} entries (the table holds {})",
        busiest.1.file_name().unwrap_or_default().to_string_lossy(),
        busiest.0,
        umber_core::Modulations::MAX
    );
    println!("\nby target:");
    let mut rows: Vec<_> = per_target.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for (target, count) in rows {
        println!("  {target:<14} {count:>4}");
    }
    println!("\nby (target, input), three or more:");
    let mut rows: Vec<_> = per_pair.iter().collect();
    rows.sort_by(|a, b| b.1.cmp(a.1));
    for ((target, input), count) in rows {
        if *count >= 3 {
            println!("  {target:<14} {input:<14} {count:>4}");
        }
    }
    println!(
        "\n{coloured} brushes need the per-dab colour path ({} take the fast one)",
        files.len() - coloured
    );
    if invisible.is_empty() {
        println!("no brush imports at zero opacity");
    } else {
        println!("STILL INVISIBLE: {invisible:?}");
    }

    // Targets nothing drives are worth knowing about: they mean either a
    // measurement that was wrong or a control in the editor that no shipped
    // brush demonstrates.
    let unused: Vec<_> = DabTarget::ALL
        .iter()
        .filter(|t| !per_target.contains_key(t.label()))
        .map(|t| t.label())
        .collect();
    if !unused.is_empty() {
        println!("targets no shipped brush drives: {unused:?}");
    }
}

/// What the importer does with a setting, so the table above can be read
/// without cross-referencing the module docs.
fn fate(setting: &str, input: &str) -> &'static str {
    if !input.is_empty() && !DabInput::ALL.iter().any(|i| i.myb_name() == input) {
        return match input {
            "brush_radius" | "viewzoom" => "folded into the base (a constant)",
            _ => "held at neutral",
        };
    }
    match setting {
        "radius_logarithmic"
        | "hardness"
        | "opaque"
        | "opaque_multiply"
        | "offset_by_random"
        | "elliptical_dab_ratio"
        | "elliptical_dab_angle"
        | "smudge"
        | "change_color_h"
        | "change_color_v"
        | "change_color_l"
        | "change_color_hsv_s"
        | "change_color_hsl_s" => "read",
        "dabs_per_actual_radius"
        | "dabs_per_basic_radius"
        | "dabs_per_second"
        | "eraser"
        | "slow_tracking"
        | "smudge_length"
        | "smudge_radius_log"
        | "radius_by_random"
        | "offset_by_speed"
        | "stroke_duration_logarithmic"
        | "stroke_holdtime" => "base read, mappings dropped",
        "opaque_linearize" | "anti_aliasing" => "correctly ignored",
        "color_h" | "color_s" | "color_v" | "restore_color" => "the saved palette colour",
        _ => "dropped",
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "myb") {
            out.push(path);
        }
    }
}
