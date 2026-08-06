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
/// A cap rather than a hope, and the figure is set by the **largest transient**
/// rather than by the result. Three things are sized off it: the coverage is a
/// byte a pixel (16 MB), the [`Clip`] it becomes is four (64 MB), and
/// `ab_glyph_rasterizer` holds an `f32` per accumulator pixel — so a block that
/// is one enormous *line* puts the whole of it through one rasteriser at four
/// bytes a pixel (64 MB). Per-line rasterising bounds that for ordinary text
/// and does nothing at all for a single line, which is exactly the case an
/// adversarial figure would reach for.
///
/// 16 megapixels is a 4096-square block: far past any caption, and small enough
/// that the worst case above is a transient rather than an event. Text asked
/// for larger is *refused with the figure*, because the alternative is an
/// allocation nobody asked for on the way to a canvas that could not hold it
/// anyway — `Clip::place` crops to the document, so pixels beyond it were never
/// going to arrive.
pub const MAX_PIXELS: u64 = 16 << 20;

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
    /// A size, a line spacing or a tracking that is not a number.
    ///
    /// Its own variant rather than [`Self::Unreadable`], because the notice for
    /// that one names the *font* — "it may have been moved or removed since
    /// Umber found it" — and a figure that is not a figure would then have
    /// accused somebody's typeface. Not reachable from the rails, which is
    /// exactly why the wrong sentence would have survived.
    NotFinite,
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
    // **Refused before any of it is used, rather than clamped.** A NaN survives
    // `f32::clamp` — the comparisons are all false, so it comes back out
    // unchanged — and from there it reaches `as usize`, which saturates to
    // zero, and the rasteriser's own coordinates, where it is a silent
    // nothing rather than an error. The rails cannot produce one, but a
    // hand-edited figure or a later caller can, and "the Place button did
    // nothing and said nothing" is the worst shape this takes.
    if !block.size.is_finite() || !block.line_spacing.is_finite() || !block.tracking.is_finite() {
        return Err(TextError::NotFinite);
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
            // `.notdef` is **skipped, not drawn**, and it keeps its advance.
            //
            // A face's `.notdef` is usually a crossed or hollow box, and
            // stamping one onto somebody's canvas is exactly the blank-box
            // failure the interface's "no Unicode symbols" rule exists to
            // prevent — put in the picture rather than in a label, where it
            // cannot be taken back out. It also has to agree with what the
            // panel *says*: the missing characters are named in a notice, and a
            // notice reading "those characters will not appear" beside a box
            // that plainly did is the control that lies. The advance stays, so
            // what is left is a gap where the character was, which is the same
            // answer `cputext.rs` gives on the splash.
            if glyph.id == 0 {
                continue;
            }
            let Some(outline) = outlines.get(GlyphId::from(glyph.id)) else {
                continue;
            };
            let mut pen = Pen::new(
                &mut raster,
                x0 + glyph.x,
                baseline - glyph.y,
                (bw as f32, line_box as f32),
            );
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
            //
            // **`str::get`, not `text[..]`.** HarfBuzz's clusters are byte
            // offsets at grapheme boundaries and this should always be a char
            // boundary — but "should" is doing the work in that sentence, the
            // index comes out of a shaper being fed somebody's own typing, and
            // the failure mode of a direct slice is a panic on the drawing
            // path. `get` answers `None` for a boundary that is not one and for
            // an index past the end, and the only cost of being wrong is a
            // character that goes unnamed in a notice.
            if let Some(ch) = text
                .get(info.cluster as usize..)
                .and_then(|rest| rest.chars().next())
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
/// **`umber-app`'s splash uses this one too, and there is deliberately no
/// second copy.** There was, in `cputext.rs`, on the reasoning that the splash
/// paints before this crate's consumers exist and so should not depend on it —
/// which is not a reason, because `umber-app` names `umber-core` as an
/// unconditional dependency and the splash is in the same binary. What the two
/// copies actually bought was drift: the inset in [`Pen::at`] below was found
/// here and never applied there, so the splash still carries the bug this
/// comment exists to explain. One `Pen` is the same argument `blend.wgsl` makes
/// for being `concat!`ed into both passes.
///
/// `pub` for that caller. It is a rasteriser adaptor rather than a model, so it
/// has no `umber-app` in it and nothing here learns about a window.
pub struct Pen<'r> {
    raster: &'r mut Rasterizer,
    dx: f32,
    baseline: f32,
    last: Point,
    start: Point,
    bounds: (f32, f32),
}

impl<'r> Pen<'r> {
    /// Draw into `raster`, with the glyph's origin at `dx` and `baseline` and
    /// the buffer's own `(width, height)` as `bounds`.
    ///
    /// The bounds are what [`Self::at`] clamps against, so they must be the
    /// dimensions `raster` was built with rather than the region a caller
    /// happens to be interested in.
    pub fn new(raster: &'r mut Rasterizer, dx: f32, baseline: f32, bounds: (f32, f32)) -> Self {
        Self {
            raster,
            dx,
            baseline,
            last: point(0.0, 0.0),
            start: point(0.0, 0.0),
            bounds,
        }
    }
}

impl Pen<'_> {
    /// Font space is y-up from the baseline; the rasteriser is y-down from the
    /// top of its box.
    ///
    /// **Clamped two pixels inside the box, and the two is the whole of this
    /// comment.** `Rasterizer` does not panic on a point outside its buffer —
    /// `draw_line_scalar` guards a negative row start and its index macro
    /// `continue`s rather than indexing out of range — so the clamp is not
    /// about a crash. It is about the accumulator: `for_each_pixel` carries one
    /// running sum across the *whole flat buffer*, and a span deposits its
    /// closing delta one cell past where it ends. A point clamped to exactly
    /// the width therefore writes into the first cell of the **next row**,
    /// which is in bounds, so the sum for that row never returns to zero and
    /// the block gains a faint line all the way across. Two pixels in is enough
    /// that the closing delta still lands inside the row it belongs to.
    ///
    /// Passing the raw point through instead is worse rather than better: a
    /// coordinate far outside lands at `linestart + x`, which for a large `x`
    /// is a perfectly valid cell several rows further down.
    ///
    /// It is reached at all because outlines routinely stray past the advance
    /// width — a script face's swash further than most — and the padding is
    /// generous rather than infinite.
    fn at(&self, x: f32, y: f32) -> Point {
        point(
            (self.dx + x).clamp(0.0, (self.bounds.0 - 2.0).max(0.0)),
            (self.baseline - y).clamp(0.0, (self.bounds.1 - 2.0).max(0.0)),
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
    ///
    /// And it really is not drawn: a face's `.notdef` is a crossed or hollow
    /// box, and stamping one onto somebody's canvas is the blank-box failure
    /// the interface's "no Unicode symbols" rule exists to prevent, put into
    /// the picture where it cannot be taken back out. A line of nothing but
    /// missing characters therefore makes no mark at all.
    #[test]
    fn a_character_the_face_cannot_show_is_named_rather_than_hidden() {
        let lib = library();
        let s = set_with(&lib, "Regular", &block("Hi \u{E000}\u{E000} there")).expect("ink");
        assert_eq!(s.missing, vec!['\u{E000}'], "{:?}", s.missing);

        // Nothing but missing characters is a block with no ink in it, which is
        // the same answer a line of spaces gets — not a row of boxes.
        assert_eq!(
            set_with(&lib, "Regular", &block("\u{E000}\u{E001}\u{E002}")),
            Err(TextError::NoInk)
        );

        // And the gap is kept: the characters do not appear, and what is on
        // either side of them does not close up over where they were.
        let with = set_with(&lib, "Regular", &block("A\u{E000}B")).unwrap();
        let without = set_with(&lib, "Regular", &block("AB")).unwrap();
        assert!(
            with.width > without.width,
            "{} is not wider than {}",
            with.width,
            without.width
        );
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

    /// A number that is not a number is refused rather than being clamped into
    /// one. `f32::clamp` hands a NaN straight back — every comparison in it is
    /// false — and what it reaches after that is `as usize`, which saturates to
    /// zero, and the rasteriser's own coordinates, where nothing is drawn and
    /// nothing is said.
    #[test]
    fn a_size_that_is_not_a_number_is_refused_rather_than_drawn_as_nothing() {
        let lib = library();
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let mut b = block("Umber");
            b.size = bad;
            assert_eq!(set_with(&lib, "Regular", &b), Err(TextError::NotFinite));
            let mut b = block("Umber");
            b.line_spacing = bad;
            assert_eq!(set_with(&lib, "Regular", &b), Err(TextError::NotFinite));
            let mut b = block("Umber");
            b.tracking = bad;
            assert_eq!(set_with(&lib, "Regular", &b), Err(TextError::NotFinite));
        }
    }

    /// The sizes the rails cannot reach still have to be safe, because a
    /// preferences file and a later caller both can. A size below the floor is
    /// clamped up rather than dividing by zero, and a wildly negative tracking
    /// closes a line up to nothing rather than making a negative width.
    #[test]
    fn a_size_or_a_tracking_off_the_end_of_its_rail_still_sets_something() {
        let lib = library();
        for (what, mut b) in [
            ("a size of zero", {
                let mut b = block("Umber");
                b.size = 0.0;
                b
            }),
            ("a negative size", {
                let mut b = block("Umber");
                b.size = -50.0;
                b
            }),
            ("tracking far past the rail", {
                let mut b = block("Umber");
                b.tracking = -10_000.0;
                b
            }),
            ("no line spacing at all", {
                let mut b = block("Umber\nUmber");
                b.line_spacing = 0.0;
                b
            }),
        ] {
            b.text = b.text.clone();
            let out = set_with(&lib, "Regular", &b);
            assert!(
                matches!(out, Ok(_) | Err(TextError::NoInk)),
                "{what}: {out:?}"
            );
        }
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

    /// The whole claim, end to end and without a device: **placed text is a
    /// paste**. What comes out of here goes through the same `Clip::place` a
    /// Ctrl+V does, lands on the canvas, and hands back a rectangle in the
    /// layer-texture form `write_texture` wants — with no second placer, no
    /// second premultiply and nothing in `umber-render` that knows what text
    /// is.
    #[test]
    fn text_reaches_the_canvas_by_exactly_the_route_a_paste_does() {
        use glam::{UVec2, Vec2};
        let doc = UVec2::new(2048, 2048);
        let lib = library();
        let mut b = block("Umber");
        b.size = 200.0;
        let setting = set_with(&lib, "Regular", &b).expect("ink");
        let clip = setting
            .clip(Color::from_srgb_u8(255, 255, 255, 255))
            .expect("a clip");

        let placed = clip
            .place(doc, Vec2::new(1024.0, 1024.0))
            .expect("on canvas");
        // Centred on where it was asked for, whole, and in bounds.
        assert_eq!(placed.rect.width, setting.width);
        assert_eq!(placed.rect.height, setting.height);
        assert_eq!(
            placed.rect.x + placed.rect.width / 2,
            1024,
            "not centred: {:?}",
            placed.rect
        );
        assert_eq!(
            placed.pixels.len(),
            (placed.rect.width * placed.rect.height * 4) as usize
        );
        // And it is *premultiplied* on the way in, which is the one conversion
        // the paste path performs and this module must not have performed
        // already: white text at half coverage is a half-grey premultiplied
        // pixel, never white with an alpha beside it.
        // `expect`, not `if let`: an assertion inside a conditional that might
        // not hold is a test that passes having checked nothing, and this is
        // the one conversion the test exists to pin. Text this size always has
        // an antialiased edge, so a run that cannot find one has already gone
        // wrong somewhere else.
        let px = placed
            .pixels
            .chunks_exact(4)
            .find(|px| (60..=200).contains(&px[3]))
            .expect("an antialiased pixel");
        assert!(px[0] < 250, "the colour was not premultiplied: {px:?}");
    }

    /// A glyph driven outside its box leaves the rows past the clamp **clean**.
    ///
    /// This is the whole of why [`Pen::at`] insets by two rather than clamping
    /// to the buffer's own dimensions, and it is a property of the inset rather
    /// than of this glyph: no point can reach `height - 2`, a span therefore
    /// deposits nothing below `height - 3`, and every delta lands at an `x` of
    /// at most `width - 1` — inside the row it belongs to. So the last two rows
    /// can hold nothing at all.
    ///
    /// Clamped to the dimensions instead, a span that ends at exactly the width
    /// writes its closing delta at `linestart + width`, which is the first cell
    /// of the **next row** and perfectly in bounds. `for_each_pixel` carries one
    /// running sum across the whole flat buffer and takes its absolute value
    /// with no ceiling, so what is left over paints a faint line across the row
    /// underneath. Measured on Archivo's `g` at these numbers it is about a
    /// third of full coverage, right the way across.
    ///
    /// A real glyph rather than a hand-built polygon, because a rectangle's own
    /// edges happen to cancel: the artefact needs a contour whose left and
    /// right sides span different rows, which is every letter and no test
    /// shape anybody writes first. `umber-app`'s splash carried its own copy of
    /// this pen, clamped to the dimensions, until the two were made one.
    #[test]
    fn a_glyph_driven_outside_its_box_leaves_the_rows_below_it_clean() {
        // A box far too small for the size asked for, which is what a tight pad
        // and an overshooting outline come to.
        const W: usize = 16;
        const H: usize = 20;
        let font = any_font();
        let location = Location::default();
        let gid = font.charmap().map('g').expect("Archivo has a g");
        let outlines = font.outline_glyphs();
        let outline = outlines.get(gid).expect("an outline");

        let mut raster = Rasterizer::new(W, H);
        {
            let mut pen = Pen::new(&mut raster, 6.0, 14.0, (W as f32, H as f32));
            outline
                .draw(DrawSettings::unhinted(Size::new(24.0), &location), &mut pen)
                .expect("the glyph draws");
        }
        let mut rows = [0.0f32; H];
        raster.for_each_pixel_2d(|_, y, coverage| rows[y as usize] += coverage);

        assert!(
            rows.iter().sum::<f32>() > 1.0,
            "nothing was drawn, so this test proves nothing: {rows:?}"
        );
        for (y, &ink) in rows.iter().enumerate().skip(H - 2) {
            assert!(
                ink < 0.01,
                "row {y} is past the clamp and holds {ink} of coverage: {rows:?}"
            );
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
