//! Bitmap brush tips.
//!
//! A tip is an 8-bit coverage mask stamped in place of the procedural round
//! falloff. It **modulates coverage** and nothing else: the dab pass still
//! writes a single channel into the stroke scratch with a `max` blend, so the
//! wet-layer invariant in `CLAUDE.md` is untouched — a tipped stroke saturates
//! at 1.0 under overlap exactly as a round one does, and stroke opacity is
//! still applied once at commit.
//!
//! Plain bytes, no GPU types: `umber-render` uploads one of these to an
//! `R8Unorm` texture, and the engine stays testable without a device.
//!
//! On disk a tip is an 8-bit greyscale PNG in the brush library's `tips/`
//! directory — see [`crate::preset::UserLibrary`] for why the library is a
//! directory rather than the single RON file it used to be.

use crate::preset::PresetError;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

/// An 8-bit coverage mask, row-major from the top-left.
///
/// `0` is no paint and `255` is full paint. That is the same convention GIMP's
/// `.gbr` uses, which is why the importer needs no inversion — worth stating,
/// because half the brush formats in the world use the opposite one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipMask {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
}

impl TipMask {
    /// Refuse anything bigger than this in either axis.
    ///
    /// Device limits are `downlevel_defaults`, which guarantees only 2048, and
    /// a tip is stamped once per dab — a 4096² tip would be both unbindable on
    /// a mobile GPU and far larger than any brush needs.
    pub const MAX_SIZE: u32 = 2048;

    /// `coverage` must hold exactly `width * height` bytes.
    pub fn new(width: u32, height: u32, coverage: Vec<u8>) -> Result<Self, PresetError> {
        if width == 0 || height == 0 {
            return Err(PresetError::Malformed(
                None,
                "a brush tip cannot be empty".to_string(),
            ));
        }
        if width > Self::MAX_SIZE || height > Self::MAX_SIZE {
            return Err(PresetError::Malformed(
                None,
                format!(
                    "brush tip is {width}x{height}, larger than {}",
                    Self::MAX_SIZE
                ),
            ));
        }
        let expected = width as usize * height as usize;
        if coverage.len() != expected {
            return Err(PresetError::Malformed(
                None,
                format!(
                    "brush tip has {} bytes, expected {expected}",
                    coverage.len()
                ),
            ));
        }
        Ok(Self {
            width,
            height,
            coverage,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Row-major coverage, `width * height` bytes.
    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    /// Coverage at one texel. Out of range reads as no paint rather than
    /// panicking — a tip is data from a file somebody else wrote.
    pub fn at(&self, x: u32, y: u32) -> u8 {
        if x >= self.width || y >= self.height {
            return 0;
        }
        self.coverage[(y * self.width + x) as usize]
    }

    /// The mask's proportions, longer side normalised to 1.
    ///
    /// A non-square mask used to be **padded into a square**, because the dab
    /// stretched whatever it was given over its bounding box. The recorded
    /// reason for padding rather than using [`crate::Brush::dab_ratio`] still
    /// stands — the ratio's long axis is the dab's *x* axis, so a portrait mask
    /// would have to be rotated a quarter turn and rotated back, and the ratio
    /// is the user's setting rather than the file's — but it turns out not to
    /// be a choice between those two.
    ///
    /// The dab pass is now told the tip's proportions directly and shapes its
    /// quad to match. That is the **same geometry padding produced** (a mask
    /// padded to side `max(w, h)` and stretched over a square occupies exactly
    /// `w/side` by `h/side` of it) with three costs removed: no empty margin to
    /// shade, no padded texture to upload, and `dab_ratio` still free. The
    /// quarter-turn problem never arises because this scales the dab's own
    /// axes rather than borrowing the ratio.
    pub fn aspect(&self) -> (f32, f32) {
        let long = self.width.max(self.height) as f32;
        (self.width as f32 / long, self.height as f32 / long)
    }

    /// Encode as an 8-bit greyscale PNG — how a tip is stored in the library.
    ///
    /// PNG rather than the raw bytes because it carries its own dimensions and
    /// because the files are then ordinary images: a tip can be looked at,
    /// replaced or copied between machines with anything that opens a picture,
    /// which matters for a format whose whole content is somebody else's
    /// bitmap. `png` is already a dependency of this crate for document import,
    /// so it costs nothing to build.
    pub fn to_png(&self) -> Result<Vec<u8>, PresetError> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| malformed(format!("the brush tip could not be written ({e})")))?;
            writer
                .write_image_data(&self.coverage)
                .map_err(|e| malformed(format!("the brush tip could not be written ({e})")))?;
        }
        Ok(out)
    }

    /// Decode a tip written by [`TipMask::to_png`].
    ///
    /// Only greyscale is accepted. A tip *is* a coverage mask, and for a
    /// colour image there is no answer to "which channel is the coverage" that
    /// is right for every file — guessing produces a brush that is quietly the
    /// wrong shape, which is worse than a sentence saying what was expected.
    pub fn from_png(bytes: &[u8]) -> Result<Self, PresetError> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        // Expands a 1/2/4-bit or 16-bit greyscale file to the 8 bits the mask
        // holds, so a tip dropped in by hand does not have to be exactly what
        // Umber writes.
        decoder.set_transformations(png::Transformations::normalize_to_color8());

        let mut reader = decoder.read_info().map_err(|e| {
            malformed(format!(
                "the brush tip's PNG header could not be read ({e})"
            ))
        })?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| malformed("the brush tip is too large to decode".to_string()))?;
        let mut buf = vec![0u8; size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| malformed(format!("the brush tip could not be decoded ({e})")))?;

        let texels = info.width as usize * info.height as usize;
        let coverage = match info.color_type {
            png::ColorType::Grayscale => {
                buf.truncate(texels);
                buf
            }
            // Written by some editors when the image has an alpha channel it
            // does not use. The grey is still the coverage.
            png::ColorType::GrayscaleAlpha => {
                buf[..texels * 2].chunks_exact(2).map(|px| px[0]).collect()
            }
            other => {
                return Err(malformed(format!(
                    "a brush tip must be an 8-bit greyscale PNG, and this one is {other:?}"
                )));
            }
        };
        Self::new(info.width, info.height, coverage)
    }
}

/// Every mask Umber ships, by name, decoded once.
///
/// The shipped library is an embedded RON and a bitmap does not go in a text
/// file, so the masks are embedded separately and named from
/// [`crate::BrushPreset::tip`] exactly as a user's own are. Naming rather than
/// embedding-per-preset is what lets two shipped brushes cut from one stamp
/// share a file and a single GPU upload.
///
/// The `Arc`s are stable for the life of the process, which
/// `CanvasRenderer::set_tip` depends on: it compares tips by pointer identity to
/// decide whether the GPU upload can be skipped, and a fresh `Arc` per lookup
/// would put a texture allocation on the first frame of every stroke.
///
/// A shipped file that will not decode is dropped rather than fatal —
/// `every_shipped_tip_decodes_and_makes_a_mark` is what stops one reaching a
/// release — because a brush that paints round is a far better outcome than a
/// binary that will not start.
pub fn builtin_tips() -> &'static BTreeMap<&'static str, Arc<TipMask>> {
    static TIPS: OnceLock<BTreeMap<&'static str, Arc<TipMask>>> = OnceLock::new();
    TIPS.get_or_init(|| {
        crate::tip_table::TIPS
            .iter()
            .filter_map(|(name, bytes)| {
                TipMask::from_png(bytes)
                    .ok()
                    .map(|mask| (*name, Arc::new(mask)))
            })
            .collect()
    })
}

/// The shipped mask a [`crate::BrushPreset::tip`] names, if it is one of ours.
pub fn builtin(name: &str) -> Option<&'static Arc<TipMask>> {
    builtin_tips().get(name)
}

/// Every paper grain Umber ships, by name, decoded once.
///
/// The same mechanism as [`builtin_tips`] and the same `Arc` stability
/// guarantee, because the grain is uploaded and compared the same way. These
/// are **tiles**: seamless by construction, since the grain is anchored to the
/// document and repeats across it, and a seam would draw a grid over every
/// stroke.
pub fn patterns() -> &'static BTreeMap<&'static str, Arc<TipMask>> {
    static PATTERNS: OnceLock<BTreeMap<&'static str, Arc<TipMask>>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        crate::pattern_table::PATTERNS
            .iter()
            .filter_map(|(name, bytes)| {
                TipMask::from_png(bytes)
                    .ok()
                    .map(|mask| (*name, Arc::new(mask)))
            })
            .collect()
    })
}

/// The tile a [`crate::brush::GrainPattern`] names.
pub fn pattern(name: &str) -> Option<&'static Arc<TipMask>> {
    patterns().get(name)
}

/// What a straight stroke of one tip reaches, under each of the two coverage
/// rules the dab pass offers.
///
/// The measurement that decides whether a stamp can be shipped, and the reason
/// it is a function rather than a judgement: a photographic texture stamp looks
/// dense and is not. See `docs/brush-sources.md`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeCoverage {
    /// Strongest coverage anywhere in the stroke under the wet-layer `max`.
    ///
    /// Bounded above by the mask's own brightest texel, whatever the spacing
    /// and however long the stroke — which is the entire problem.
    pub under_max: f32,
    /// The same, with each dab compositing over the last:
    /// [`crate::Brush::build_up`]. This is what GIMP and Krita draw.
    pub under_build_up: f32,
}

impl StrokeCoverage {
    /// How close the two rules agree, as a fraction. `1.0` is a tip that needs
    /// no build-up at all.
    pub fn agreement(&self) -> f32 {
        if self.under_build_up <= f32::EPSILON {
            return 1.0;
        }
        (self.under_max / self.under_build_up).clamp(0.0, 1.0)
    }

    /// Whether a `max` stroke of this tip is the same mark a compositing one
    /// makes, to within a level of 8-bit storage either way.
    ///
    /// True for a dense stamp — a solid silhouette, a blot — whose texels
    /// already reach 1.0, and false for every sparse photographic texture. A
    /// tip that answers false must be shipped with `build_up` set, or it paints
    /// at a fraction of the strength its author drew it at.
    pub fn needs_build_up(&self) -> bool {
        self.agreement() < 0.98
    }

    /// Whether the tip makes a usable mark at all once it is allowed to build.
    ///
    /// The floor is the `R8Unorm` scratch: a dab weaker than `1/255` rounds
    /// away and a stroke of them never accumulates, so a mask that faint is a
    /// brush that paints nothing however hard it is pressed.
    pub fn is_usable(&self) -> bool {
        self.under_build_up >= 0.5
    }
}

/// Stamp `mask` along a straight line at its own scale and report the peak
/// coverage under both rules.
///
/// The model, deliberately the same one the dab pass implements:
///
/// - the tip is stretched over the dab's bounding square, side `max(w, h)`,
///   with a non-square mask centred in it — exactly what
///   [`TipMask::aspect`] gives the dab pass, and the same geometry padding the
///   mask into a square used to produce;
/// - dabs land every `spacing * side` document pixels;
/// - `max` takes the strongest texel any dab put at that pixel, and build-up
///   composites: `a += cov(1 - a)`.
///
/// Peak rather than mean, because peak is what an artist reads as "the strength
/// of the stroke" and because it is the figure a `max` blend caps: no `max`
/// stroke can ever exceed the mask's brightest texel.
///
/// Sampled over one full period of the stamp spacing, well inside the stroke so
/// that neither end is measured, and over the whole height of the mask.
pub fn stroke_coverage(mask: &TipMask, spacing: f32) -> StrokeCoverage {
    let side = mask.width().max(mask.height()) as f32;
    let step = (spacing.max(0.001) * side).max(1.0);
    // Enough stamps that a pixel in the sampled period is reached by every dab
    // whose footprint can contain it, from both sides.
    let count = (side / step).ceil() as i32 * 2 + 2;
    // Start far enough in that the sampled period sees a full complement.
    let base = count / 2;

    // The mask, centred in the square, in texel coordinates.
    let x0 = (side - mask.width() as f32) * 0.5;
    let y0 = (side - mask.height() as f32) * 0.5;

    let mut under_max: f32 = 0.0;
    let mut under_build_up: f32 = 0.0;
    let period = step.ceil() as i32;
    let half = (side * 0.5).ceil() as i32;

    for py in -half..=half {
        for px in 0..period {
            let x = (base as f32) * step + px as f32;
            let mut peak: f32 = 0.0;
            let mut built: f32 = 0.0;
            for i in 0..count {
                // Position within the dab's bounding square, then within the
                // mask sitting centred in it.
                let u = x - i as f32 * step + side * 0.5 - x0;
                let v = py as f32 + side * 0.5 - y0;
                if u < 0.0 || v < 0.0 {
                    continue;
                }
                let cov = mask.at(u as u32, v as u32) as f32 / 255.0;
                peak = peak.max(cov);
                built += cov * (1.0 - built);
            }
            under_max = under_max.max(peak);
            under_build_up = under_build_up.max(built);
        }
    }

    StrokeCoverage {
        under_max,
        under_build_up,
    }
}

fn malformed(message: String) -> PresetError {
    PresetError::Malformed(None, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mask_reads_back_row_major() {
        let tip = TipMask::new(3, 2, vec![1, 2, 3, 4, 5, 6]).expect("build");
        assert_eq!(tip.at(0, 0), 1);
        assert_eq!(tip.at(2, 0), 3);
        assert_eq!(tip.at(0, 1), 4);
        assert_eq!(tip.at(2, 1), 6);
    }

    #[test]
    fn out_of_range_is_empty_rather_than_a_panic() {
        let tip = TipMask::new(2, 2, vec![255; 4]).expect("build");
        assert_eq!(tip.at(2, 0), 0);
        assert_eq!(tip.at(0, 2), 0);
        assert_eq!(tip.at(u32::MAX, u32::MAX), 0);
    }

    #[test]
    fn a_mismatched_buffer_is_refused() {
        assert!(TipMask::new(4, 4, vec![0; 15]).is_err());
        assert!(TipMask::new(4, 4, vec![0; 17]).is_err());
        assert!(TipMask::new(0, 4, vec![]).is_err());
        assert!(TipMask::new(4096, 1, vec![0; 4096]).is_err());
    }

    /// The whole point of storing tips as files: what comes back has to be the
    /// mask that went in, to the byte. A lossy step here would change the shape
    /// of every stamp brush every time the library was reloaded.
    #[test]
    fn a_mask_survives_a_png_round_trip_byte_for_byte() {
        let coverage: Vec<u8> = (0..(17 * 5)).map(|i| (i * 7 % 256) as u8).collect();
        let tip = TipMask::new(17, 5, coverage.clone()).expect("build");
        let png = tip.to_png().expect("encode");
        let back = TipMask::from_png(&png).expect("decode");

        assert_eq!(back.width(), 17);
        assert_eq!(back.height(), 5);
        assert_eq!(back.coverage(), coverage);
        assert_eq!(back, tip);
    }

    #[test]
    fn a_colour_png_is_refused_rather_than_guessed_at() {
        // Which channel would the coverage be? There is no answer that is right
        // for every file, and a wrong guess is a brush that is quietly the
        // wrong shape.
        let mut rgb = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut rgb, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[10, 20, 30]).expect("data");
        }
        let err = TipMask::from_png(&rgb).expect_err("colour is refused");
        assert!(err.to_string().contains("greyscale"), "{err}");

        assert!(TipMask::from_png(b"not a png").is_err());
    }

    #[test]
    fn a_masks_aspect_normalises_its_longer_side() {
        // What the dab pass multiplies its quad's axes by, and what replaced
        // padding a stamp into a square. The long side is 1.0 either way round,
        // because `Brush::size` describes the long axis.
        let tall = TipMask::new(2, 4, vec![9; 8]).expect("build");
        assert_eq!(tall.aspect(), (0.5, 1.0));

        let wide = TipMask::new(4, 2, vec![9; 8]).expect("build");
        assert_eq!(wide.aspect(), (1.0, 0.5));

        // A square mask scales nothing, which is the exact identity the round
        // path relies on.
        let square = TipMask::new(2, 2, vec![1, 2, 3, 4]).expect("build");
        assert_eq!(square.aspect(), (1.0, 1.0));
    }

    /// A shipped tip that will not decode is a brush that paints round, which
    /// is a quiet failure. The table is generated from a directory listing, so
    /// the way to get one is to commit a file that is not a greyscale PNG.
    #[test]
    fn every_shipped_tip_decodes_and_makes_a_mark() {
        assert_eq!(
            builtin_tips().len(),
            crate::tip_table::TIPS.len(),
            "a shipped tip failed to decode"
        );
        assert!(!builtin_tips().is_empty(), "the table should not be empty");

        for (name, mask) in builtin_tips() {
            assert!(
                stroke_coverage(mask, 0.1).is_usable(),
                "{name} is too faint to accumulate — it would paint nothing"
            );
        }
    }

    /// Every shipped stamp has to paint at the strength it was drawn at, which
    /// after the build-up work is a measurable claim rather than a hope.
    #[test]
    fn a_shipped_stamp_paints_at_the_strength_it_was_drawn_at() {
        let stamped: Vec<_> = crate::preset::builtin()
            .iter()
            .filter(|p| p.tip.is_some())
            .collect();
        assert!(
            !stamped.is_empty(),
            "the shipped library carries no stamp brush, so the mechanism is untested"
        );

        for preset in stamped {
            let name = preset.tip.as_deref().expect("tip");
            let mask = builtin(name)
                .unwrap_or_else(|| panic!("{} names a tip nobody ships: {name}", preset.name));
            let measured = stroke_coverage(mask, preset.brush.spacing);
            assert_eq!(
                preset.brush.build_up,
                measured.needs_build_up(),
                "{} is shipped with build_up = {} but measures {measured:?}",
                preset.name,
                preset.brush.build_up
            );
        }
    }

    /// The grain is anchored to the document and repeats across it, so a seam
    /// would draw a grid over every stroke that used it.
    ///
    /// Tested as a statistic rather than as an equality: the tiles are noise, so
    /// neighbouring texels differ everywhere. What must not happen is for the
    /// pair *across* the seam to differ by more than pairs inside the tile do.
    #[test]
    fn every_shipped_pattern_tiles_without_a_seam() {
        assert_eq!(
            patterns().len(),
            crate::pattern_table::PATTERNS.len(),
            "a shipped pattern failed to decode"
        );

        for (name, tile) in patterns() {
            let (w, h) = (tile.width(), tile.height());
            let step = |a: u8, b: u8| a.abs_diff(b) as f32;

            let interior: f32 = (0..h)
                .map(|y| step(tile.at(w / 2, y), tile.at(w / 2 + 1, y)))
                .sum::<f32>()
                / h as f32;
            let seam: f32 = (0..h)
                .map(|y| step(tile.at(w - 1, y), tile.at(0, y)))
                .sum::<f32>()
                / h as f32;
            assert!(
                seam <= interior * 2.0 + 1.0,
                "{name} has a vertical seam: {seam:.2} across it, {interior:.2} inside"
            );

            let interior: f32 = (0..w)
                .map(|x| step(tile.at(x, h / 2), tile.at(x, h / 2 + 1)))
                .sum::<f32>()
                / w as f32;
            let seam: f32 = (0..w)
                .map(|x| step(tile.at(x, h - 1), tile.at(x, 0)))
                .sum::<f32>()
                / w as f32;
            assert!(
                seam <= interior * 2.0 + 1.0,
                "{name} has a horizontal seam: {seam:.2} across it, {interior:.2} inside"
            );
        }
    }

    /// The measurement `docs/brush-sources.md` turns on, in a form small enough
    /// to check by hand.
    #[test]
    fn a_faint_stamp_needs_build_up_and_a_solid_one_does_not() {
        // Every texel at 0.49 — the peak of the sparsest CC0 pack sampled. A
        // `max` stroke of it can never pass 0.49 however many times it lands;
        // compositing reaches solid.
        let faint = TipMask::new(8, 8, vec![125; 64]).expect("build");
        let measured = stroke_coverage(&faint, 0.1);
        assert!((measured.under_max - 125.0 / 255.0).abs() < 1e-3);
        assert!(
            measured.under_build_up > 0.99,
            "compositing should reach solid, got {measured:?}"
        );
        assert!(measured.needs_build_up());
        assert!(measured.is_usable());

        // A solid stamp is the same mark under either rule, so it ships on the
        // `max` path — which is the cheaper one and the one that keeps a stroke
        // crossing itself even.
        let solid = TipMask::new(8, 8, vec![255; 64]).expect("build");
        let measured = stroke_coverage(&solid, 0.1);
        assert_eq!(measured.under_max, 1.0);
        assert!(!measured.needs_build_up());

        // And a mask too faint for eight-bit coverage to accumulate is not a
        // brush at all.
        let ghost = TipMask::new(8, 8, vec![0; 64]).expect("build");
        assert!(!stroke_coverage(&ghost, 0.1).is_usable());
    }
}
