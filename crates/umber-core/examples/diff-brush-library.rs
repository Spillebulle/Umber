//! Read the diff of a regenerated brush library.
//!
//! ```sh
//! git show HEAD:crates/umber-core/assets/builtin-brushes.ron > /tmp/before.ron
//! cargo run -p umber-core --example build-brush-library
//! cargo run -p umber-core --example diff-brush-library -- \
//!     /tmp/before.ron crates/umber-core/assets/builtin-brushes.ron
//! ```
//!
//! `builtin-brushes.ron` is committed rather than built, on the grounds that a
//! generated file in a commit is a file whose diff can be read. Fifteen
//! thousand lines of pretty-printed RON is not, in practice, readable: a change
//! to one field of one brush and a change to every field of every brush look
//! much the same in `git diff`. This turns the same information into the four
//! questions actually worth asking — did any preset appear or vanish, which
//! *fields* moved and in how many brushes, did anything change collection, and
//! is the resulting spread of values sane.
//!
//! It reports rather than asserts. A brush library is a judgement, and the
//! numbers here are the evidence for it, not a pass mark: MyPaint really does
//! ship a one-pixel liner and a five-hundred-pixel knife, so "the size range is
//! 1 to 700" is a fact to check against the sources and not a fault.

use std::collections::BTreeMap;

use umber_core::brush::Brush;
use umber_core::preset::{self, BrushPreset};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [before, after] = &args[..] else {
        eprintln!("usage: diff-brush-library <before.ron> <after.ron>");
        std::process::exit(2);
    };
    let old = load(before);
    let new = load(after);

    let old_by: BTreeMap<&str, &BrushPreset> = old.iter().map(|p| (p.id.as_str(), p)).collect();
    let new_by: BTreeMap<&str, &BrushPreset> = new.iter().map(|p| (p.id.as_str(), p)).collect();

    println!("{} presets before, {} after", old.len(), new.len());
    report(
        "appeared",
        new_by.keys().filter(|k| !old_by.contains_key(*k)),
    );
    report(
        "vanished",
        old_by.keys().filter(|k| !new_by.contains_key(*k)),
    );

    // Which fields moved, and in how many brushes. This is the question a RON
    // diff cannot answer and the one that says whether an import change did
    // what it claimed.
    let mut moved: BTreeMap<&str, usize> = BTreeMap::new();
    let mut changed = 0usize;
    let mut recategorised: Vec<String> = Vec::new();
    for (id, new) in &new_by {
        let Some(old) = old_by.get(id) else { continue };
        if old.category != new.category {
            recategorised.push(format!("  {id}: {} -> {}", old.category, new.category));
        }
        let fields = differences(&old.brush, &new.brush);
        if !fields.is_empty() {
            changed += 1;
        }
        for field in fields {
            *moved.entry(field).or_default() += 1;
        }
    }
    println!(
        "\n{changed} of {} presets present in both changed",
        new_by.len()
    );
    for (field, count) in &moved {
        println!("  {field:26} {count:4}");
    }
    println!("\nchanged collection: {}", recategorised.len());
    for line in &recategorised {
        println!("{line}");
    }

    println!("\nthe new library's spread:");
    spread("size", &new, |b| b.size);
    spread("opacity", &new, |b| b.opacity);
    spread("hardness", &new, |b| b.hardness);
    spread("spacing", &new, |b| b.spacing);
    spread("dab_ratio", &new, |b| b.dab_ratio);
    spread("scatter", &new, |b| b.scatter);
    spread("smudge", &new, |b| b.smudge);
    spread("stabilization", &new, |b| b.stabilization);

    // A preset that paints nothing is the one fault that needs no judgement at
    // all, and three shipped that way once — see `docs/brushes.md`.
    let invisible: Vec<&str> = new
        .iter()
        .filter(|p| p.brush.opacity <= 0.0)
        .map(|p| p.id.as_str())
        .collect();
    println!("\npresets that would paint nothing: {invisible:?}");
}

fn load(path: &str) -> Vec<BrushPreset> {
    let text = std::fs::read_to_string(path).expect("read a library");
    preset::parse_library(&text).expect("parse a library")
}

fn report<'a>(what: &str, ids: impl Iterator<Item = &'a &'a str>) {
    let ids: Vec<&str> = ids.copied().collect();
    println!("{what} {}: {ids:?}", ids.len());
}

/// Which fields differ, by name. Floats compare with a tolerance because the
/// library round-trips through decimal text.
fn differences(a: &Brush, b: &Brush) -> Vec<&'static str> {
    let mut out = Vec::new();
    macro_rules! number {
        ($($name:ident),* $(,)?) => {$(
            if (a.$name - b.$name).abs() > 1e-5 { out.push(stringify!($name)); }
        )*};
    }
    macro_rules! exact {
        ($($name:ident),* $(,)?) => {$(
            if a.$name != b.$name { out.push(stringify!($name)); }
        )*};
    }
    number!(
        size,
        min_size_ratio,
        hardness,
        opacity,
        flow,
        spacing,
        min_hardness_ratio,
        stabilization,
        smudge,
        smudge_length,
        smudge_radius,
        dabs_per_second,
        dab_ratio,
        dab_angle,
        dab_angle_jitter,
        scatter,
        min_scatter_ratio,
        radius_jitter,
        grain,
        grain_scale,
        speed_offset,
        stroke_span,
        stroke_hold,
    );
    exact!(
        pressure_size,
        pressure_opacity,
        pressure_hardness,
        pressure_scatter,
        dab_angle_follows_stroke,
        build_up,
        mode,
        grain_pattern,
        size_curve,
        opacity_curve,
        hardness_curve,
        scatter_curve,
        modulations,
    );
    out
}

fn spread(name: &str, presets: &[BrushPreset], of: impl Fn(&Brush) -> f32) {
    let mut values: Vec<f32> = presets.iter().map(|p| of(&p.brush)).collect();
    values.sort_by(f32::total_cmp);
    let at = |q: usize| values[q * (values.len() - 1) / 100];
    println!(
        "  {name:16} min {:8.3}  p10 {:8.3}  median {:8.3}  p90 {:8.3}  max {:8.3}",
        values[0],
        at(10),
        at(50),
        at(90),
        values[values.len() - 1]
    );
}
