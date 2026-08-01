//! What a saved undo history actually costs, in bytes and in seconds.
//!
//! The numbers quoted in [`umber_core::docformat::history`] — the compression
//! ratios, the choice of PNG over the ZIP's own Deflate, and the size of
//! `BUDGET_BYTES` — came from here, and it is checked in so they can be
//! re-measured rather than trusted. It is a measuring tool, not a test: it
//! prints and asserts nothing.
//!
//! ```sh
//! cargo run --release -p umber-core --example measure-history -- 120 0 1.0
//! #                                                              ^   ^  ^ stroke scale
//! #                                                              |   ` grain, 0..1
//! #                                                              ` strokes
//! ```
//!
//! It paints a synthetic session on a 2048² canvas, capturing exactly what
//! `History` would hold — the pre-stroke bytes of each stroke's damaged rect —
//! and then writes the whole document twice, with the history and without, so
//! the last two lines are the numbers a user would actually notice: how much
//! bigger the file is, and how much longer the save took.

use std::io::Write as _;
use std::time::Instant;

use glam::UVec2;
use umber_core::docformat::{self, SaveDocument, SaveHistory, SaveLayer};
use umber_core::document::Background;
use umber_core::geom::PixelRect;
use umber_core::history::{Edit, EditKind, History, PixelPatch};
use umber_core::layer::LayerStack;

const W: usize = 2048;
const H: usize = 2048;

/// xorshift64. Not for correctness, only so a run is repeatable.
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

/// One stamp of premultiplied sRGB colour, composited "over" the layer.
struct Dab {
    at: (f32, f32),
    radius: f32,
    colour: [f32; 3],
    alpha: f32,
    grain: f32,
}

fn stamp(layer: &mut [u8], d: &Dab, rng: &mut Rng) {
    let (cx, cy) = d.at;
    let r = d.radius;
    let x0 = (cx - r).floor().max(0.0) as usize;
    let x1 = (cx + r).ceil().min(W as f32 - 1.0) as usize;
    let y0 = (cy - r).floor().max(0.0) as usize;
    let y1 = (cy + r).ceil().min(H as f32 - 1.0) as usize;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let dist = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() / r;
            if dist > 1.0 {
                continue;
            }
            let mut cov = (1.0 - dist * dist).powf(1.5) * d.alpha;
            if d.grain > 0.0 {
                cov *= 1.0 - d.grain * rng.f();
            }
            let i = (y * W + x) * 4;
            let da = layer[i + 3] as f32 / 255.0;
            for c in 0..3 {
                let dc = layer[i + c] as f32 / 255.0;
                layer[i + c] =
                    ((d.colour[c] * cov + dc * (1.0 - cov)) * 255.0).clamp(0.0, 255.0) as u8;
            }
            layer[i + 3] = ((cov + da * (1.0 - cov)) * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
}

/// Exactly what `finish_stroke` records: the layer as it was before the stroke,
/// over the rectangle the stroke damaged.
fn capture(layer: &[u8], slot: u32, rect: PixelRect) -> PixelPatch {
    let mut bytes = Vec::with_capacity(rect.area() as usize * 4);
    for y in rect.y..rect.y + rect.height {
        let start = (y as usize * W + rect.x as usize) * 4;
        bytes.extend_from_slice(&layer[start..start + rect.width as usize * 4]);
    }
    PixelPatch::new(rect, slot, bytes)
}

/// What the ZIP's own compressor would make of a patch — the alternative to
/// PNG, and the one worth measuring because it needs no extra code at all.
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

fn png_len(rect: PixelRect, bytes: &[u8], level: png::Compression) -> usize {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, rect.width, rect.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(level);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(bytes))
        .unwrap();
    out.len()
}

fn main() {
    let arg = |n: usize, or: f32| {
        std::env::args()
            .nth(n)
            .and_then(|a| a.parse().ok())
            .unwrap_or(or)
    };
    let strokes = arg(1, 120.0) as usize;
    let grain = arg(2, 0.0);
    let scale = arg(3, 1.0);

    let mut layer = vec![0u8; W * H * 4];
    let mut rng = Rng(0x1234_5678);
    let mut history = History::default();

    for _ in 0..strokes {
        let mut x = rng.f() * W as f32;
        let mut y = rng.f() * H as f32;
        let r = (6.0 + rng.f() * 40.0) * scale;
        let colour = [rng.f(), rng.f(), rng.f()];
        let alpha = 0.3 + rng.f() * 0.7;
        let steps = ((60.0 + rng.f() * 400.0) * scale) as usize;
        let (mut dx, mut dy) = (rng.f() * 2.0 - 1.0, rng.f() * 2.0 - 1.0);

        // A wandering path, and the box it damages.
        let (mut lo, mut hi) = ((W as f32, H as f32), (0.0f32, 0.0f32));
        let mut path = Vec::with_capacity(steps);
        for _ in 0..steps {
            dx += rng.f() * 0.4 - 0.2;
            dy += rng.f() * 0.4 - 0.2;
            let len = (dx * dx + dy * dy).sqrt().max(0.001);
            x = (x + dx / len * (r * 0.25).max(1.0)).clamp(0.0, W as f32 - 1.0);
            y = (y + dy / len * (r * 0.25).max(1.0)).clamp(0.0, H as f32 - 1.0);
            lo = (lo.0.min(x - r), lo.1.min(y - r));
            hi = (hi.0.max(x + r), hi.1.max(y + r));
            path.push((x, y));
        }

        let rect = PixelRect {
            x: lo.0.max(0.0) as u32,
            y: lo.1.max(0.0) as u32,
            width: (hi.0.ceil() as usize).min(W) as u32 - lo.0.max(0.0) as u32,
            height: (hi.1.ceil() as usize).min(H) as u32 - lo.1.max(0.0) as u32,
        };
        history.record(Edit::new(EditKind::Paint, capture(&layer, 0, rect)));

        for at in path {
            stamp(
                &mut layer,
                &Dab {
                    at,
                    radius: r,
                    colour,
                    alpha,
                    grain,
                },
                &mut rng,
            );
        }
    }

    // --- the patches on their own -------------------------------------------

    let mb = |n: usize| n as f64 / (1024.0 * 1024.0);
    let mut raw = 0usize;
    let (mut deflate, mut fast, mut balanced) = (0usize, 0usize, 0usize);
    let (mut t_deflate, mut t_fast, mut t_balanced) = (0.0, 0.0, 0.0);

    for i in 0..history.len() {
        let patch = &history.entry_at(i).unwrap().patch;
        raw += patch.byte_len();

        let t = Instant::now();
        deflate += deflated_len(&patch.bytes);
        t_deflate += t.elapsed().as_secs_f64();
        let t = Instant::now();
        fast += png_len(patch.rect, &patch.bytes, png::Compression::Fast);
        t_fast += t.elapsed().as_secs_f64();
        let t = Instant::now();
        balanced += png_len(patch.rect, &patch.bytes, png::Compression::Balanced);
        t_balanced += t.elapsed().as_secs_f64();
    }

    println!("{strokes} strokes, grain {grain}, scale {scale}");
    println!("  raw          {:8.2} MB", mb(raw));
    let ratio = |n: usize| raw as f64 / n as f64;
    println!(
        "  deflate      {:8.2} MB  ({:.1}x)  {t_deflate:.2}s",
        mb(deflate),
        ratio(deflate)
    );
    println!(
        "  png fast     {:8.2} MB  ({:.1}x)  {t_fast:.2}s",
        mb(fast),
        ratio(fast)
    );
    println!(
        "  png balanced {:8.2} MB  ({:.1}x)  {t_balanced:.2}s",
        mb(balanced),
        ratio(balanced)
    );

    // --- and the whole document, which is what a user sees ------------------

    let mut stack = LayerStack::new();
    stack.get_mut(0).unwrap().name = "Paint".into();
    let size = UVec2::new(W as u32, H as u32);
    let layers = [SaveLayer {
        name: "Paint",
        visible: true,
        opacity: 1.0,
        blend: umber_core::layer::BlendMode::Normal,
        pixels: &layer,
    }];
    let mut document = SaveDocument {
        size,
        layers: &layers,
        active: 0,
        background: Background::WHITE,
        dpi: 72.0,
        merged: &layer,
        history: None,
    };

    let t = Instant::now();
    let (plain, _) = docformat::encode(&document).unwrap();
    let t_plain = t.elapsed().as_secs_f64();

    document.history = SaveHistory::new(&history, &stack);
    let t = Instant::now();
    let (with, _) = docformat::encode(&document).unwrap();
    let t_with = t.elapsed().as_secs_f64();

    println!("one-layer document, {} × {}", size.x, size.y);
    println!(
        "  without history {:8.2} MB  {t_plain:.2}s",
        mb(plain.len())
    );
    println!(
        "  with history    {:8.2} MB  {t_with:.2}s  (+{:.2} MB, +{:.2}s)",
        mb(with.len()),
        mb(with.len() - plain.len()),
        t_with - t_plain
    );
}
