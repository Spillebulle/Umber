//! What one undo step costs in memory, and how deep the history therefore goes.
//!
//! The numbers in [`umber_core::damage`] and in CLAUDE.md's Undo section came
//! from here. `examples/measure-history.rs` is the equivalent for the *saved*
//! history — bytes in a file, encoded with PNG — and this is the one for what
//! is held in RAM while painting, which is what bounds the depth of the stack.
//!
//! ```sh
//! cargo run --release -p umber-core --example measure-undo
//! ```
//!
//! It generates three shapes of stroke on two canvas sizes and, for each,
//! measures what the history would hold under:
//!
//! * **box** — the stroke's bounding rectangle, which is what Umber stored
//!   before tiles;
//! * **tiles** — the cells of a [`TileMask`] the dabs actually reached, at
//!   several cell sizes;
//! * **flat pieces collapsed** — the same, with a piece whose every pixel is
//!   identical stored as that one pixel. Blank canvas and flat fills are the
//!   common case early in a painting and cost nothing to detect.
//!
//! It then prints the depth each gives against `History`'s 512 MB budget, and
//! what compressing the tiled bytes would add — which is measured here because
//! the cost of it lands on **pointer-up**, in front of the artist, and a
//! ratio is worthless without the seconds beside it.
//!
//! The canvas underneath is generated rather than painted stroke by stroke: a
//! smooth colour field with noise in it, which is what a patch of a busy
//! painting looks like to a compressor. The "fresh" column is a canvas nobody
//! has painted on yet, which is the other half of a real session.

use std::io::Write as _;
use std::time::Instant;

use glam::{UVec2, Vec2};
use umber_core::damage::TileMask;
use umber_core::geom::{PixelRect, Rect};

/// One dab of a synthetic stroke: where it landed and how far its quad reaches.
struct Step {
    centre: Vec2,
    half: Vec2,
}

/// xorshift64, so a run repeats.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn f(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1 << 24) as f32
    }
}

/// A thin line corner to corner: the case a bounding box describes worst.
fn diagonal(canvas: u32, rng: &mut Rng) -> Vec<Step> {
    let n = canvas as usize;
    let r = 12.0 + rng.f() * 12.0;
    let wobble = rng.f() * 200.0;
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32;
            let x = t * canvas as f32;
            let y = t * canvas as f32 + (t * 9.0).sin() * wobble;
            Step {
                centre: Vec2::new(x, y),
                half: Vec2::splat(r),
            }
        })
        .collect()
}

/// A broad serpentine wash over the whole canvas: the case tiles cannot help,
/// and therefore the one that has to come out no worse than it used to.
fn wash(canvas: u32, rng: &mut Rng) -> Vec<Step> {
    let r = 140.0 + rng.f() * 60.0;
    let rows = (canvas as f32 / r) as usize + 1;
    let mut steps = Vec::new();
    for row in 0..rows {
        let y = row as f32 * r + r * 0.5;
        let across = (canvas as f32 / (r * 0.25)) as usize;
        for i in 0..across {
            let t = i as f32 / across as f32;
            let x = if row % 2 == 0 { t } else { 1.0 - t } * canvas as f32;
            steps.push(Step {
                centre: Vec2::new(x, y),
                half: Vec2::splat(r),
            });
        }
    }
    steps
}

/// A small scribble in one corner of the canvas: what most strokes are, and
/// the case where a cell grid could easily make things worse.
fn scribble(canvas: u32, rng: &mut Rng) -> Vec<Step> {
    let r = 4.0 + rng.f() * 8.0;
    let (cx, cy) = (rng.f() * canvas as f32, rng.f() * canvas as f32);
    let (mut x, mut y) = (cx, cy);
    let (mut dx, mut dy) = (1.0f32, 0.3f32);
    (0..600)
        .map(|_| {
            dx += rng.f() * 0.6 - 0.3;
            dy += rng.f() * 0.6 - 0.3;
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            x = (x + dx / len * 3.0).clamp(0.0, canvas as f32 - 1.0);
            y = (y + dy / len * 3.0).clamp(0.0, canvas as f32 - 1.0);
            Step {
                centre: Vec2::new(x, y),
                half: Vec2::splat(r),
            }
        })
        .collect()
}

/// The rectangle the stroke damages, exactly as `StrokeBuilder::bounds` and
/// `to_pixels_clamped` would produce it.
fn bounds_of(steps: &[Step], canvas: u32) -> Option<PixelRect> {
    let mut bounds = Rect::empty();
    for s in steps {
        bounds.union_box(s.centre, s.half);
    }
    bounds.to_pixels_clamped(UVec2::splat(canvas))
}

/// A plausible painted canvas: broad smooth strokes of colour, opaque
/// everywhere, which is what a compressor sees in a patch of a busy painting.
///
/// Smooth on purpose. Per-pixel noise would be a canvas nothing compresses, and
/// the compression figures below would then say more about the fixture than
/// about painting — `measure-history.rs` stamps real dabs and gets 2.6–5× out
/// of PNG, so a fixture that got 1.3× would be measuring the wrong thing.
fn painted(canvas: u32) -> Vec<u8> {
    let n = canvas as usize;
    let mut out = vec![0u8; n * n * 4];
    let mut rng = Rng(0x51ED_270B);
    // A handful of wide soft blobs, laid over a gradient.
    let blobs: Vec<(f32, f32, f32, [f32; 3])> = (0..60)
        .map(|_| {
            (
                rng.f() * canvas as f32,
                rng.f() * canvas as f32,
                canvas as f32 * (0.05 + rng.f() * 0.25),
                [rng.f(), rng.f(), rng.f()],
            )
        })
        .collect();
    for y in 0..n {
        let fy = y as f32 / n as f32;
        for x in 0..n {
            let fx = x as f32 / n as f32;
            let mut c = [fx * 0.6 + 0.2, (fx + fy) * 0.3 + 0.1, fy * 0.7 + 0.1];
            for (bx, by, br, colour) in &blobs {
                let d = ((x as f32 - bx).powi(2) + (y as f32 - by).powi(2)).sqrt() / br;
                if d < 1.0 {
                    let w = (1.0 - d * d).powi(2);
                    for k in 0..3 {
                        c[k] += (colour[k] - c[k]) * w;
                    }
                }
            }
            let i = (y * n + x) * 4;
            for k in 0..3 {
                out[i + k] = (c[k].clamp(0.0, 1.0) * 255.0) as u8;
            }
            out[i + 3] = 255;
        }
    }
    out
}

/// Lift a rectangle out of a canvas — what `read_layer_rect` hands back.
fn lift(canvas: u32, pixels: &[u8], rect: PixelRect) -> Vec<u8> {
    let n = canvas as usize;
    let mut out = Vec::with_capacity(rect.area() as usize * 4);
    for y in rect.y..rect.y + rect.height {
        let start = (y as usize * n + rect.x as usize) * 4;
        out.extend_from_slice(&pixels[start..start + rect.width as usize * 4]);
    }
    out
}

/// Whether every pixel of a piece is the same one, which is what lets a piece
/// of blank canvas or flat fill be stored as four bytes.
fn uniform(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && bytes.chunks_exact(4).all(|p| p == &bytes[..4])
}

fn png_len(rect: PixelRect, bytes: &[u8]) -> usize {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, rect.width, rect.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(bytes))
        .unwrap();
    out.len()
}

fn deflated_len(bytes: &[u8]) -> usize {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file(
        "p",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .unwrap();
    zip.write_all(bytes).unwrap();
    zip.finish().unwrap().into_inner().len()
}

const BUDGET: f64 = 512.0 * 1024.0 * 1024.0;

fn mb(n: u64) -> f64 {
    n as f64 / (1024.0 * 1024.0)
}

/// Depth in strokes, as the History budget over one stroke's cost.
fn depth(bytes: u64) -> String {
    if bytes == 0 {
        return "     —".into();
    }
    let d = BUDGET / bytes as f64;
    if d >= 100_000.0 {
        format!("{:>6}", "100k+")
    } else {
        format!("{d:>6.0}")
    }
}

fn main() {
    let only: Option<u32> = std::env::args().nth(1).and_then(|a| a.parse().ok());

    for canvas in [2048u32, 10000] {
        if only.is_some_and(|c| c != canvas) {
            continue;
        }
        println!("\n=== {canvas} × {canvas} canvas ===\n");
        let pixels = painted(canvas);

        for (name, make) in [
            ("thin diagonal", diagonal as fn(u32, &mut Rng) -> Vec<Step>),
            ("broad wash", wash),
            ("small scribble", scribble),
        ] {
            let mut rng = Rng(0x1234_5678);
            let steps = make(canvas, &mut rng);
            let Some(rect) = bounds_of(&steps, canvas) else {
                continue;
            };
            let box_bytes = rect.area() * 4;

            println!(
                "{name} — box {} × {} = {:.1} MB, depth {}",
                rect.width,
                rect.height,
                mb(box_bytes),
                depth(box_bytes).trim()
            );

            for tile in [64u32, 128, 256] {
                let t = Instant::now();
                let mut mask = TileMask::new(tile);
                for s in &steps {
                    mask.mark(s.centre, s.half);
                }
                let pieces = mask.pieces(rect);
                let marking = t.elapsed().as_secs_f64();

                let kept: u64 = pieces.iter().map(|r| r.area() * 4).sum();

                // What collapsing flat pieces saves, on a painted canvas and on
                // one nobody has touched yet.
                let t = Instant::now();
                let mut painted_kept = 0u64;
                for p in &pieces {
                    let bytes = lift(canvas, &pixels, *p);
                    painted_kept += if uniform(&bytes) {
                        4
                    } else {
                        bytes.len() as u64
                    };
                }
                let scan = t.elapsed().as_secs_f64();
                let fresh_kept = pieces.len() as u64 * 4;

                println!(
                    "  tile {tile:>3}  {:>7} pieces  {:>8.1} MB ({:>4.1}× less)  depth {}  \
                     mark {:.1} ms, flat-scan {:.1} ms",
                    pieces.len(),
                    mb(kept),
                    box_bytes as f64 / kept.max(1) as f64,
                    depth(kept),
                    marking * 1000.0,
                    scan * 1000.0,
                );
                println!(
                    "            flat pieces collapsed: painted canvas {:>8.1} MB  depth {} \
                     | fresh canvas {:>8.1} MB  depth {}",
                    mb(painted_kept),
                    depth(painted_kept),
                    mb(fresh_kept),
                    depth(fresh_kept),
                );
            }

            // And what compressing the tiled bytes would cost, since the bill
            // lands on pointer-up. Measured on the 128-cell pieces, which is
            // what the engine keeps.
            let mut mask = TileMask::new(128);
            for s in &steps {
                mask.mark(s.centre, s.half);
            }
            let pieces = mask.pieces(rect);
            let (mut raw, mut png, mut deflate) = (0u64, 0u64, 0u64);
            let (mut t_png, mut t_deflate) = (0.0, 0.0);
            for p in &pieces {
                let bytes = lift(canvas, &pixels, *p);
                raw += bytes.len() as u64;
                let t = Instant::now();
                png += png_len(*p, &bytes) as u64;
                t_png += t.elapsed().as_secs_f64();
                let t = Instant::now();
                deflate += deflated_len(&bytes) as u64;
                t_deflate += t.elapsed().as_secs_f64();
            }
            println!(
                "  compressing the 128-cell pieces: png fast {:.1} MB ({:.1}×) in {:.2} s | \
                 deflate {:.1} MB ({:.1}×) in {:.2} s",
                mb(png),
                raw as f64 / png.max(1) as f64,
                t_png,
                mb(deflate),
                raw as f64 / deflate.max(1) as f64,
                t_deflate,
            );
            println!();
        }
    }
}
