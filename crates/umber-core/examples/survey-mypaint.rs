//! Survey the MyPaint pack: which (setting, input) pairs actually do anything.
//!
//! ```sh
//! cargo run -p umber-core --example survey-mypaint
//! ```
//!
//! MyPaint's editor writes a two-point mapping for **every** input a brush has
//! ever been shown, and most of those are flat, so "has control points" is not
//! the same question as "is driven by this input". This measures the *span* of
//! each mapping's output, which is the only figure that means anything.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Deserialize)]
struct MybFile {
    #[allow(dead_code)]
    version: u32,
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

/// MyPaint's own defaults, from `brushsettings.json`. A setting left at its
/// default contributes nothing, so counting "sets a base value" needs these.
fn default_base(name: &str) -> f32 {
    match name {
        "opaque" | "opaque_multiply" | "opaque_linearize" | "hardness" | "anti_aliasing" => {
            match name {
                "opaque" => 1.0,
                "hardness" => 0.8,
                "opaque_linearize" => 0.9,
                "anti_aliasing" => 1.0,
                _ => 1.0,
            }
        }
        "radius_logarithmic" => 2.0,
        "dabs_per_basic_radius" => 0.0,
        "dabs_per_actual_radius" => 2.0,
        "dabs_per_second" => 0.0,
        "elliptical_dab_ratio" => 1.0,
        "restore_color" => 0.0,
        "smudge_length" => 0.5,
        "smudge_bucket" => 0.0,
        "stroke_duration_logarithmic" => 4.0,
        "stroke_holdtime" => 0.0,
        "custom_input_slowness" => 0.0,
        "posterize_num" => 1.0,
        "paint_mode" => 0.0,
        _ => 0.0,
    }
}

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/brushes/mypaint/brushes")
        .canonicalize()
        .expect("the MyPaint pack must be fetched first (tools/fetch-brushes.ps1)");

    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();
    println!("{} brushes under {}\n", files.len(), root.display());

    // (setting, input) -> spans, one per brush that has a non-flat mapping.
    let mut mapped: BTreeMap<(String, String), Vec<f32>> = BTreeMap::new();
    // setting -> how many brushes state a non-default base value.
    let mut based: BTreeMap<String, Vec<f32>> = BTreeMap::new();
    // setting -> how many brushes name it at all.
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
                let sp = span(points);
                if sp > 1e-6 {
                    mapped
                        .entry((setting.clone(), input.clone()))
                        .or_default()
                        .push(sp);
                }
            }
        }
    }

    println!("== live (setting, input) mappings, by brush count ==");
    println!(
        "{:<32} {:<18} {:>6}  {:>8} {:>8} {:>8}",
        "setting", "input", "count", "min", "median", "max"
    );
    let mut rows: Vec<_> = mapped.iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    for ((setting, input), spans) in rows {
        let mut v = spans.clone();
        v.sort_by(f32::total_cmp);
        println!(
            "{:<32} {:<18} {:>6}  {:>8.3} {:>8.3} {:>8.3}",
            setting,
            input,
            v.len(),
            v[0],
            v[v.len() / 2],
            v[v.len() - 1]
        );
    }

    println!("\n== settings with a non-default base value ==");
    println!(
        "{:<32} {:>6} {:>6}  {:>9} {:>9}",
        "setting", "named", "set", "min", "max"
    );
    let mut rows: Vec<_> = based.iter().collect();
    rows.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    for (setting, values) in rows {
        let mut v = values.clone();
        v.sort_by(f32::total_cmp);
        println!(
            "{:<32} {:>6} {:>6}  {:>9.3} {:>9.3}",
            setting,
            named.get(setting).copied().unwrap_or(0),
            v.len(),
            v[0],
            v[v.len() - 1]
        );
    }

    println!("\n== every setting the pack names, and whether anything uses it ==");
    for (setting, count) in &named {
        let live_maps: usize = mapped
            .iter()
            .filter(|((s, _), _)| s == setting)
            .map(|(_, v)| v.len())
            .sum();
        let live_base = based.get(setting).map_or(0, Vec::len);
        println!("{setting:<32} named {count:>4}  base {live_base:>4}  mappings {live_maps:>4}");
    }

    println!("\n== inputs, totalled across every setting ==");
    let mut per_input: BTreeMap<String, usize> = BTreeMap::new();
    for ((_, input), spans) in &mapped {
        *per_input.entry(input.clone()).or_default() += spans.len();
    }
    let mut rows: Vec<_> = per_input.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    for (input, count) in rows {
        println!("{input:<20} {count:>5}");
    }
}

fn collect(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
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
