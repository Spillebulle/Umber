//! Text rasterised on the CPU, for the splash.
//!
//! Everything else in Umber draws text through egui, which needs a GPU device
//! and a glyph atlas. `splash.rs` runs *before* either of those exists — that is
//! its entire purpose — so it has to turn a string into coverage by itself.
//!
//! Doing that buys one thing beyond "text without a GPU", and it is the reason
//! this module uses `skrifa` rather than the `ab_glyph` already sitting in the
//! tree. Archivo is a **variable** font. `ab_glyph` does not apply variation
//! axes, so egui renders the Regular master whatever weight is asked for — which
//! is why the interface has no bold and why `theme.rs` says so. `skrifa` does
//! apply them. The splash can therefore ask for the weight 900 the design
//! specifies for the wordmark and genuinely get it, rather than quietly drawing
//! 400 and looking wrong.
//!
//! `ab_glyph_rasterizer` then turns the outlines into coverage. It is the same
//! rasteriser egui uses, just addressed directly.
//!
//! The font bytes are included a second time here rather than shared with
//! `theme.rs`: that module hands its copy to egui as a `'static` slice inside an
//! `Arc<FontData>` and never exposes it. A quarter of a megabyte of duplicated
//! read-only data is the price of not widening `theme.rs`'s interface for one
//! caller.

use ab_glyph_rasterizer::{Point, Rasterizer, point};
use skrifa::instance::{Location, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, MetadataProvider};

/// Archivo, the typeface the design specifies — the same file the interface
/// loads, under the SIL Open Font License. See `assets/fonts/`.
///
/// Public to the crate because `textpanel` registers it as the one face
/// `umber_core::fonts` is guaranteed to have. That is what stops the bytes
/// being compiled in a *third* time: `umber-core` deliberately holds no font
/// of its own and is handed this one.
pub const ARCHIVO: &[u8] = include_bytes!("../../../assets/fonts/Archivo[wdth,wght].ttf");

/// Archivo instanced at one size and weight, ready to draw.
///
/// Built per string rather than cached: the splash draws four short runs, once
/// each, and a cache would outlive the only thing that uses it.
pub struct Font {
    face: FontRef<'static>,
    location: Location,
    size: Size,
    /// Extra space added after every glyph. The design tracks the wordmark
    /// tight (−2 px at 64) and the small caps lines loose (+3 px at 11.5).
    tracking: f32,
}

impl Font {
    /// Instance Archivo at `px` and `weight` on the `wght` axis.
    ///
    /// `weight` is clamped to the axis's own range by skrifa, so asking for a
    /// weight the font does not carry degrades to its heaviest rather than
    /// failing.
    pub fn new(px: f32, weight: f32, tracking: f32) -> Option<Self> {
        let face = FontRef::new(ARCHIVO).ok()?;
        let location = face.axes().location([("wght", weight)]);
        Some(Self {
            face,
            location,
            size: Size::new(px),
            tracking,
        })
    }

    /// Total advance of `text`, in pixels.
    pub fn width(&self, text: &str) -> f32 {
        let metrics = self.face.glyph_metrics(self.size, &self.location);
        let charmap = self.face.charmap();
        let mut w = 0.0;
        for ch in text.chars() {
            let Some(gid) = charmap.map(ch) else { continue };
            w += metrics.advance_width(gid).unwrap_or(0.0) + self.tracking;
        }
        // The trailing letter's tracking is space *after* the run, which would
        // push a centred string left by half of it.
        (w - self.tracking).max(0.0)
    }

    /// Distance from the baseline to the top of a capital letter.
    ///
    /// Used to centre a line on something other than its baseline, which is
    /// what every measurement in the design is relative to.
    pub fn cap_height(&self) -> f32 {
        self.face
            .metrics(self.size, &self.location)
            .cap_height
            .unwrap_or_else(|| self.size.ppem().unwrap_or(0.0) * 0.72)
    }

    /// Rasterise `text` with its left edge at `x` and its baseline at `y`,
    /// calling `plot(x, y, coverage)` for every pixel the glyphs touch.
    ///
    /// Coverage is 0..=1 and already antialiased; the caller decides how to
    /// blend it.
    pub fn draw(&self, text: &str, x: f32, y: f32, mut plot: impl FnMut(i32, i32, f32)) {
        let metrics = self.face.glyph_metrics(self.size, &self.location);
        let charmap = self.face.charmap();
        let outlines = self.face.outline_glyphs();
        let ppem = self.size.ppem().unwrap_or(0.0);

        // One rasteriser for the whole run rather than one per glyph: glyphs
        // may overlap when tracking is negative, as it is for the wordmark, and
        // separate buffers would show a seam where they do.
        //
        // The box is generous on every side because outlines routinely reach
        // past the advance width — accents above, descenders below, and
        // overshoot on round letters.
        let pad = ppem;
        let width = (self.width(text) + pad * 2.0).ceil().max(1.0) as usize;
        let height = (ppem * 2.5).ceil().max(1.0) as usize;
        let baseline = ppem * 1.8;

        let mut raster = Rasterizer::new(width, height);
        let mut cursor = pad;
        for ch in text.chars() {
            let Some(gid) = charmap.map(ch) else { continue };
            if let Some(glyph) = outlines.get(gid) {
                let settings = DrawSettings::unhinted(self.size, &self.location);
                let mut pen = Pen {
                    raster: &mut raster,
                    dx: cursor,
                    baseline,
                    last: point(0.0, 0.0),
                    start: point(0.0, 0.0),
                    bounds: (width as f32, height as f32),
                };
                // A glyph that will not draw is skipped rather than aborting the
                // run; a splash missing one letter beats a splash missing all
                // of them.
                let _ = glyph.draw(settings, &mut pen);
            }
            cursor += metrics.advance_width(gid).unwrap_or(0.0) + self.tracking;
        }

        let ox = (x - pad).round() as i32;
        let oy = (y - baseline).round() as i32;
        raster.for_each_pixel_2d(|px, py, coverage| {
            if coverage > 0.0 {
                plot(ox + px as i32, oy + py as i32, coverage.min(1.0));
            }
        });
    }
}

/// Feeds glyph outlines to the rasteriser, flipping to y-down and offsetting to
/// the run's position as it goes.
struct Pen<'r> {
    raster: &'r mut Rasterizer,
    dx: f32,
    baseline: f32,
    last: Point,
    start: Point,
    bounds: (f32, f32),
}

impl Pen<'_> {
    /// Font space is y-up from the baseline; the rasteriser is y-down from the
    /// top of its box.
    ///
    /// Clamped to the box: `Rasterizer` indexes its accumulation buffer from
    /// these coordinates, and a glyph whose outline strays outside — which
    /// happens with overshoot on round letters at small sizes — would otherwise
    /// be a panic rather than a clipped pixel.
    fn at(&self, x: f32, y: f32) -> Point {
        point(
            (self.dx + x).clamp(0.0, self.bounds.0),
            (self.baseline - y).clamp(0.0, self.bounds.1),
        )
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        // An unclosed contour would leak coverage across the whole scanline, so
        // the start is remembered and `close` always joins back to it.
        self.last = self.at(x, y);
        self.start = self.last;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let to = self.at(x, y);
        self.raster.draw_line(self.last, to);
        self.last = to;
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let ctrl = self.at(cx, cy);
        let to = self.at(x, y);
        self.raster.draw_quad(self.last, ctrl, to);
        self.last = to;
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        let c0 = self.at(cx0, cy0);
        let c1 = self.at(cx1, cy1);
        let to = self.at(x, y);
        self.raster.draw_cubic(self.last, c0, c1, to);
        self.last = to;
    }

    fn close(&mut self) {
        self.raster.draw_line(self.last, self.start);
        self.last = self.start;
    }
}

/// Whether Archivo carries a glyph for every character in `text`.
///
/// The interface's standing rule is that no Unicode symbol may be put in the UI,
/// because Archivo carries none of the ones people reach for and they render as
/// blank boxes. The splash uses one character that is *not* obviously safe, the
/// middle dot, so this exists to let a test prove it is really in the font
/// rather than assume it. That is its only caller, hence the gate: it is a
/// proof obligation, not a runtime check.
///
/// It used to be two, the second being an em dash. Nothing the interface draws
/// carries one now — the shader line joins with a colon — but the check still
/// runs over the whole of every string the splash draws, so a character added
/// to one of them is covered without this comment having to name it.
#[cfg(test)]
pub fn covers(text: &str) -> bool {
    let Ok(face) = FontRef::new(ARCHIVO) else {
        return false;
    };
    let charmap = face.charmap();
    text.chars().all(|ch| charmap.map(ch).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(text: &str, px: f32, weight: f32) -> f32 {
        let font = Font::new(px, weight, 0.0).expect("Archivo should parse");
        let mut total = 0.0;
        font.draw(text, 0.0, px * 1.5, |_, _, coverage| total += coverage);
        total
    }

    #[test]
    fn archivo_parses_and_draws() {
        assert!(ink("UMBER", 64.0, 400.0) > 0.0);
    }

    #[test]
    fn the_weight_axis_is_actually_applied() {
        // The whole reason this module exists rather than reusing egui's
        // `ab_glyph`: if variations were being ignored, these two would
        // rasterise identically and the design's weight 900 wordmark would
        // silently be a weight 400 one.
        let light = ink("UMBER", 64.0, 100.0);
        let regular = ink("UMBER", 64.0, 400.0);
        let black = ink("UMBER", 64.0, 900.0);
        assert!(
            regular > light * 1.2,
            "weight 400 ({regular}) is not meaningfully heavier than 100 ({light})"
        );
        assert!(
            black > regular * 1.2,
            "weight 900 ({black}) is not meaningfully heavier than 400 ({regular})"
        );
    }

    #[test]
    fn tracking_moves_the_end_of_the_run_and_nothing_else() {
        let tight = Font::new(64.0, 900.0, -2.0).unwrap();
        let loose = Font::new(64.0, 900.0, 3.0).unwrap();
        assert!(loose.width("UMBER") > tight.width("UMBER"));
        // Tracking is space *between* letters, so a single glyph is unaffected.
        assert!((loose.width("U") - tight.width("U")).abs() < 0.01);
    }

    #[test]
    fn archivo_carries_every_character_the_splash_draws() {
        // Including the middle dot, which is the one character here that is not
        // plain ASCII. If any of these ever fails, the fix is to change the
        // string, not to add a fallback font.
        assert!(covers("UMBER"), "the wordmark");
        assert!(covers("GPU PAINTING · v0.1.0"), "the middle dot");
        assert!(covers("compiling shaders: dab.wgsl"), "the shader line");
        assert!(covers("D3D12 · NVIDIA GeForce RTX 4070"), "an adapter line");
    }

    #[test]
    fn an_unmapped_character_is_skipped_rather_than_drawn_as_a_box() {
        // A private-use codepoint no text face carries. Skipping keeps the rest
        // of the line legible; drawing .notdef would put the blank box the
        // interface rule exists to prevent right in the middle of the brand.
        let font = Font::new(24.0, 400.0, 0.0).unwrap();
        assert_eq!(font.width("\u{E000}"), 0.0);
    }
}
