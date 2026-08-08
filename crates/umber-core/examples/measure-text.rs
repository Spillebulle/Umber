//! What re-rasterising a block of text through the transform actually costs.
//!
//! `docs/text-tool.md` §4(c) settles that scaling placed text has to be a fresh
//! rasterisation rather than a bilinear resample, and then states a bound
//! without a number in it: "the cost is the *destination* area, so text dragged
//! to fill a 10000² canvas is 400 MB of upload a frame". A budget has to be
//! measured before it is written down, exactly as `measure-history.rs` and
//! `measure-pressure.rs` are the numbers behind theirs.
//!
//! ```sh
//! cargo run -p umber-core --example measure-text --release
//! ```
//!
//! **`--release` is not optional here and the debug figure is not a slower
//! version of the same reading.** `[profile.dev]` builds dependencies at
//! `opt-level = 3` and this crate's own code unoptimised, and the rasteriser's
//! inner loop is `ab_glyph_rasterizer`'s while the pen feeding it is ours — so a
//! debug run measures a mixture nothing ships. Run it in release and say so.
//!
//! # What is being measured, and what is not
//!
//! Three costs make up one frame of a drag, and only the first two are here:
//!
//! 1. **Shaping and layout.** `harfrust` over the string, once per line. It is
//!    independent of the scale, which is exactly why it is worth separating: if
//!    it dominated, the answer would be to cache the shaped run rather than to
//!    budget the area.
//! 2. **Rasterisation.** `ab_glyph_rasterizer` over the mapped outlines, plus
//!    the merge into the block's coverage buffer and the trim. This is the one
//!    that follows the destination area. **`BoundsPen`'s measuring walk is
//!    counted here and not with the shaping**, which is where it belongs by
//!    cost — it is O(glyphs) and the column it sits in is O(pixels), so it is
//!    invisible either way — but not by name. Said out loud because the second
//!    walk of every outline is the thing this branch *added*, and a reader
//!    checking whether it was worth the em of padding it replaced should know
//!    which column it is hiding in.
//! 3. **The upload**, `Queue::write_texture` of the destination rectangle into
//!    the float's source texture. Not measured — it needs a device, so it
//!    belongs in `umber-render/examples/measure-effects.rs`'s company rather
//!    than here. What this prints instead is the **byte volume**, which is the
//!    part that is arithmetic rather than hardware, and it is four bytes a pixel
//!    of the destination.
//!
//! The rows are wall clock over [`RUNS`] repetitions, and the **fastest** of them
//! is the figure to quote, with the median printed beside it so the gap says how
//! quiet the machine was. [`timings`] is where that choice is argued, and it is
//! a departure from `measure-history.rs`'s median made for a reason this example
//! has already been caught by once.
//!
//! `ms/Mpx` is printed per row rather than left to be worked out, because the
//! rate is **not** constant — it is worst for small blocks, where the per-glyph
//! setup has nothing to amortise over — and a single average multiplied by an
//! area is how a budget ends up flattering itself.

use std::time::Instant;
use umber_core::fonts::FontLibrary;
use umber_core::text::{self, TextBlock};
use umber_core::transform::Affine;

/// The same face the tests use, included here rather than reached for: the
/// crate's own `TEST_FONT` is `pub(crate)` and an example is another crate.
const FACE: &[u8] = include_bytes!("../../../assets/fonts/Archivo[wdth,wght].ttf");

/// How many times each case is set. The median is reported.
const RUNS: usize = 9;

/// The blocks worth timing, and why each is here.
///
/// Not a sweep of sizes: what varies down this list is the *shape* of the work,
/// because the question is whether the cost follows the area or the glyph count.
const CASES: &[(&str, &str, f32)] = &[
    // The ordinary case. A caption somebody actually types.
    ("a caption", "Umber", 72.0),
    // Twelve times the glyphs at the same size, so shaping is stressed and the
    // area is not.
    (
        "a paragraph",
        "The quick brown fox\njumps over the lazy dog\nand keeps going",
        72.0,
    ),
    // The same caption at the top of the rail. Nearly the same glyph count as
    // the first, fourteen times the area.
    ("a caption at 1000 px", "Umber", text::MAX_SIZE),
];

/// Scales a drag reaches. 1.0 is where the text was placed; the transform tool
/// clamps magnitude only, so the top of this is a hand that has dragged a corner
/// a long way and the bottom is one that has shrunk it.
const SCALES: &[f32] = &[0.25, 1.0, 2.0, 4.0, 8.0, 16.0];

fn main() {
    let mut lib = FontLibrary::default();
    lib.add_builtin("archivo", FACE);
    let face = lib
        .resolve("Archivo", "Regular")
        .expect("Archivo Regular parses");
    let data = face.load().expect("its bytes");

    println!("Re-rasterising placed text, medians of {RUNS} runs.");
    println!(
        "Build: {}.  Cap: {} megapixels.",
        if cfg!(debug_assertions) {
            "DEBUG — do not quote these figures"
        } else {
            "release"
        },
        text::MAX_PIXELS >> 20,
    );
    println!();

    for (what, string, size) in CASES {
        let block = TextBlock {
            text: (*string).to_string(),
            size: *size,
            ..Default::default()
        };
        println!("--- {what} ---");
        println!(
            "  {:>6}  {:>13}  {:>9}  {:>9}  {:>9}  {:>6}  {:>9}",
            "scale", "destination", "shape", "raster", "(median)", "ms/Mpx", "upload"
        );
        for &s in SCALES {
            let map = Affine {
                m: glam::Mat2::from_diagonal(glam::Vec2::splat(s)),
                t: glam::Vec2::ZERO,
            };
            // Shaping and layout alone, subtracted from the full call below.
            //
            // **The blank block is built once, outside the timed closure.** It
            // was inside, so a `clone` and a `chars().map().collect()` were
            // counted as shaping and then subtracted from the raster — small,
            // and wrong in the direction that makes the expensive column look
            // cheaper.
            //
            // Every line is still shaped, laid out and measured; what a line of
            // spaces does not do is *draw*. It returns `NoInk` before the
            // coverage buffer and the rasteriser are allocated, so this is not
            // "the same allocations without the pixels" — an earlier comment
            // here claimed that and it is not true. It is the shaper and the
            // layout, which is what the column is for: if shaping dominated,
            // the answer would be to cache the shaped run rather than to budget
            // the area at all.
            let mut blank = block.clone();
            blank.text = blank
                .text
                .chars()
                .map(|c| if c == '\n' { c } else { ' ' })
                .collect();
            let full = timings(RUNS, || {
                let _ = text::set_through(face, &data, &block, map);
            });
            let shaping = timings(RUNS, || {
                let _ = text::set_through(face, &data, &blank, map);
            });
            match text::set_through(face, &data, &block, map) {
                Ok(placed) => {
                    let px = u64::from(placed.setting.width) * u64::from(placed.setting.height);
                    let mpx = px as f64 / (1 << 20) as f64;
                    let raster = (full.best - shaping.best).max(0.0);
                    println!(
                        "  {s:>6.2}  {:>5}x{:<7} {:>7.3} ms {:>7.2} ms {:>7.2} ms {:>6.1} \
                         {:>6.1} MB",
                        placed.setting.width,
                        placed.setting.height,
                        shaping.best,
                        raster,
                        (full.median - shaping.median).max(0.0),
                        raster / mpx.max(1e-9),
                        px as f64 * 4.0 / (1 << 20) as f64,
                    );
                }
                Err(err) => {
                    // The cap is a real outcome and belongs in the table: it is
                    // what stops the budget below from ever being asked about a
                    // block the module would refuse anyway.
                    println!(
                        "  {s:>6.2}  {:>13}  {:>7.3} ms          -          -        -          -",
                        refusal(err),
                        shaping.best
                    );
                }
            }
        }
        println!();
    }

    budget_note();
}

/// The fastest and the middling of `runs` timings of `f`, in milliseconds.
struct Timing {
    best: f64,
    median: f64,
}

/// Time `f` `runs` times and report both.
///
/// **The `best` column is the one to quote, and that is a departure from
/// `measure-history.rs`'s median.** A median is the right answer to "one
/// scheduling hiccup moved the mean" — it resists an *outlier*. It does not
/// resist *sustained* load, because under contention every sample is slow and
/// the middle one is slow too. This is pure single-threaded CPU work with no
/// device and no I/O in it, so load can only ever add: the minimum is the
/// closest reading available to what the code costs, and the median is reported
/// beside it so the gap between them says how quiet the machine actually was.
///
/// This is not a stylistic preference. The first figures published from this
/// example were taken while another crate was building and were up to 1.6x their
/// own quiet value, which is the mistake `CLAUDE.md` already records against
/// `measure-clipboard.rs` — "the first figures written into these docs were
/// three times too slow because the machine was building six other things at the
/// time". Reporting the minimum is what stops the same reading being wrong the
/// next time somebody runs this on a busy laptop, rather than relying on them
/// noticing that they should not have.
///
/// **A wide gap is the signal to re-run, not to average.** If `best` and
/// `median` differ by more than a few tens of percent the machine was busy, and
/// the honest response is another run on a quiet one.
fn timings(runs: usize, mut f: impl FnMut()) -> Timing {
    let mut times: Vec<f64> = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    times.sort_by(f64::total_cmp);
    Timing {
        best: times[0],
        median: times[runs / 2],
    }
}

fn refusal(err: text::TextError) -> String {
    match err {
        text::TextError::TooLarge { width, height } => format!("refused {width}x{height}"),
        other => format!("{other:?}"),
    }
}

/// What the table implies for the budget, printed by the example rather than
/// only living in a doc comment — so a re-run says whether the figure still
/// holds instead of leaving somebody to compare two tables by eye.
fn budget_note() {
    println!("--- what this bounds ---");
    println!(
        "  A 60 Hz frame is 16.7 ms and a drag has to draw the rest of the\n  \
         interface inside it too. Rasterising is the whole of the cost above\n  \
         about a megapixel, and it is linear in the destination area."
    );
    println!(
        "  {} pixels is the cap `text::MAX_PIXELS` already imposes, which is\n  \
         {:.0} MB of upload a frame at four bytes a pixel.",
        text::MAX_PIXELS,
        text::MAX_PIXELS as f64 * 4.0 / (1 << 20) as f64,
    );
}
