//! What capping the shipped library's spacing costs, and what it changes.
//!
//! ```sh
//! git show HEAD:crates/umber-core/assets/builtin-brushes.ron > /tmp/before.ron
//! cargo run -p umber-core --release --example measure-spacing -- \
//!     /tmp/before.ron crates/umber-core/assets/builtin-brushes.ron
//! ```
//!
//! Two libraries in, paired by id, exactly as `diff-brush-library` takes them —
//! and for the same reason. `build-brush-library.rs` caps a converted preset's
//! spacing at [`preset::SHIPPED_SPACING_CAP`], so once that has run the
//! author's own figure is not in the shipped file any more and the cost of the
//! cap cannot be re-derived from it. The before-file is where that number
//! still lives.
//!
//! Two questions, and neither should be answered from an armchair:
//!
//! - **Does the cap change what the brush paints, or only how smoothly?**
//!   Umber's dab pass blends with a `max`, so a stroke's ink is the *union* of
//!   its dabs' footprints and not their sum — that is the whole wet-layer
//!   scheme. Dabs that overlap along the line therefore cover the same ground
//!   however many of them there are, and inserting more only fills in the
//!   scallops between them. Dabs that vary per dab — thrown off the line by
//!   `Brush::scatter`, rolled by an angle modulation, resized by a random one —
//!   are where that stops being true, and this says by how much.
//! - **What does it cost?** The dab pass is one draw call for N dabs, so what
//!   matters is how many more instances a stroke hands it and how many more
//!   fragments those cover, per **frame** rather than per stroke.
//!
//! # Ink is the union of the supports, not a re-rasterisation
//!
//! The area is sampled with `length(local) <= 1` — the dab's own support, the
//! predicate `Brush::reach_at` is documented against and the one `dab.wgsl`
//! tests to decide whether a fragment is inside the dab at all. It is
//! deliberately **not** the falloff: reproducing that here would be a third
//! copy of the dab shader's arithmetic, after the one `widgets::preview_dabs`
//! is licensed to keep, and it would answer a softer question than the one
//! being asked. What "does this brush lay down more paint" means under a
//! saturating `max` is how much ground the dabs cover between them.
//!
//! Sampled on a grid rather than integrated, because the dabs overlap and
//! scatter and there is no closed form for the union of a few hundred
//! ellipses. Two things it cannot see, and both make it an **upper** bound on
//! the change rather than a soft one: a bitmap tip's own mask is not applied,
//! so a stamp brush is measured as the whole ellipse its stamp is stretched
//! over; and the scatter RNG is a single stream, so two strokes with different
//! dab counts draw different random numbers and a scattering brush's figure
//! carries a few per cent of sampling noise in either direction.

use glam::Vec2;
use umber_core::brush::Brush;
use umber_core::input::InputPoint;
use umber_core::preset::{self, BrushPreset};
use umber_core::stroke::{Dab, StrokeBuilder};

/// How long a stroke to draw, in multiples of the brush's own diameter.
///
/// Long enough that the ends are a small share of the mark and short enough
/// that the grid below stays cheap on a 1045 px brush, which is the largest
/// thing in the library.
const STROKE_DIAMETERS: f32 = 12.0;

/// A plausible hand: document pixels per second, sampled at this many hertz.
///
/// It has to be *some* speed, because a good few shipped presets deposit paint
/// on a timer (`dabs_per_second`) and would otherwise contribute nothing or
/// everything. 500 px/s is an unhurried arm movement and 120 Hz is what a pen
/// reports; both are stated here rather than buried, so a reader can see what
/// the dab counts are counts *of*.
const HAND_SPEED: f32 = 500.0;
const SAMPLE_HZ: f64 = 120.0;

/// The frame the per-frame figures are per.
const FRAME_HZ: f32 = 60.0;

/// Grid spacing for the area sample, as a fraction of the brush's radius.
///
/// A twelfth of the radius puts about 24 samples across a dab, which settles
/// the union area to well inside the one per cent the conclusions here turn on.
const SAMPLE_STEP: f32 = 1.0 / 12.0;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [before, after] = &args[..] else {
        eprintln!("usage: measure-spacing <before.ron> <after.ron>");
        std::process::exit(2);
    };
    let old = load(before);
    let new = load(after);

    let mut rows: Vec<Row> = new
        .iter()
        .filter_map(|preset| {
            let was = old.iter().find(|p| p.id == preset.id)?;
            (was.brush.spacing != preset.brush.spacing).then(|| measure(&was.brush, preset))
        })
        .collect();
    rows.sort_by(|a, b| b.was.total_cmp(&a.was));

    println!(
        "{} presets, {} of them respaced; stroke {STROKE_DIAMETERS}x diameter \
         at {HAND_SPEED} px/s, {SAMPLE_HZ} Hz\n",
        new.len(),
        rows.len()
    );
    if rows.is_empty() {
        println!("nothing was respaced between these two libraries");
        return;
    }

    println!(
        "{:<46} {:>6} {:>6} {:>6} {:>7} {:>7} {:>6} {:>7} {:>10}",
        "preset", "was", "now", "scat", "dabs", "now", "x", "ink x", "fill/frame"
    );
    for row in &rows {
        println!(
            "{:<46} {:>6.3} {:>6.3} {:>6.2} {:>7} {:>7} {:>6.2} {:>7.3} {:>10.0}",
            row.id,
            row.was,
            row.now,
            row.scatter,
            row.dabs,
            row.now_dabs,
            row.dab_ratio,
            row.ink_ratio,
            row.fill_per_frame
        );
    }

    let by = |f: fn(&Row) -> f32| {
        rows.iter()
            .max_by(|a, b| f(a).total_cmp(&f(b)))
            .expect("at least one respaced preset")
    };
    println!("\n{} presets respaced", rows.len());
    let worst = by(|r| r.dab_ratio);
    println!(
        "worst dab multiplier: {:.2}x on {} ({} -> {} dabs over the stroke)",
        worst.dab_ratio, worst.id, worst.dabs, worst.now_dabs
    );
    let heaviest = by(|r| r.now_dabs as f32);
    println!(
        "most dabs in a stroke: {} on {}, which is {:.0} a frame at {FRAME_HZ} Hz",
        heaviest.now_dabs, heaviest.id, heaviest.dabs_per_frame
    );
    let fill = by(|r| r.fill_per_frame);
    println!(
        "most fragments shaded a frame: {:.0} on {}, {:.2}x a 1920x1080 window",
        fill.fill_per_frame,
        fill.id,
        fill.fill_per_frame / (1920.0 * 1080.0)
    );
    let inky = by(|r| r.ink_ratio);
    println!(
        "worst ink multiplier: {:.3}x on {}",
        inky.ink_ratio, inky.id
    );
    let mean = rows.iter().map(|r| r.ink_ratio).sum::<f32>() / rows.len() as f32;
    println!("mean ink multiplier: {mean:.3}x");

    // The split a scatter-based exemption would have been drawn on, printed so
    // that `build-brush-library.rs`'s refusal of one can be re-argued from
    // numbers rather than from memory. It does not separate the library: the
    // distributions overlap, and the worst case in the left-hand group has no
    // scatter at all.
    for (label, thrown) in [
        ("dabs on the line (scatter < 1)", false),
        ("dabs thrown off it (scatter >= 1)", true),
    ] {
        let group: Vec<&Row> = rows
            .iter()
            .filter(|r| (r.scatter >= 1.0) == thrown)
            .collect();
        let Some(worst) = group
            .iter()
            .max_by(|a, b| a.ink_ratio.total_cmp(&b.ink_ratio))
        else {
            continue;
        };
        let mean = group.iter().map(|r| r.ink_ratio).sum::<f32>() / group.len() as f32;
        println!(
            "{label}: {} presets, mean ink {mean:.3}x, worst {:.3}x ({})",
            group.len(),
            worst.ink_ratio,
            worst.id
        );
    }
}

fn load(path: &str) -> Vec<BrushPreset> {
    let text = std::fs::read_to_string(path).expect("read a library");
    preset::parse_library(&text).expect("parse a library")
}

struct Row {
    id: String,
    was: f32,
    now: f32,
    scatter: f32,
    dabs: usize,
    now_dabs: usize,
    dab_ratio: f32,
    ink_ratio: f32,
    dabs_per_frame: f32,
    fill_per_frame: f32,
}

fn measure(was: &Brush, now: &BrushPreset) -> Row {
    let before = lay_a_stroke(was);
    let after = lay_a_stroke(&now.brush);
    let ink_before = ink(&before, was);
    let ink_after = ink(&after, &now.brush);
    // The stroke is drawn at a constant speed, so a frame's share of it is a
    // frame's share of the dabs. That is the figure a frame budget cares about;
    // the per-stroke count is only what it is a share of.
    let frames = (now.brush.size * STROKE_DIAMETERS / HAND_SPEED * FRAME_HZ).max(1.0);
    let dabs_per_frame = after.len() as f32 / frames;
    Row {
        id: now.id.clone(),
        was: was.spacing,
        now: now.brush.spacing,
        scatter: now.brush.scatter,
        dabs: before.len(),
        now_dabs: after.len(),
        dab_ratio: after.len() as f32 / before.len().max(1) as f32,
        ink_ratio: ink_after / ink_before.max(1e-6),
        dabs_per_frame,
        fill_per_frame: dabs_per_frame * mean_quad_area(&after),
    }
}

/// One straight stroke at full pressure, and every dab it emitted.
///
/// Straight because the question is about spacing along a line: a curve would
/// bring the heading into it, which `Brush::step_at` already has its own tests
/// for. Full pressure because that is where `Brush::size` is stated.
fn lay_a_stroke(brush: &Brush) -> Vec<Dab> {
    let length = brush.size * STROKE_DIAMETERS;
    let mut builder = StrokeBuilder::new();
    let mut dabs = Vec::new();
    let start = Vec2::new(0.0, 0.0);
    builder.begin(*brush, [1.0, 1.0, 1.0], InputPoint::new(start, 1.0, 0.0));
    dabs.extend(builder.drain_pending());

    let steps = (length / (HAND_SPEED / SAMPLE_HZ as f32)).ceil().max(1.0) as usize;
    let seconds = f64::from(length / HAND_SPEED);
    for i in 1..=steps {
        let f = i as f32 / steps as f32;
        let point = InputPoint::new(
            start + Vec2::new(length * f, 0.0),
            1.0,
            f64::from(f) * seconds,
        );
        builder.extend(point);
        dabs.extend(builder.drain_pending());
    }
    builder.end();
    dabs.extend(builder.drain_pending());
    dabs
}

/// Fragments one dab's quad covers, averaged over the stroke.
///
/// The quad and not the ellipse inside it: the dab pass rasterises a quad and
/// discards nothing, so every fragment in it is shaded whether or not the
/// falloff leaves it at zero. The vertex shader gives the quad the dab's own
/// proportions, so it is `2r` by `2r / aspect`.
fn mean_quad_area(dabs: &[Dab]) -> f32 {
    if dabs.is_empty() {
        return 0.0;
    }
    let total: f32 = dabs
        .iter()
        .map(|d| 4.0 * d.radius * d.radius / d.aspect.max(1.0))
        .sum();
    total / dabs.len() as f32
}

/// Ground covered by the union of the dabs' supports, in document pixels.
fn ink(dabs: &[Dab], brush: &Brush) -> f32 {
    if dabs.is_empty() {
        return 0.0;
    }
    let step = (brush.size * 0.5 * SAMPLE_STEP).max(0.05);
    let mut min = Vec2::new(f32::MAX, f32::MAX);
    let mut max = Vec2::new(f32::MIN, f32::MIN);
    for dab in dabs {
        let pos = Vec2::new(dab.pos[0], dab.pos[1]);
        let reach = Vec2::splat(dab.radius);
        min = min.min(pos - reach);
        max = max.max(pos + reach);
    }

    let cols = ((max.x - min.x) / step).ceil() as usize + 1;
    let rows = ((max.y - min.y) / step).ceil() as usize + 1;
    let mut covered = 0usize;
    for row in 0..rows {
        for col in 0..cols {
            let p = min + Vec2::new(col as f32 * step, row as f32 * step);
            if dabs.iter().any(|d| inside(d, p)) {
                covered += 1;
            }
        }
    }
    covered as f32 * step * step
}

/// `length(local) <= 1` — the dab pass's own test for "inside the dab".
///
/// The quad is built rotated and squashed in the vertex shader, so `local` is
/// the point in the dab's own frame with the long semi-axis at 1.0 and the
/// short one at `1 / aspect`. Read off the *dab* rather than off the brush,
/// because a `DabTarget::Ratio` modulation moves it per dab — the same reason
/// `StrokeBuilder::bounds` reads `dab.aspect`.
fn inside(dab: &Dab, p: Vec2) -> bool {
    let d = p - Vec2::new(dab.pos[0], dab.pos[1]);
    let (sin, cos) = dab.angle.sin_cos();
    let long = (d.x * cos + d.y * sin) / dab.radius.max(1e-4);
    let short = (-d.x * sin + d.y * cos) / (dab.radius / dab.aspect.max(1.0)).max(1e-4);
    long * long + short * short <= 1.0
}
