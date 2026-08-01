//! Generate the bitmaps Umber ships: brush tips and paper grain.
//!
//! ```sh
//! cargo run -p umber-core --example build-bitmaps
//! ```
//!
//! Writes `crates/umber-core/assets/tips/*.png`,
//! `crates/umber-core/src/tip_table.rs`, `assets/patterns/*.png` and
//! `crates/umber-core/src/pattern_table.rs`, then prints what each one measures.
//!
//! # Why these are generated rather than photographed
//!
//! The rule at the top of `docs/brush-sources.md` is that a licence has to be
//! verifiable *from the files themselves*. Every CC0 paper-texture library
//! worth having — ambientCG, Poly Haven, Texture Ninja — states its licence on
//! a web page beside the download and not inside it, which is exactly the case
//! that rule exists for. Rather than ship something whose licence cannot be
//! checked, or vendor a photograph and hope, these are drawn here: the source
//! is this file, the licence is the project's, and both travel with the
//! repository.
//!
//! They are also *tileable by construction*, which a photograph is not. Grain
//! is anchored to the document and repeats across it, so a seam would be a grid
//! of lines across every stroke.
//!
//! # Why a generated table
//!
//! `include_bytes!` needs a literal path, so the set of shipped bitmaps has to
//! be source code. The table is written from the directory listing, so adding a
//! stamp is dropping a PNG in and re-running this — the same shape as
//! `build-brush-library.rs` and `builtin-brushes.ron`, and a generated file
//! whose diff can be read.
//!
//! `assets/tips/` is **shared**: this writes Umber's own stamps there and
//! `build-brush-library.rs` writes the brush packs' masks beside them. Both end
//! by rewriting `tip_table.rs` from the whole listing, through the same
//! [`table::write_table`], so either one can be run on its own and neither can
//! leave the table naming a file that is not there.

use std::path::Path;
use umber_core::tip::{TipMask, stroke_coverage};

#[path = "common/table.rs"]
mod table;
use table::{workspace_root, write_table};

/// Side of a paper tile, in texels. One tile covers `Brush::grain_scale`
/// document pixels, so 256 is roughly one texel per pixel at the default scale
/// — fine enough to read as tooth and small enough that three of them are 200 kB
/// of binary rather than two megabytes.
const TILE: u32 = 256;

fn main() {
    let root = workspace_root();
    let tips_dir = root.join("crates/umber-core/assets/tips");
    let patterns_dir = root.join("assets/patterns");
    std::fs::create_dir_all(&tips_dir).expect("create tips directory");
    std::fs::create_dir_all(&patterns_dir).expect("create patterns directory");

    // ---- tips ------------------------------------------------------------
    //
    // One stamp, and it is deliberately a sparse one: a dense silhouette would
    // paint the same under either coverage rule and would prove nothing. This
    // is what a build-up brush is for.
    let stipple = stipple();
    write_png(&tips_dir.join("umber-stipple.png"), &stipple);
    let measured = stroke_coverage(&stipple, 0.06);
    println!(
        "tips/umber-stipple.png  {}x{}  stroke: max {:.3}, building up {:.3}  -> {}",
        stipple.width(),
        stipple.height(),
        measured.under_max,
        measured.under_build_up,
        if measured.needs_build_up() {
            "needs build-up"
        } else {
            "max is enough"
        }
    );
    assert!(
        measured.is_usable(),
        "a shipped tip has to make a mark: {measured:?}"
    );

    write_table(
        &root.join("crates/umber-core/src/tip_table.rs"),
        "tip",
        "TIPS",
        "../assets/tips",
        &tips_dir,
    );

    // ---- paper grain -----------------------------------------------------
    for (name, tile) in [("tooth", tooth()), ("canvas", canvas()), ("grit", grit())] {
        let path = patterns_dir.join(format!("{name}.png"));
        write_png(&path, &tile);
        let mean = tile.coverage().iter().map(|&c| c as f32).sum::<f32>()
            / tile.coverage().len() as f32
            / 255.0;
        let min = tile.coverage().iter().copied().min().unwrap_or(0);
        println!(
            "patterns/{name}.png  {TILE}x{TILE}  mean {mean:.3}, darkest texel {}",
            min as f32 / 255.0
        );
    }

    write_table(
        &root.join("crates/umber-core/src/pattern_table.rs"),
        "pattern",
        "PATTERNS",
        "../../../assets/patterns",
        &patterns_dir,
    );
}

// ---------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------

/// A hash, not a generator: the same lattice point must give the same value
/// however it is reached, or the noise will not tile.
fn hash(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32).wrapping_mul(0x27d4_eb2d) ^ (y as u32).wrapping_mul(0x1656_67b1) ^ seed;
    h ^= h >> 15;
    h = h.wrapping_mul(0x2c1b_3c6d);
    h ^= h >> 12;
    h = h.wrapping_mul(0x2974_5c41);
    h ^= h >> 15;
    h as f32 / u32::MAX as f32
}

fn smooth(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

/// Value noise on a lattice of `period` cells across the tile.
///
/// The lattice wraps at `period`, which is what makes every one of these tiles
/// seamless — the right edge interpolates towards the same lattice points the
/// left edge came from. A photograph would need the usual mirror-and-blend
/// trick and would show it.
fn noise(x: f32, y: f32, period: i32, seed: u32) -> f32 {
    let fx = x * period as f32 / TILE as f32;
    let fy = y * period as f32 / TILE as f32;
    let (x0, y0) = (fx.floor() as i32, fy.floor() as i32);
    let (tx, ty) = (smooth(fx - x0 as f32), smooth(fy - y0 as f32));
    let wrap = |v: i32| v.rem_euclid(period);
    let (x1, y1) = (wrap(x0 + 1), wrap(y0 + 1));
    let (x0, y0) = (wrap(x0), wrap(y0));

    let a = hash(x0, y0, seed);
    let b = hash(x1, y0, seed);
    let c = hash(x0, y1, seed);
    let d = hash(x1, y1, seed);
    let top = a + (b - a) * tx;
    let bottom = c + (d - c) * tx;
    top + (bottom - top) * ty
}

/// Sum of octaves, each half the amplitude and twice the frequency.
fn fbm(x: f32, y: f32, base: i32, octaves: u32, seed: u32) -> f32 {
    let mut total = 0.0;
    let mut amplitude = 1.0;
    let mut sum = 0.0;
    for o in 0..octaves {
        let period = base << o;
        if period > TILE as i32 {
            break;
        }
        total += noise(x, y, period, seed.wrapping_add(o * 7919)) * amplitude;
        sum += amplitude;
        amplitude *= 0.5;
    }
    total / sum
}

fn tile_from(f: impl Fn(f32, f32) -> f32) -> TipMask {
    let mut pixels = Vec::with_capacity((TILE * TILE) as usize);
    for y in 0..TILE {
        for x in 0..TILE {
            let v = f(x as f32, y as f32).clamp(0.0, 1.0);
            pixels.push((v * 255.0).round() as u8);
        }
    }
    TipMask::new(TILE, TILE, pixels).expect("tile")
}

// ---------------------------------------------------------------------------
// The bitmaps themselves
// ---------------------------------------------------------------------------

/// Hot-pressed paper: a fine, even tooth. What a pencil catches on.
fn tooth() -> TipMask {
    tile_from(|x, y| {
        let n = fbm(x, y, 64, 3, 0x51ed_2701);
        // Biased high and compressed: paper resists paint a little everywhere
        // rather than a lot in places, so the mean sits near 0.8 and the
        // darkest pits still let some paint through.
        0.55 + n * 0.45
    })
}

/// Cotton canvas: a woven grid under a slow blotch.
fn canvas() -> TipMask {
    tile_from(|x, y| {
        // 32 threads across the tile, each way. `sin` of an exact multiple of
        // the tile width keeps the weave seamless.
        let tau = std::f32::consts::TAU;
        let warp = (x / TILE as f32 * tau * 32.0).sin();
        let weft = (y / TILE as f32 * tau * 32.0).sin();
        let weave = 0.5 + 0.5 * (warp * weft).abs();
        let slow = fbm(x, y, 8, 3, 0x2b7f_10c3);
        (0.45 + weave * 0.35 + slow * 0.25).min(1.0)
    })
}

/// Cold-pressed rough: coarse hollows a dry brush skips across entirely.
fn grit() -> TipMask {
    tile_from(|x, y| {
        let n = fbm(x, y, 16, 4, 0x7a3d_9f11);
        // Contrast pushed hard: the point of this one is that a light stroke
        // misses the hollows completely.
        let v = (n - 0.35) * 2.2 + 0.35;
        0.15 + v.clamp(0.0, 1.0) * 0.85
    })
}

/// Umber's own stamp: a sparse speckle, for chalk and dry media.
///
/// Deliberately faint — the brightest texel is around half — so it is a working
/// example of the case build-up exists for. Stamped with a `max` it would paint
/// a stroke half the strength this one does.
fn stipple() -> TipMask {
    const SIDE: u32 = 192;
    let mut pixels = vec![0u8; (SIDE * SIDE) as usize];

    // Speckles inside a soft circular envelope, so the stamp reads as a round
    // chalk end rather than as a square of noise.
    for i in 0..2400u32 {
        let cx = hash(i as i32, 1, 0xa137) * SIDE as f32;
        let cy = hash(i as i32, 2, 0xa137) * SIDE as f32;
        let r = 0.8 + hash(i as i32, 3, 0xa137) * 2.6;
        let peak = 0.18 + hash(i as i32, 4, 0xa137) * 0.38;

        let dx = cx / SIDE as f32 - 0.5;
        let dy = cy / SIDE as f32 - 0.5;
        let envelope = 1.0 - (dx * dx + dy * dy).sqrt() * 2.0;
        if envelope <= 0.0 {
            continue;
        }
        let peak = peak * envelope.min(1.0).powf(0.6);

        let x0 = (cx - r).floor().max(0.0) as u32;
        let x1 = ((cx + r).ceil() as u32).min(SIDE - 1);
        let y0 = (cy - r).floor().max(0.0) as u32;
        let y1 = ((cy + r).ceil() as u32).min(SIDE - 1);
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
                if d > r {
                    continue;
                }
                let v = peak * (1.0 - d / r);
                let px = &mut pixels[(y * SIDE + x) as usize];
                // `max`, not a sum: the speckles are the *mask*, and letting
                // them add would give the middle of the stamp a solid core.
                *px = (*px).max((v * 255.0).round() as u8);
            }
        }
    }

    TipMask::new(SIDE, SIDE, pixels).expect("stipple")
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

fn write_png(path: &Path, mask: &TipMask) {
    let bytes = mask.to_png().expect("encode");
    std::fs::write(path, bytes).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}
