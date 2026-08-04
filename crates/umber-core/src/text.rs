//! Turning a string, a face and a size into coverage.
//!
//! # What comes out, and where it goes
//!
//! A [`Setting`] is a rectangle of **coverage** — one byte a pixel, antialiased,
//! trimmed to the text's own ink — and [`Setting::clip`] paints it in a colour
//! and hands back a [`Clip`](crate::Clip). From there the caller does exactly
//! what a paste does: `Clip::place` decides where it goes, `begin_float` puts it
//! on the canvas, and the transform tool moves, scales, turns and commits it.
//!
//! **That reuse is the whole design.** Text placed on the canvas *is* a paste —
//! no lift, so no hole to restore and no mask pass — and `render_float` is one
//! function called twice, so the preview and the commit cannot disagree about a
//! single pixel. Nothing in `umber-render` learns that there is such a thing as
//! text, exactly as nothing in it learns that there is such a thing as a paste.
//!
//! # Why shaping rather than a charmap walk
//!
//! `umber-app`'s `cputext.rs` already turns a string into coverage with no GPU:
//! it maps characters through the `charmap` and sums advance widths. That is
//! right for four short ASCII runs of one known font on the splash, and it is
//! **wrong — not plainer, wrong** — for text somebody types. No kerning, no
//! ligatures, no mark positioning, and unshaped Arabic is a row of disconnected
//! isolated forms rather than a word. A text tool that renders somebody's
//! language as nonsense is the control that lies.
//!
//! So the glyphs and their positions come from `harfrust`, a complete HarfBuzz
//! port, and the outlines from `skrifa` — the two the interface's own text
//! already sits on, at the versions `epaint` already asks for. See the workspace
//! manifest for why that is not `cosmic-text`.
//!
//! # What this deliberately does not do, and says so
//!
//! Three things `cosmic-text` would have brought, each named here because the
//! interface has to be able to say it:
//!
//! * **No bidirectional reordering.** `harfrust` shapes one run in one
//!   direction, and the direction is guessed from the first strong character.
//!   A wholly Arabic or Hebrew line is right; a line mixing an English phrase
//!   into an Arabic sentence puts the two in the wrong order.
//!   [`Setting::mixed_directions`] is what says so.
//! * **No line wrapping.** Lines break at `\n` and nowhere else, which is what
//!   "point text" means in every application that draws this distinction. Area
//!   text — a box you drag, that reflows — is the later feature, and it needs
//!   Unicode line breaking rather than a space hunt.
//! * **No font fallback.** A character the chosen face has no glyph for is
//!   *reported*, in [`Setting::missing`], rather than being quietly filled from
//!   somewhere else. Asking "which of this machine's four hundred faces has this
//!   codepoint" and splitting a run across the answers is the piece that looks
//!   trivial and is not; naming what is missing is honest, and it is the rule
//!   every import in this codebase already follows.

use crate::clipboard::Clip;
use crate::color::Color;
use crate::fonts::{Face, FontData};
use ab_glyph_rasterizer::{Point, Rasterizer, point};
use harfrust::{ShaperData, ShaperInstance, UnicodeBuffer};
use skrifa::instance::{Location, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{GlyphId, MetadataProvider};

/// The most pixels one block of text may cover.
///
/// A cap rather than a hope. The rasterised block is a byte a pixel and the
/// clip it becomes is four, so this is 64 MB of coverage and 256 MB of colour —
/// already past what is reasonable to hand a paste. Text asked for larger than
/// this is *refused with the figure*, because the alternative is an allocation
/// nobody asked for on the way to a canvas that could not hold it anyway:
/// `Clip::place` crops to the document, so pixels beyond it were never going to
/// arrive.
pub const MAX_PIXELS: u64 = 64 << 20;

/// The largest point size the rails offer, in document pixels.
///
/// Not a limit of anything here — [`MAX_PIXELS`] is the real bound — but the
/// number a control has to stop at, and it belongs beside the model rather than
/// beside the slider that draws it.
pub const MAX_SIZE: f32 = 1000.0;

/// The smallest. Below about four pixels an em nothing legible survives the
/// rasteriser, and a zero would be a division by zero in the scale.
pub const MIN_SIZE: f32 = 4.0;

/// Where a line sits within the block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Align {
    #[default]
    Left,
    Centre,
    Right,
}

impl Align {
    pub const ALL: [Align; 3] = [Align::Left, Align::Centre, Align::Right];

    pub fn label(self) -> &'static str {
        match self {
            Align::Left => "Left",
            Align::Centre => "Centre",
            Align::Right => "Right",
        }
    }
}

/// What to set, and how.
///
/// The face is *not* in here: it is a [`Face`] the caller resolved out of the
/// library, and holding a name in the same struct as the size would mean two
/// places deciding which face a block is in. See [`set`].
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    pub text: String,
    /// The em size, in **document** pixels — so text is as zoom-independent as
    /// a brush is, and for the same reason.
    pub size: f32,
    /// Multiplier on the face's own line height. 1.0 is what the designer of
    /// the typeface chose.
    pub line_spacing: f32,
    /// Extra space after every glyph, in document pixels. Negative tightens.
    pub tracking: f32,
    pub align: Align,
}

impl Default for TextBlock {
    fn default() -> Self {
        Self {
            text: String::new(),
            size: 72.0,
            line_spacing: 1.0,
            tracking: 0.0,
            align: Align::Left,
        }
    }
}

/// Why a block could not be set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextError {
    /// Nothing was typed.
    Empty,
    /// Something was typed and none of it makes a mark — spaces, or a line of
    /// characters the face has no glyph for. Distinct from [`Self::Empty`]
    /// because the interface has different things to say about the two.
    NoInk,
    /// Past [`MAX_PIXELS`]. Carries what was asked for, so the notice can name
    /// it rather than saying "too big".
    TooLarge { width: u32, height: u32 },
    /// The face would not parse, or holds no outlines.
    Unreadable,
}

/// A rectangle of antialiased coverage, trimmed to its own ink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    pub width: u32,
    pub height: u32,
    /// `width * height`, row-major, 0..=255.
    pub coverage: Vec<u8>,
    /// Characters the face has no glyph for, in the order they were first met,
    /// de-duplicated. Empty is the ordinary case.
    ///
    /// Reported rather than filled in from another face — see the module docs.
    pub missing: Vec<char>,
    /// True where a line held both left-to-right and right-to-left strong
    /// characters, so the run this shaped is in one direction and the text is
    /// not. Without bidi reordering the two halves come out in the wrong order,
    /// and that has to be said rather than discovered.
    pub mixed_directions: bool,
}

impl Setting {
    /// Paint the coverage in `colour` and hand back a clip ready for
    /// `Clip::place`.
    ///
    /// **Straight-alpha sRGB**, which is what a [`Clip`] holds and therefore
    /// what the paste path already knows how to premultiply. The colour's own
    /// alpha multiplies the coverage, so a colour picked at half opacity sets
    /// half-opacity text — one multiply, on the straight side, where it is the
    /// same arithmetic the coverage already is.
    pub fn clip(&self, colour: Color) -> Option<Clip> {
        let [r, g, b, a] = colour.to_srgb_u8();
        let mut pixels = Vec::with_capacity(self.coverage.len() * 4);
        for &cov in &self.coverage {
            pixels.extend_from_slice(&[r, g, b, ((cov as u32 * a as u32 + 127) / 255) as u8]);
        }
        Clip::from_rgba(self.width, self.height, pixels)
    }
}

/// Set `block` in `face`.
///
/// The face is loaded by the caller — [`Face::load`] blocks on a file read, and
/// this is called from an interface that redraws — so what arrives here is
/// bytes and a location on the variable axes.
pub fn set(face: &Face, data: &FontData, block: &TextBlock) -> Result<Setting, TextError> {
    if block.text.is_empty() {
        return Err(TextError::Empty);
    }
    let font = data.font().ok_or(TextError::Unreadable)?;
    // Empty variations is the exact identity and the fast path: a static face,
    // and a variable font's own default instance, both take `Location::default`
    // and no instancing work at all.
    let location = if face.variations.is_empty() {
        Location::default()
    } else {
        font.axes().location(
            face.variations
                .iter()
                .map(|(tag, v)| (tag.as_str(), *v))
                .collect::<Vec<_>>(),
        )
    };
    let size = block.size.clamp(MIN_SIZE, MAX_SIZE);
    let metrics = font.metrics(Size::new(size), &location);
    let upem = metrics.units_per_em.max(1) as f32;
    let scale = size / upem;

    // The shaper is built once for the whole block: `ShaperData::new` walks
    // every OpenType table the font has, which is far too much to pay per line.
    let shaper_data = ShaperData::new(&font);
    let instance = (!face.variations.is_empty()).then(|| {
        ShaperInstance::from_variations(
            &font,
            face.variations.iter().map(|(t, v)| (t.as_str(), *v)),
        )
    });
    let shaper = shaper_data
        .shaper(&font)
        .instance(instance.as_ref())
        .build();

    let mut lines = Vec::new();
    let mut missing: Vec<char> = Vec::new();
    let mut mixed = false;
    for text in block.text.split('\n') {
        let line = shape_line(
            &shaper,
            text,
            scale,
            block.tracking,
            &mut missing,
            &mut mixed,
        );
        lines.push(line);
    }

    let block_width = lines.iter().fold(0.0_f32, |w, l| w.max(l.width));
    // The face's own line height, which is what its designer chose, times what
    // the artist asked for. `leading` is the gap the font states *between*
    // lines; ascent and descent are the box one line occupies.
    let line_height = ((metrics.ascent - metrics.descent) + metrics.leading) * block.line_spacing;
    // Generous on every side: outlines routinely reach past the advance width —
    // accents above, descenders below, overshoot on round letters, and a swash
    // or a script face further than any of them. The block is trimmed to its
    // ink afterwards, so this costs a transient rather than a margin.
    let pad = size;
    let width = (block_width + pad * 2.0).ceil().max(1.0);
    let height = (line_height * lines.len() as f32 + pad * 2.0)
        .ceil()
        .max(1.0);
    if width * height > MAX_PIXELS as f32 {
        return Err(TextError::TooLarge {
            width: width as u32,
            height: height as u32,
        });
    }
    let (bw, bh) = (width as usize, height as usize);
    let mut cover = vec![0u8; bw * bh];

    // One rasteriser per line, merged with a `max`.
    //
    // Per *line* rather than per glyph, because glyphs within a line genuinely
    // overlap — tight tracking, a script face's connecting strokes, a mark over
    // a letter — and separate buffers composited would show a seam where they
    // do. Per line rather than one for the whole block, because the rasteriser
    // holds an `f32` an accumulator pixel: a block at the cap would be a quarter
    // of a gigabyte of transient for a picture that is 64 MB.
    let line_box = (line_height + pad * 2.0).ceil().max(1.0) as usize;
    let mut raster = Rasterizer::new(bw, line_box);
    let outlines = font.outline_glyphs();
    for (i, line) in lines.iter().enumerate() {
        if line.glyphs.is_empty() {
            continue;
        }
        raster.reset(bw, line_box);
        let x0 = pad
            + match block.align {
                Align::Left => 0.0,
                Align::Centre => (block_width - line.width) * 0.5,
                Align::Right => block_width - line.width,
            };
        let baseline = pad + metrics.ascent;
        for glyph in &line.glyphs {
            let Some(outline) = outlines.get(GlyphId::from(glyph.id)) else {
                continue;
            };
            let mut pen = Pen {
                raster: &mut raster,
                dx: x0 + glyph.x,
                baseline: baseline - glyph.y,
                last: point(0.0, 0.0),
                start: point(0.0, 0.0),
                bounds: (bw as f32, line_box as f32),
            };
            // A glyph that will not draw is skipped rather than abandoning the
            // line: a caption missing one letter beats a caption missing all of
            // them, which is `cputext.rs`'s rule and the same one.
            let _ = outline.draw(DrawSettings::unhinted(Size::new(size), &location), &mut pen);
        }
        let dy = (i as f32 * line_height).round() as isize;
        raster.for_each_pixel_2d(|px, py, coverage| {
            if coverage <= 0.0 {
                return;
            }
            let y = py as isize + dy;
            if y < 0 || y as usize >= bh {
                return;
            }
            let at = y as usize * bw + px as usize;
            let v = (coverage.min(1.0) * 255.0 + 0.5) as u8;
            // A `max`, exactly as the dab pass composites coverage: two lines
            // whose descenders and ascenders meet must saturate at 1.0 rather
            // than compounding into a dark band. Same rule, same reason.
            cover[at] = cover[at].max(v);
        });
    }

    trim(cover, bw, bh).map(|(coverage, width, height)| Setting {
        width,
        height,
        coverage,
        missing,
        mixed_directions: mixed,
    })
}

/// One glyph, positioned in pixels relative to the start of its line.
struct Positioned {
    id: u32,
    x: f32,
    /// Up from the baseline, as font space has it.
    y: f32,
}

struct Line {
    glyphs: Vec<Positioned>,
    width: f32,
}

/// Shape one line and lay its glyphs out along the baseline.
fn shape_line(
    shaper: &harfrust::Shaper,
    text: &str,
    scale: f32,
    tracking: f32,
    missing: &mut Vec<char>,
    mixed: &mut bool,
) -> Line {
    if text.is_empty() {
        return Line {
            glyphs: Vec::new(),
            width: 0.0,
        };
    }
    *mixed |= has_mixed_directions(text);

    let mut buffer = UnicodeBuffer::new();
    buffer.push_str(text);
    // HarfBuzz's own first step, and it must be explicit here: without it the
    // buffer's direction and script are unset, and shaping a right-to-left run
    // as left-to-right is the "disconnected isolated forms" failure the module
    // docs describe.
    buffer.guess_segment_properties();
    let shaped = shaper.shape(buffer, &[]);

    let mut glyphs = Vec::with_capacity(shaped.glyph_infos().len());
    let mut pen = 0.0_f32;
    for (info, pos) in shaped
        .glyph_infos()
        .iter()
        .zip(shaped.glyph_positions().iter())
    {
        if info.glyph_id == 0 {
            // `.notdef`. The cluster is a byte index into the line, so this can
            // name the character rather than reporting a count.
            if let Some(ch) = text[info.cluster as usize..].chars().next()
                && !missing.contains(&ch)
            {
                missing.push(ch);
            }
        }
        glyphs.push(Positioned {
            id: info.glyph_id,
            x: pen + pos.x_offset as f32 * scale,
            y: pos.y_offset as f32 * scale,
        });
        pen += pos.x_advance as f32 * scale + tracking;
    }
    // The last glyph's tracking is space *after* the run, which would push a
    // centred line left by half of it and make a right-aligned one hang.
    let width = (pen - tracking).max(0.0);
    Line { glyphs, width }
}

/// Does this line hold strong characters of both directions?
///
/// Deliberately crude — the Arabic, Hebrew, Syriac and Thaana blocks against
/// everything else — because it is not deciding anything, only saying whether
/// the one thing this module cannot do might have been needed. Getting it
/// slightly wrong shows a sentence somebody can ignore; getting the *shaping*
/// wrong is text nobody can read.
fn has_mixed_directions(text: &str) -> bool {
    let rtl = |c: char| {
        matches!(c as u32,
            0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
    };
    let ltr = |c: char| c.is_alphabetic() && !rtl(c);
    text.chars().any(rtl) && text.chars().any(ltr)
}

/// Cut the block down to the pixels that actually hold ink.
///
/// What makes the transform box hug the text rather than the generous padding
/// the rasteriser needed. `None` where nothing was drawn, which is a line of
/// spaces or a line the face had no glyph for — [`TextError::NoInk`], and a
/// different sentence from "you have not typed anything".
fn trim(cover: Vec<u8>, w: usize, h: usize) -> Result<(Vec<u8>, u32, u32), TextError> {
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if cover[y * w + x] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return Err(TextError::NoInk);
    }
    let (tw, th) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity(tw * th);
    for y in y0..y1 {
        out.extend_from_slice(&cover[y * w + x0..y * w + x1]);
    }
    Ok((out, tw as u32, th as u32))
}

/// Feeds glyph outlines to the rasteriser, flipping to y-down as it goes.
///
/// The same shape `cputext::Pen` has, and deliberately not shared with it: that
/// one lives in `umber-app` because the splash paints before this crate's
/// consumers exist, and a shared version would mean `umber-app` depending on
/// `umber-core` for the one thing it draws before `umber-core` is involved.
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
    /// Clamped to the box, because `Rasterizer` indexes its accumulation buffer
    /// from these coordinates directly: an outline straying outside — which a
    /// script face's swash routinely does — would be a panic rather than a
    /// clipped pixel.
    fn at(&self, x: f32, y: f32) -> Point {
        point(
            (self.dx + x).clamp(0.0, self.bounds.0),
            (self.baseline - y).clamp(0.0, self.bounds.1),
        )
    }
}

impl OutlinePen for Pen<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        // An unclosed contour leaks coverage across the whole scanline, so the
        // start is remembered and `close` always joins back to it.
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

/// A `FontRef` a test can hold, so the tests below read like the callers do.
#[cfg(test)]
fn any_font() -> skrifa::FontRef<'static> {
    skrifa::FontRef::new(crate::fonts::TEST_FONT).expect("Archivo parses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::FontLibrary;

    fn library() -> FontLibrary {
        let mut lib = FontLibrary::default();
        lib.add_builtin("archivo", crate::fonts::TEST_FONT);
        lib
    }

    fn block(text: &str) -> TextBlock {
        TextBlock {
            text: text.to_string(),
            size: 48.0,
            ..Default::default()
        }
    }

    fn set_with(lib: &FontLibrary, style: &str, b: &TextBlock) -> Result<Setting, TextError> {
        let face = lib.resolve("Archivo", style).expect("a face");
        let data = face.load().expect("bytes");
        set(face, &data, b)
    }

    #[test]
    fn a_word_comes_out_as_coverage_trimmed_to_its_own_ink() {
        let lib = library();
        let s = set_with(&lib, "Regular", &block("Umber")).expect("ink");
        assert!(s.width > 0 && s.height > 0);
        assert_eq!(s.coverage.len(), (s.width * s.height) as usize);
        // Trimmed means the outermost row and column each hold something.
        let row = |y: u32| (0..s.width).any(|x| s.coverage[(y * s.width + x) as usize] != 0);
        let col = |x: u32| (0..s.height).any(|y| s.coverage[(y * s.width + x) as usize] != 0);
        assert!(row(0) && row(s.height - 1), "untrimmed vertically");
        assert!(col(0) && col(s.width - 1), "untrimmed horizontally");
        assert!(s.missing.is_empty(), "{:?}", s.missing);
    }

    /// Nothing typed and nothing that makes a mark are different sentences in
    /// the interface, so they are different answers here.
    #[test]
    fn an_empty_string_and_a_line_of_spaces_are_told_apart() {
        let lib = library();
        assert_eq!(set_with(&lib, "Regular", &block("")), Err(TextError::Empty));
        assert_eq!(
            set_with(&lib, "Regular", &block("   ")),
            Err(TextError::NoInk)
        );
    }

    /// The size is in document pixels, so twice the size is about twice the
    /// mark — which is what makes text as zoom-independent as a brush.
    #[test]
    fn the_size_is_the_em_in_document_pixels() {
        let lib = library();
        let small = set_with(&lib, "Regular", &block("Hxg")).unwrap();
        let mut b = block("Hxg");
        b.size = 96.0;
        let large = set_with(&lib, "Regular", &b).unwrap();
        let ratio = large.height as f32 / small.height as f32;
        assert!((ratio - 2.0).abs() < 0.1, "doubled to {ratio}×");
    }

    /// Newlines are the one thing that breaks a line — point text, which is
    /// what this stage is. A block of three lines is about three times as tall
    /// as one and no wider than its longest.
    #[test]
    fn a_newline_is_the_only_thing_that_breaks_a_line() {
        let lib = library();
        let one = set_with(&lib, "Regular", &block("Hxg")).unwrap();
        let three = set_with(&lib, "Regular", &block("Hxg\nHxg\nHxg")).unwrap();
        assert!(
            three.height > one.height * 2,
            "{} vs {}",
            three.height,
            one.height
        );
        assert!(three.width.abs_diff(one.width) <= 1);
        // And a long line does not wrap, however long it is.
        let long = set_with(&lib, "Regular", &block(&"word ".repeat(40))).unwrap();
        assert!(long.width > long.height * 10, "{long:?} looks wrapped");
    }

    /// Centring is about the *block*, so a short line beside a long one is
    /// inset on both sides rather than on one.
    #[test]
    fn alignment_moves_the_short_line_and_not_the_long_one() {
        let lib = library();
        let ink_start = |s: &Setting, y0: u32, y1: u32| -> u32 {
            (0..s.width)
                .find(|&x| (y0..y1).any(|y| s.coverage[(y * s.width + x) as usize] != 0))
                .unwrap_or(0)
        };
        let text = "Wide line here\nx";
        for (align, expect_left) in [
            (Align::Left, true),
            (Align::Centre, false),
            (Align::Right, false),
        ] {
            let mut b = block(text);
            b.align = align;
            let s = set_with(&lib, "Regular", &b).unwrap();
            let half = s.height / 2;
            let second = ink_start(&s, half, s.height);
            if expect_left {
                assert!(second < s.width / 8, "{align:?} indented the short line");
            } else {
                assert!(
                    second > s.width / 8,
                    "{align:?} left the short line at the margin"
                );
            }
        }
    }

    /// A character no face on earth carries is *named*, not silently dropped
    /// and not drawn as a box. Naming it is the whole of what this module does
    /// instead of font fallback.
    #[test]
    fn a_character_the_face_cannot_show_is_named_rather_than_hidden() {
        let lib = library();
        let s = set_with(&lib, "Regular", &block("Hi \u{E000}\u{E000} there")).expect("ink");
        assert_eq!(s.missing, vec!['\u{E000}'], "{:?}", s.missing);
    }

    /// A wholly right-to-left line is shaped as one and raises nothing; a line
    /// that mixes directions says so, because without bidi reordering the two
    /// halves come out the wrong way round.
    #[test]
    fn a_line_mixing_directions_says_so_and_one_that_does_not_stays_quiet() {
        assert!(!has_mixed_directions("Umber paints"));
        assert!(!has_mixed_directions(
            "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"
        ));
        assert!(has_mixed_directions(
            "hello \u{0645}\u{0631}\u{062D}\u{0628}\u{0627}"
        ));
        // Digits and punctuation beside Arabic are not a mixed line: they are
        // weak, and reporting them would put the sentence on nearly every line
        // of Arabic anybody types.
        assert!(!has_mixed_directions("\u{0645}\u{0631} 1234!"));
    }

    /// The shaper is doing work a charmap walk would not. Archivo kerns "AV",
    /// so the pair is narrower than the two letters set apart — which is the
    /// difference between this module and `cputext.rs`.
    #[test]
    fn glyphs_are_shaped_rather_than_merely_looked_up() {
        let font = any_font();
        let data = ShaperData::new(&font);
        let shaper = data.shaper(&font).build();
        let width = |text: &str| {
            let mut missing = Vec::new();
            let mut mixed = false;
            shape_line(&shaper, text, 1.0, 0.0, &mut missing, &mut mixed).width
        };
        let kerned = width("AV");
        let apart = width("A") + width("V");
        assert!(kerned < apart, "AV is {kerned} and A+V is {apart}");
    }

    /// Tracking is space *between* glyphs. A single glyph is unaffected, which
    /// is what stops a centred line drifting by half a step.
    #[test]
    fn tracking_moves_the_gaps_and_not_the_ends() {
        let font = any_font();
        let data = ShaperData::new(&font);
        let shaper = data.shaper(&font).build();
        let width = |text: &str, tracking: f32| {
            let mut m = Vec::new();
            let mut d = false;
            shape_line(&shaper, text, 1.0, tracking, &mut m, &mut d).width
        };
        assert!(width("ABC", 5.0) > width("ABC", 0.0));
        assert!((width("A", 5.0) - width("A", 0.0)).abs() < 0.01);
    }

    /// A variable font's named instance genuinely instances. If the location
    /// were being ignored, Bold and Regular would rasterise identically — which
    /// is exactly the bug `cputext.rs` exists to avoid on the splash.
    #[test]
    fn a_named_instance_is_actually_drawn_at_its_own_weight() {
        let lib = library();
        let ink = |style: &str| -> u64 {
            let s = set_with(&lib, style, &block("UMBER")).unwrap();
            s.coverage.iter().map(|&c| c as u64).sum()
        };
        let regular = ink("Regular");
        let bold = lib
            .family("Archivo")
            .into_iter()
            .find(|f| f.label().eq_ignore_ascii_case("bold"))
            .map(|f| f.label().to_string())
            .expect("a bold");
        assert!(
            ink(&bold) > regular * 5 / 4,
            "bold ({}) is not meaningfully heavier than regular ({regular})",
            ink(&bold)
        );
    }

    /// The colour goes on straight-alpha, and the coverage becomes the alpha —
    /// which is what makes the clip the paste path already knows how to place.
    #[test]
    fn the_clip_carries_the_coverage_as_its_alpha() {
        let lib = library();
        let s = set_with(&lib, "Regular", &block("Hxg")).unwrap();
        let clip = s
            .clip(Color::from_srgb_u8(20, 40, 60, 255))
            .expect("a clip");
        assert_eq!(clip.size().x, s.width);
        assert_eq!(clip.size().y, s.height);
        for (i, chunk) in clip.pixels().chunks_exact(4).enumerate() {
            assert_eq!(chunk[..3], [20, 40, 60], "colour at {i}");
            assert_eq!(chunk[3], s.coverage[i], "alpha at {i}");
        }
    }

    /// A colour picked at half opacity sets half-opacity text, and it does it
    /// by one multiply on the straight side — where the coverage already is.
    #[test]
    fn a_translucent_colour_thins_the_text_rather_than_being_ignored() {
        let lib = library();
        let s = set_with(&lib, "Regular", &block("Hxg")).unwrap();
        let solid = s.clip(Color::from_srgb_u8(255, 255, 255, 255)).unwrap();
        let half = s.clip(Color::from_srgb_u8(255, 255, 255, 128)).unwrap();
        let alpha = |c: &Clip| {
            c.pixels()
                .iter()
                .skip(3)
                .step_by(4)
                .map(|&a| a as u64)
                .sum::<u64>()
        };
        let (a, b) = (alpha(&solid), alpha(&half));
        assert!(
            b * 2 > a * 9 / 10 && b * 2 < a * 11 / 10,
            "{b} is not half of {a}"
        );
    }

    /// Refused with the figure rather than allocating something no canvas could
    /// hold. `Clip::place` crops to the document, so those pixels were never
    /// going to arrive.
    #[test]
    fn a_block_past_the_cap_is_refused_and_names_the_size() {
        let lib = library();
        let mut b = block(&"M".repeat(4000));
        b.size = MAX_SIZE;
        match set_with(&lib, "Regular", &b) {
            Err(TextError::TooLarge { width, height }) => {
                assert!(width as u64 * height as u64 > MAX_PIXELS);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// Two lines whose descenders and ascenders meet saturate rather than
    /// compounding into a dark band — the dab pass's `max`, for the same
    /// reason, one module along.
    #[test]
    fn overlapping_lines_saturate_rather_than_compounding() {
        let lib = library();
        let mut b = block("gggg\nHHHH");
        // Tight enough that the descenders of the first line sit inside the
        // second.
        b.line_spacing = 0.55;
        let s = set_with(&lib, "Regular", &b).unwrap();
        assert!(s.coverage.iter().any(|&c| c > 200), "nothing was drawn");
    }
}
