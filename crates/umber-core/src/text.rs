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
//!
//! # Scaling a placement is a re-set, not a resample
//!
//! `Float` scales by sampling, which is right for photographic pixels and is the
//! one thing text cannot tolerate: at twice the size a caption is soft and at a
//! third of it it is mush. So [`set_through`] takes the affine the transform
//! tool is holding and rasterises the outlines **already scaled and turned**,
//! straight into document space. Every frame of a drag is a fresh sharp
//! rasterisation, the antialiasing is computed at the size the pixels will
//! actually be, and the float's own matrix has nothing left to do.
//!
//! [`set`] is that same function at [`Affine::IDENTITY`] and is the only place
//! either is written down. `docs/text-tool.md` §4(c) is the argument, including
//! the two cheaper answers it refuses; two of its details did not survive
//! contact:
//!
//! * **`skrifa`'s `DrawSettings` does not take an affine.** At 0.42.1 — the
//!   version `epaint` pins and this crate uses — it carries a size, a variation
//!   location, a scratch buffer and a path style, and nothing else. So the map
//!   goes in [`Pen`], which is where the y-flip already lives and is the same
//!   arithmetic one step later. It is emphatically **not** `Size::new(size *
//!   scale)`: that is the "drive the point size from the handle" answer §4(c)
//!   refuses, and it would change what the shapes *are* — optical sizing and a
//!   variable font's `opsz` axis both read the size — as well as having no
//!   answer for a rotation.
//! * **The block is measured before it is drawn**, by [`BoundsPen`], which walks
//!   each outline through the same map and records where the points went.
//!   [`set`] used to pad by a whole em on every side because nothing knew how
//!   far an outline strays past its advance width. That padding is 2.6x the
//!   buffer for a single line at identity and *twenty times an em* under a scale
//!   of twenty, which is the difference between a caption that re-sets inside a
//!   frame and one that does not. Measuring costs a second walk of each outline,
//!   which is O(glyphs) against a rasterisation that is O(pixels).

use crate::clipboard::Clip;
use crate::color::Color;
use crate::fonts::{Face, FontData};
use crate::geom::PixelRect;
use crate::transform::Affine;
use ab_glyph_rasterizer::{Point, Rasterizer, point};
use glam::{IVec2, UVec2, Vec2};
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

/// How far outside a block's own ink its rasterisation buffer reaches, in
/// **document** pixels.
///
/// Three, and each of them is spent: one for the antialiased edge of the
/// outermost contour, and two for [`Pen::at`]'s inset, which cannot draw in the
/// last two columns and rows of the buffer it is given.
///
/// **In document pixels rather than in the block's own space, which is the whole
/// reason it can be this small.** [`set`] used to pad by `size` — a whole em on
/// every side — because nothing measured how far an outline strays past its
/// advance width, and that is 2.6x the buffer for a single line. Under a scale
/// of twenty it would be twenty ems of *document* buffer, three quarters of it
/// empty, on the one path that has to finish inside a frame.
/// [`BoundsPen`] measures the ink instead, from the points the drawing pen will
/// itself be handed, so the only thing left to pay for is the storage detail and
/// the edge.
///
/// **The inset is why this is three and not one.** `Pen::at`'s docs say that if
/// anybody tightens the padding the inset is the first thing to reconsider.
/// This is that tightening, and the answer is to keep the inset — it is cheap
/// insurance against a rasteriser storage detail — and to pay for it here,
/// where it costs a constant rather than a proportion of the size.
/// `no_point_a_mapped_pen_is_handed_ever_reaches_its_clamp` is the guard.
///
/// **The three are not spent symmetrically, and the tidy reading of that
/// sentence is wrong.** `Pen::at` clamps to `[0, size - 2]`, so the *near* side
/// needs none of them — a point at the box's own minimum lands at 3.0 and the
/// lower bound is 0.0 — and the *far* side needs the two. Two would therefore be
/// exactly sufficient. The third is one pixel of headroom against the floor and
/// ceiling in [`Bounds::pixels`] rounding a hair the wrong way, which is cheap
/// where the alternative is a clipped contour.
const MARGIN: f32 = 3.0;

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

/// A block set through an affine, and where its ink landed.
///
/// **Not [`crate::clipboard::Placed`]**, which is straight-alpha pixels and a
/// rectangle a paste has already been resolved onto a canvas. This is coverage
/// and the place the map put it, which may be partly or wholly off the canvas —
/// [`Self::layer_rect`] is what crops it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub setting: Setting,
    /// Where the top-left of `setting` sits in the space the map maps *into*.
    ///
    /// Signed, because a block dragged off the top or the left of the canvas has
    /// ink at negative coordinates. Nothing here crops: this module has never
    /// been told how large the document is, and the two callers that know —
    /// [`Self::layer_rect`] and `Clip::place` — disagree about what to do with
    /// the overhang.
    pub at: IVec2,
}

impl Placed {
    /// The document rectangle this covers and the premultiplied layer bytes for
    /// it, in `colour`, cropped to a `doc`-sized canvas.
    ///
    /// `None` where none of it lands on the canvas at all.
    ///
    /// **Nothing calls this yet**, and the paragraph below is written in the
    /// future tense it deserves rather than the present tense it had. The caller
    /// is `app.rs`'s transform region once a text float carries the block it was
    /// set from; until then a placement goes through [`Setting::clip`] and
    /// `Clip::place` like a paste, and this saves nobody anything.
    /// `the_layer_rect_of_a_block_off_the_top_left_reads_its_own_coverage` is what
    /// stops it rotting in the meantime — the arithmetic here is the part with a
    /// signed origin in it, which is the thing that has already been wrong once
    /// in this module.
    ///
    /// **One pass, and that will not be tidiness.** The three steps it replaces —
    /// paint the coverage, premultiply it, crop it — are each a full copy of a
    /// rectangle that on a large drag is tens of megabytes, and they would run
    /// every frame. The colour is constant over the block, so what varies per pixel is
    /// one byte; a 256-entry table built from
    /// [`crate::docimport::srgb::encode_pixel`] turns the whole conversion into
    /// a lookup and keeps the *exact* arithmetic the paste path uses, rather than
    /// a second premultiply that could round differently.
    pub fn layer_rect(&self, colour: Color, doc: UVec2) -> Option<(PixelRect, Vec<u8>)> {
        let [r, g, b, a] = colour.to_srgb_u8();
        let (w, h) = (self.setting.width as i64, self.setting.height as i64);
        let x0 = self.at.x as i64;
        let y0 = self.at.y as i64;
        // The overlap of the block with the canvas, in the block's own pixels.
        let cx0 = x0.max(0);
        let cy0 = y0.max(0);
        let cx1 = (x0 + w).min(doc.x as i64);
        let cy1 = (y0 + h).min(doc.y as i64);
        if cx1 <= cx0 || cy1 <= cy0 {
            return None;
        }
        let rect = PixelRect {
            x: cx0 as u32,
            y: cy0 as u32,
            width: (cx1 - cx0) as u32,
            height: (cy1 - cy0) as u32,
        };
        let table: Vec<[u8; 4]> = (0..=255u32)
            .map(|cov| {
                crate::docimport::srgb::encode_pixel([
                    r,
                    g,
                    b,
                    ((cov * a as u32 + 127) / 255) as u8,
                ])
            })
            .collect();
        let mut pixels = Vec::with_capacity((rect.area() * 4) as usize);
        for y in cy0..cy1 {
            let row = ((y - y0) * w) as usize;
            for x in cx0..cx1 {
                let cov = self.setting.coverage[row + (x - x0) as usize];
                pixels.extend_from_slice(&table[cov as usize]);
            }
        }
        Some((rect, pixels))
    }
}

/// Set `block` in `face`.
///
/// The face is loaded by the caller — [`Face::load`] blocks on a file read, and
/// this is called from an interface that redraws — so what arrives here is
/// bytes and a location on the variable axes.
///
/// [`set_through`] at [`Affine::IDENTITY`], and deliberately not a second
/// rasteriser beside it: a scaled placement and an unscaled one differ by one
/// affine and nothing else, so two loops would be two things to keep in step
/// about every antialiased edge. The identity is the exact identity — `Pen::at`
/// applies `Affine::IDENTITY` and lands on the same point it did before there
/// was a map at all.
pub fn set(face: &Face, data: &FontData, block: &TextBlock) -> Result<Setting, TextError> {
    set_through(face, data, block, Affine::IDENTITY).map(|placed| placed.setting)
}

/// Set `block` in `face`, with every outline drawn through `map`.
///
/// `map` takes the block's own space — document pixels with the block's ink
/// starting near the origin, which is what [`set`] produces — to wherever the
/// caller wants it. For a floating placement that is `Transform::matrix`
/// composed with where the block was first put down, and the point of handing it
/// here rather than to a sampler is in the module docs: the rasteriser then
/// antialiases at the size the pixels will be.
pub fn set_through(
    face: &Face,
    data: &FontData,
    block: &TextBlock,
    map: Affine,
) -> Result<Placed, TextError> {
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

    // Where a line's glyph origins sit in the block's own space: x from the
    // alignment, y down from the top of the first line's box.
    //
    // **Not rounded to a whole row.** The old code offset each line's finished
    // coverage by `(i * line_height).round()`, because the merge was an integer
    // row shift; the baseline is now a float the pen carries, so a fractional
    // line height lands where the face asked for it instead of accumulating a
    // rounding error down a paragraph.
    let origin = |i: usize, line: &Line| -> Vec2 {
        let x = match block.align {
            Align::Left => 0.0,
            Align::Centre => (block_width - line.width) * 0.5,
            Align::Right => block_width - line.width,
        };
        Vec2::new(x, metrics.ascent + i as f32 * line_height)
    };
    let outlines = font.outline_glyphs();
    let settings = || DrawSettings::unhinted(Size::new(size), &location);

    // --- where the ink goes -------------------------------------------------
    //
    // Every outline is walked once through `map` before any of it is drawn, and
    // the block's buffer is the union of what came back. See [`MARGIN`] for what
    // that buys over the whole em of padding this used to guess with, and the
    // module docs for why it has to be measured through the map rather than
    // measured once and scaled: a rotation turns an ascender into width.
    //
    // It also makes `Pen::at`'s clamp unreachable rather than merely unlikely,
    // because the box is computed from the very points the drawing pen will be
    // handed. `no_point_a_mapped_pen_is_handed_ever_reaches_its_clamp` is that
    // claim as a test.
    let mut ink: Option<Bounds> = None;
    let mut per_line: Vec<Option<Bounds>> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        let at = origin(i, line);
        let mut here: Option<Bounds> = None;
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
            let mut pen = BoundsPen {
                map,
                dx: at.x + glyph.x,
                baseline: at.y - glyph.y,
                seen: None,
            };
            let _ = outline.draw(settings(), &mut pen);
            here = Bounds::union(here, pen.seen);
        }
        ink = Bounds::union(ink, here);
        per_line.push(here);
    }
    // Nothing was drawn: a line of spaces, or a line the face had no glyph for.
    // The same answer the trim used to give, one pass earlier.
    let Some(ink) = ink else {
        return Err(TextError::NoInk);
    };
    let Some(box_) = ink.pixels() else {
        // A map carrying an infinity or a NaN. Not reachable from a `Transform`
        // — `MIN_SCALE` keeps the matrix invertible and the angle is a real
        // number — but this is a `pub fn` taking an `Affine` from anywhere, and
        // the failure without the guard is silent: `as usize` saturates a NaN to
        // zero, so the block comes back as `NoInk` and the caller reports that
        // nothing was typed.
        return Err(TextError::NotFinite);
    };
    if box_.area() > MAX_PIXELS {
        return Err(TextError::TooLarge {
            width: box_.size.x,
            height: box_.size.y,
        });
    }
    let (bw, bh) = (box_.size.x as usize, box_.size.y as usize);
    let mut cover = vec![0u8; bw * bh];

    // One rasteriser per line, reused, merged with a `max`.
    //
    // Per *line* rather than per glyph, because glyphs within a line genuinely
    // overlap — tight tracking, a script face's connecting strokes, a mark over
    // a letter — and separate buffers composited would show a seam where they
    // do. Per line rather than one for the whole block, because the rasteriser
    // holds an `f32` an accumulator pixel: a block at the cap would be a quarter
    // of a gigabyte of transient for a picture that is 64 MB.
    //
    // A line's buffer is now its **own** mapped ink rather than the block's full
    // width, which is what keeps that bound true under a rotation: turn a
    // paragraph 45° and every line's bounding box is nearly the block's, so a
    // buffer sized to the block would be one transient per line of very nearly
    // the whole thing.
    let boxes: Vec<Option<InkRect>> = per_line
        .iter()
        .map(|b| b.and_then(Bounds::pixels))
        .collect();
    let (rw, rh) = boxes.iter().flatten().fold((1usize, 1usize), |(w, h), r| {
        (w.max(r.size.x as usize), h.max(r.size.y as usize))
    });
    let mut raster = Rasterizer::new(rw, rh);
    for ((i, line), b) in lines.iter().enumerate().zip(&boxes) {
        let Some(b) = *b else { continue };
        let at = origin(i, line);
        let (lw, lh) = (b.size.x as usize, b.size.y as usize);
        raster.reset(lw, lh);
        let corner = b.min.as_vec2();
        for glyph in &line.glyphs {
            if glyph.id == 0 {
                continue;
            }
            let Some(outline) = outlines.get(GlyphId::from(glyph.id)) else {
                continue;
            };
            let mut pen = Pen::mapped(&mut raster, at.x + glyph.x, at.y - glyph.y, map, corner);
            // A glyph that will not draw is skipped rather than abandoning the
            // line: a caption missing one letter beats a caption missing all of
            // them, which is `cputext.rs`'s rule and the same one.
            let _ = outline.draw(settings(), &mut pen);
        }
        // Both origins are signed and both are already bounded into `i32`, so
        // this difference is exact and cannot wrap.
        let dx = b.min.x as i64 - box_.min.x as i64;
        let dy = b.min.y as i64 - box_.min.y as i64;
        raster.for_each_pixel_2d(|px, py, coverage| {
            if coverage <= 0.0 {
                return;
            }
            let x = px as i64 + dx;
            let y = py as i64 + dy;
            if x < 0 || y < 0 || x as usize >= bw || y as usize >= bh {
                return;
            }
            let at = y as usize * bw + x as usize;
            let v = (coverage.min(1.0) * 255.0 + 0.5) as u8;
            // A `max`, exactly as the dab pass composites coverage: two lines
            // whose descenders and ascenders meet must saturate at 1.0 rather
            // than compounding into a dark band. Same rule, same reason.
            cover[at] = cover[at].max(v);
        });
    }

    let (coverage, width, height, offset) = trim(cover, bw, bh)?;
    Ok(Placed {
        setting: Setting {
            width,
            height,
            coverage,
            missing,
            mixed_directions: mixed,
        },
        at: box_.min + offset,
    })
}

/// An axis-aligned box being accumulated, in the space a map maps into.
#[derive(Clone, Copy, Debug)]
struct Bounds {
    min: Vec2,
    max: Vec2,
}

impl Bounds {
    fn add(&mut self, p: Vec2) {
        self.min = self.min.min(p);
        self.max = self.max.max(p);
    }

    fn union(a: Option<Bounds>, b: Option<Bounds>) -> Option<Bounds> {
        match (a, b) {
            (Some(a), Some(b)) => Some(Bounds {
                min: a.min.min(b.min),
                max: a.max.max(b.max),
            }),
            (some, None) | (None, some) => some,
        }
    }

    /// The whole pixels this covers, padded by [`MARGIN`].
    ///
    /// `None` for a box that is not a pair of numbers, and the check is written
    /// against the *padded* corners rather than against `min` and `max` so that
    /// an addition that overflows to an infinity is caught too.
    ///
    /// **[`LIMIT`](Self::pixels) is 2^24 and not `i32::MAX`, for two reasons that
    /// meet at the same number.** This used to bound at `i32::MAX as f32`, which
    /// is a claim with two holes in it:
    ///
    /// * `i32::MAX as f32` rounds *up* to 2147483648.0, one past what an `i32`
    ///   holds, so `lo.x as i32` could saturate to `i32::MAX` — and
    ///   [`Placed::at`] then adds `trim`'s offset to it with a plain `IVec2 +
    ///   IVec2`, which panics in debug and wraps in release.
    /// * [`MARGIN`] is 3.0, and one `ulp` of an f32 exceeds 6 once the magnitude
    ///   passes about 6.7e7 — so beyond there `min - MARGIN == min` exactly, the
    ///   padding this function exists to add silently evaporates, and
    ///   [`Pen::at`]'s clamp starts biting the very points the measurement was
    ///   supposed to keep inside the buffer.
    ///
    /// At 2^24 (16.7 million) an `ulp` is 2.0, so `MARGIN` still lands a whole
    /// pixel of padding on the near side and the box is still honest. It is also
    /// far past anything reachable: a coordinate that large with [`MAX_PIXELS`]'s
    /// 16-megapixel cap on the *area* means a block flung a thousand canvases
    /// away, and `MIN_SCALE` keeps a `Transform` nowhere near it.
    ///
    /// The old comment claimed "a block a hundred million pixels off the canvas
    /// is refused"; a hundred million is 1e8, the bound was 2.1e9, and it was not
    /// refused — it was the magnitude at which the margin had already gone.
    fn pixels(self) -> Option<InkRect> {
        let lo = self.min - Vec2::splat(MARGIN);
        let hi = self.max + Vec2::splat(MARGIN);
        let lo = Vec2::new(lo.x.floor(), lo.y.floor());
        let hi = Vec2::new(hi.x.ceil(), hi.y.ceil());
        if !lo.is_finite() || !hi.is_finite() {
            return None;
        }
        const LIMIT: f32 = (1 << 24) as f32;
        if lo.x < -LIMIT || lo.y < -LIMIT || hi.x > LIMIT || hi.y > LIMIT {
            return None;
        }
        Some(InkRect {
            min: IVec2::new(lo.x as i32, lo.y as i32),
            size: UVec2::new((hi.x - lo.x).max(1.0) as u32, (hi.y - lo.y).max(1.0) as u32),
        })
    }
}

/// A whole-pixel rectangle whose origin is **signed**.
///
/// Deliberately not [`PixelRect`], and that distinction was a bug rather than a
/// preference. A block's ink starts a little to the left of and above the pen's
/// own origin — a left sidebearing is often under a pixel and [`MARGIN`] then
/// takes three more off — so `min` is routinely negative for text nobody has
/// dragged anywhere, and under a map it can be negative by thousands. Squeezing
/// that into `PixelRect`'s `u32` and reading it back out as one made
/// [`Pen::mapped`]'s `corner` about four billion, so every point of the line
/// collapsed onto the clamp and the block came back [`TextError::NoInk`]:
/// `set("A")` set nothing, while `set("Umber")` worked because a capital `U` has
/// a wide enough sidebearing to keep `min` positive. Four tests caught it and it
/// is exactly the class of thing they exist for, so the type says signed now
/// instead of the arithmetic hoping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct InkRect {
    /// Top-left, in the space the map maps into.
    min: IVec2,
    size: UVec2,
}

impl InkRect {
    fn area(self) -> u64 {
        self.size.x as u64 * self.size.y as u64
    }
}

/// Records where an outline's points land under a map, drawing nothing.
///
/// It sees the **control** points, so the box is a superset of the curve: the
/// rasteriser flattens a quadratic or a cubic into points inside its own control
/// hull, so every point [`Pen`] is later handed is inside what this measured.
/// That containment is what makes `Pen::at`'s clamp unreachable, and it is why
/// this is a second walk of the outline rather than a read of
/// `GlyphMetrics::bounds` — a font with a wrong `glyf` bounding box is a real
/// thing, and trusting one would put a clamped, smeared contour on somebody's
/// canvas rather than merely a slightly loose buffer.
struct BoundsPen {
    map: Affine,
    dx: f32,
    baseline: f32,
    seen: Option<Bounds>,
}

impl BoundsPen {
    fn at(&mut self, x: f32, y: f32) {
        let p = self.map.apply(Vec2::new(self.dx + x, self.baseline - y));
        match &mut self.seen {
            Some(b) => b.add(p),
            none => *none = Some(Bounds { min: p, max: p }),
        }
    }
}

impl OutlinePen for BoundsPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.at(x, y);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.at(x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.at(cx, cy);
        self.at(x, y);
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.at(cx0, cy0);
        self.at(cx1, cy1);
        self.at(x, y);
    }

    fn close(&mut self) {}
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

/// Cut the block down to the pixels that actually hold ink, and say how far in
/// the cut started.
///
/// What makes the transform box hug the text rather than the [`MARGIN`] the
/// rasteriser needed. `Err` where nothing was drawn, which is a line of spaces
/// or a line the face had no glyph for — [`TextError::NoInk`], and a different
/// sentence from "you have not typed anything".
///
/// **The offset is why this still exists** now that the buffer is measured
/// rather than guessed. The measurement is of the *control hull*, which is a
/// superset of the curve, and the margin is three pixels on every side; so the
/// buffer is a few pixels larger than the ink in a way that varies with the
/// glyphs and the map. Trimming makes [`Setting`]'s promise — a rectangle of
/// coverage trimmed to its own ink — true whatever the map, which is what lets
/// the transform box hug a rotated block as tightly as an upright one, and the
/// offset is what keeps [`Placed::at`] pointing at the trimmed corner rather
/// than the buffer's.
#[allow(clippy::type_complexity)]
fn trim(cover: Vec<u8>, w: usize, h: usize) -> Result<(Vec<u8>, u32, u32, IVec2), TextError> {
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
    Ok((out, tw as u32, th as u32, IVec2::new(x0 as i32, y0 as i32)))
}

/// Feeds glyph outlines to the rasteriser, flipping to y-down as it goes.
///
/// **`umber-app`'s splash uses this one too, and there is deliberately no
/// second copy.** There was, in `cputext.rs`, on the reasoning that the splash
/// paints before this crate's consumers exist and so should not depend on it —
/// which is not a reason, because `umber-app` names `umber-core` as an
/// unconditional dependency and the splash is in the same binary. What the two
/// copies actually bought was drift: the inset in [`Pen::at`] below was applied
/// here and never there, so the two rasterised the same outline differently.
/// One `Pen` is the same argument `blend.wgsl` makes for being `concat!`ed into
/// both passes.
///
/// Which of the two was *right* turned out to be a separate question from
/// whether they should be one, and [`Pen::at`] now answers it honestly rather
/// than assuming the fixed copy was the fixed one.
///
/// `pub` for that caller. It is a rasteriser adaptor rather than a model, so it
/// has no `umber-app` in it and nothing here learns about a window.
pub struct Pen<'r> {
    raster: &'r mut Rasterizer,
    dx: f32,
    baseline: f32,
    /// Applied after the y-flip, so a point reaches the rasteriser already
    /// scaled and turned. [`Affine::IDENTITY`] for [`Self::new`], where it is
    /// the exact identity rather than an approximation of one.
    map: Affine,
    /// Where the rasteriser's own `(0, 0)` sits in the space `map` maps into.
    /// Zero for [`Self::new`], whose buffer *is* that space.
    corner: Vec2,
    last: Point,
    start: Point,
    bounds: (f32, f32),
}

impl<'r> Pen<'r> {
    /// Draw into `raster`, with the glyph's origin at `dx` and `baseline`.
    ///
    /// **The bounds are read off `raster` rather than passed in.** They are
    /// what [`Self::at`] clamps against, so they have to be the dimensions the
    /// rasteriser was actually built with and not the region a caller happens
    /// to be interested in — and this is now a `pub` type with a caller in
    /// another crate, where "must be" is a discipline and `dimensions()` is a
    /// fact. It also removes the third statement of `(width, height)` in
    /// [`set`], which already says it to `Rasterizer::new` and to `reset`.
    pub fn new(raster: &'r mut Rasterizer, dx: f32, baseline: f32) -> Self {
        Self::mapped(raster, dx, baseline, Affine::IDENTITY, Vec2::ZERO)
    }

    /// The same, with every point taken through `map` and then measured from
    /// `corner`.
    ///
    /// This is the whole of how a placement is scaled and turned without being
    /// resampled: the outline is drawn where it will finally *be*, so the
    /// rasteriser antialiases at that size. `corner` is where the buffer's own
    /// origin sits in the mapped space, which is what lets a line be rasterised
    /// into a buffer no larger than its own ink.
    ///
    /// [`Self::new`] is this at the identity and at the origin, and the
    /// arithmetic reduces to exactly what it was before there was a map —
    /// `Mat2::IDENTITY * p + 0 - 0` is `p`, in every bit.
    pub fn mapped(
        raster: &'r mut Rasterizer,
        dx: f32,
        baseline: f32,
        map: Affine,
        corner: Vec2,
    ) -> Self {
        let (w, h) = raster.dimensions();
        Self {
            raster,
            dx,
            baseline,
            map,
            corner,
            last: point(0.0, 0.0),
            start: point(0.0, 0.0),
            bounds: (w as f32, h as f32),
        }
    }
}

impl Pen<'_> {
    /// Font space is y-up from the baseline; the rasteriser is y-down from the
    /// top of its box.
    ///
    /// **Clamped two pixels inside the box.** `Rasterizer` does not panic on a
    /// point outside its buffer — `draw_line_scalar` guards a negative row
    /// start and its index macro `continue`s rather than indexing out of
    /// range — so this is not about a crash. Passing the raw point through is
    /// nonetheless worse rather than better: a coordinate far outside lands at
    /// `linestart + x`, which for a large `x` is a perfectly valid cell several
    /// rows further down. Something has to clamp.
    ///
    /// What the **two** buys is that a span's closing delta stays in the row it
    /// belongs to. `for_each_pixel` carries one running sum across the whole
    /// flat buffer and a span deposits that delta one cell past where it ends,
    /// so a point clamped to exactly the width writes into the first cell of
    /// the *next row*. `the_pen_keeps_every_point_two_pixels_inside_the_buffer`
    /// pins the property.
    ///
    /// **What it does not buy is a cleaner picture, and that was measured after
    /// this comment claimed otherwise.** It used to say that clamping to the
    /// width left a row's sum non-zero and "the block gains a faint line all
    /// the way across". It does not. The displaced delta lands at `(0, row+1)`
    /// and the prefix sum cancels it in that one cell before the pixel is
    /// reported, so the row below comes out clean either way. Sweeping 63
    /// glyphs over eight widths, four heights, seven offsets and four baselines
    /// against the same glyph drawn in a buffer nothing clamps in, the worst
    /// spurious pixel is 1.047 **with** the inset and 1.047 without it, and the
    /// worst spurious total on a row the glyph does not touch at all is 2.90
    /// with and 3.12 without. The artefact is a property of clamping a contour
    /// at all — a clamped outline is a different outline — not of where it is
    /// clamped to.
    ///
    /// The measured *cost*, on the other hand, is real: two columns and two
    /// rows of the buffer cannot be drawn in. On Archivo's `g` at 24 px in a
    /// 16x20 box that is 22.9 of coverage truncated, against 0.06 for the
    /// un-inset clamp. **Both callers pay for it rather than risking it, and
    /// they now pay differently.** `cputext::draw` still pads by `ppem`, which
    /// is the generous-and-reasoned-about answer the sentence above described.
    /// [`set_through`] no longer pads at all: it *measures*, through
    /// [`BoundsPen`] and with [`MARGIN`] left over, so what used to be an em of
    /// slack is three pixels of it. That tightening is the one this comment
    /// warned would make the inset worth reconsidering, and the answer was to
    /// keep it and put two of `MARGIN`'s three pixels aside for it — a constant
    /// rather than a proportion of the size, which is what makes it affordable
    /// at a scale of twenty.
    ///
    /// So with a map in force the clamp is **unreachable rather than merely
    /// unlikely**: the buffer was sized from the very points this is about to be
    /// handed, by the same arithmetic, and the control hull [`BoundsPen`]
    /// records is a superset of the flattened curve.
    /// `no_point_a_mapped_pen_is_handed_ever_reaches_its_clamp` is that claim as
    /// a test. It stays anyway, because `cputext::draw` reasons its way to
    /// safety rather than measuring it, and because a clamp that never fires
    /// costs two comparisons.
    fn at(&self, x: f32, y: f32) -> Point {
        let p = self.map.apply(Vec2::new(self.dx + x, self.baseline - y)) - self.corner;
        point(
            p.x.clamp(0.0, (self.bounds.0 - 2.0).max(0.0)),
            p.y.clamp(0.0, (self.bounds.1 - 2.0).max(0.0)),
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

    /// Every point the pen hands the rasteriser is two pixels inside the
    /// buffer, whatever the outline asked for.
    ///
    /// This is what [`Pen::at`]'s inset actually guarantees, and it is stated
    /// against `at` rather than against a picture on purpose. The test that
    /// used to sit here rasterised Archivo's `g` into a box too small for it
    /// and asserted the bottom rows were clean — which they were, **because
    /// the clamp had cut the descender off**. It passed for the wrong reason
    /// and it failed on the un-inset version for the wrong reason, so it was
    /// evidence of nothing. Measured against the same glyph in a buffer nothing
    /// clamps in, the un-inset version is the *more* faithful of the two.
    ///
    /// What is left is a real invariant with a real consequence — a span's
    /// closing delta lands at most at `width`, so it cannot reach the next
    /// row's storage — and it is the only thing here worth pinning. A
    /// degenerate buffer clamps everything to the origin rather than going
    /// negative, which is the other half of `max(0.0)`.
    #[test]
    fn the_pen_keeps_every_point_two_pixels_inside_the_buffer() {
        let mut raster = Rasterizer::new(16, 20);
        let pen = Pen::new(&mut raster, 6.0, 14.0);
        for x in [-1e6, -3.0, -0.5, 0.0, 7.5, 14.0, 15.9, 100.0, 1e6] {
            for y in [-1e6, -20.0, -3.5, 0.0, 5.0, 13.9, 100.0, 1e6] {
                let p = pen.at(x, y);
                assert!(
                    (0.0..=14.0).contains(&p.x) && (0.0..=18.0).contains(&p.y),
                    "({x}, {y}) left the box at {p:?}"
                );
            }
        }
        // The point of the two: a span can never close at `width`, which is
        // the first cell of the next row in the rasteriser's flat buffer.
        assert!(pen.at(1e6, 0.0).x < 16.0);

        // A buffer too small for the inset collapses to the origin rather than
        // producing a negative bound for `clamp`, which panics.
        let mut tiny = Rasterizer::new(1, 1);
        let pen = Pen::new(&mut tiny, 0.0, 0.0);
        assert_eq!((pen.at(50.0, -50.0).x, pen.at(50.0, -50.0).y), (0.0, 0.0));
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

    // --- the affine path ---------------------------------------------------
    //
    // Everything below is [`set_through`] with a map that is not the identity,
    // which is the whole point of the module's second entry point and had no
    // test at all until the four the identity path already had started failing.

    fn set_map(lib: &FontLibrary, b: &TextBlock, map: Affine) -> Result<Placed, TextError> {
        let face = lib.resolve("Archivo", "Regular").expect("a face");
        let data = face.load().expect("bytes");
        set_through(face, &data, b, map)
    }

    fn scale(s: f32) -> Affine {
        Affine {
            m: glam::Mat2::from_diagonal(Vec2::splat(s)),
            t: Vec2::ZERO,
        }
    }

    /// **Scaling a placement is a re-set, not a resample**, and this is that
    /// sentence as an assertion: a block set through a scale of `s` is the same
    /// picture as the same block set at `s` times the point size.
    ///
    /// It is the strongest form the claim can take here. A bilinear resample
    /// would agree about the *size* and disagree about every antialiased edge —
    /// its fringe is `s` pixels wide where a fresh rasterisation's is one — so
    /// comparing dimensions alone would pass under the bug this exists to
    /// prevent. What pins the sharpness is the fringe count: the proportion of
    /// partly covered pixels has to *fall* as the block grows, because a fringe
    /// is a perimeter and the ink is an area.
    ///
    /// Exactness is not available and is not wanted: `skrifa` is asked for the
    /// outline at the block's own size and the points are multiplied afterwards,
    /// where setting at `size * s` asks for it at the larger size. For a face
    /// with no `opsz` axis those differ only by float rounding — which is
    /// precisely why driving the point size from the handle looks equivalent and
    /// is not, for a face that *has* one.
    ///
    /// **The metric proves it discriminates rather than being trusted to.** The
    /// test resamples the identity block itself and checks that the bound it
    /// asserts on the rasterised one *fails* for the resampled one. A sharpness
    /// measure nobody has shown a blurred picture to is a number, not a test —
    /// this module has already shipped one guard that passed for the wrong
    /// reason, and the fringe fraction was invented here rather than borrowed.
    #[test]
    fn a_block_set_through_a_scale_is_rasterised_at_that_size_rather_than_resampled() {
        let lib = library();
        let fringe = |s: &Setting| -> f64 {
            let ink = s.coverage.iter().filter(|&&c| c != 0).count();
            let edge = s
                .coverage
                .iter()
                .filter(|&&c| (20..=235).contains(&c))
                .count();
            edge as f64 / ink.max(1) as f64
        };
        // What `Float` does today, and the thing this whole path exists to stop
        // happening to text: a bilinear magnification of the small picture.
        let resample = |src: &Setting, s: f32| -> Setting {
            let (w, h) = (
                (src.width as f32 * s).round().max(1.0) as u32,
                (src.height as f32 * s).round().max(1.0) as u32,
            );
            let tap = |x: i64, y: i64| -> f32 {
                let x = x.clamp(0, src.width as i64 - 1) as usize;
                let y = y.clamp(0, src.height as i64 - 1) as usize;
                src.coverage[y * src.width as usize + x] as f32
            };
            let mut coverage = Vec::with_capacity((w * h) as usize);
            for y in 0..h {
                for x in 0..w {
                    let sx = (x as f32 + 0.5) / s - 0.5;
                    let sy = (y as f32 + 0.5) / s - 0.5;
                    let (fx, fy) = (sx.floor(), sy.floor());
                    let (tx, ty) = (sx - fx, sy - fy);
                    let (x0, y0) = (fx as i64, fy as i64);
                    let top = tap(x0, y0) * (1.0 - tx) + tap(x0 + 1, y0) * tx;
                    let bot = tap(x0, y0 + 1) * (1.0 - tx) + tap(x0 + 1, y0 + 1) * tx;
                    coverage.push((top * (1.0 - ty) + bot * ty).round() as u8);
                }
            }
            Setting {
                width: w,
                height: h,
                coverage,
                missing: Vec::new(),
                mixed_directions: false,
            }
        };
        let base = block("Umber");
        let at_identity = set_map(&lib, &base, Affine::IDENTITY).expect("ink");
        for s in [2.0_f32, 5.0, 12.0] {
            let mapped = set_map(&lib, &base, scale(s)).expect("ink");
            let mut bigger = base.clone();
            bigger.size = base.size * s;
            let reset = set_with(&lib, "Regular", &bigger).expect("ink");
            // The same picture, to within the rounding above and the one pixel
            // the trim's floor and ceiling can differ by.
            assert!(
                mapped.setting.width.abs_diff(reset.width) <= 2
                    && mapped.setting.height.abs_diff(reset.height) <= 2,
                "scale {s}: {}x{} through the map, {}x{} re-set",
                mapped.setting.width,
                mapped.setting.height,
                reset.width,
                reset.height
            );
            // And it is sharp: the fringe is a perimeter, so it thins as the
            // area grows. A resample's fringe would grow with the scale and
            // this ratio would hold roughly constant.
            let bound = fringe(&at_identity.setting) * 0.8;
            assert!(
                fringe(&mapped.setting) < bound,
                "scale {s}: fringe {:.3} against {:.3} at identity — resampled?",
                fringe(&mapped.setting),
                fringe(&at_identity.setting)
            );
            // And the bound has teeth: the resampled picture of the same block
            // at the same size does not clear it.
            let blurred = resample(&at_identity.setting, s);
            assert!(
                fringe(&blurred) >= bound,
                "scale {s}: a bilinear magnification scored {:.3} against a \
                 bound of {bound:.3}, so this assertion proves nothing",
                fringe(&blurred)
            );
        }
    }

    /// [`Affine::IDENTITY`] moves no point **in the last bit**, which is what
    /// lets [`set`] be [`set_through`] rather than a second rasteriser beside it.
    ///
    /// **Stated against the arithmetic, because the obvious version of this test
    /// cannot fail.** Comparing `set(…)` with `set_through(…, IDENTITY)` is
    /// comparing a one-line wrapper with the thing it wraps — the same call with
    /// the same arguments — so it holds for every possible body of
    /// `set_through`, including a badly broken one. That is exactly the guard
    /// that passes for the wrong reason this module has already thrown one of
    /// out for, and it was the first version of this test.
    ///
    /// What is actually load-bearing is that [`Pen::at`]'s extra arithmetic —
    /// `map.apply(p) - corner` where the map is the identity and the corner is
    /// zero — is bit-for-bit the bare `p` it replaced. `Mat2::IDENTITY * p` is a
    /// pair of multiply-adds by exactly 1.0 and 0.0, so it is exact for every
    /// finite input including subnormals; this pins that rather than trusting it,
    /// over the awkward values as well as the ordinary ones.
    ///
    /// **There is exactly one exception and it is the sign of zero.** `-0.0`
    /// comes back as `+0.0`, because the identity is a sum rather than a copy and
    /// `-0.0 + 0.0` is `+0.0` by IEEE 754. This test found that, which is the
    /// right way round, and it is provably harmless rather than tolerated: the
    /// two zeroes compare equal under every operator downstream — `clamp`, the
    /// `min`/`max` in [`Bounds::add`], and the rasteriser's own comparisons and
    /// multiplications — so no coverage byte can differ. It is worth recording
    /// because "the exact identity" appears three times in this module's docs and
    /// is, at the bit level, very slightly less than true.
    #[test]
    fn setting_through_the_identity_moves_no_point_at_all() {
        for p in [
            Vec2::ZERO,
            Vec2::new(1.0, -1.0),
            Vec2::new(0.1, 12345.678),
            Vec2::new(-4096.5, 1e-30),
            Vec2::new(f32::MIN_POSITIVE, -f32::MIN_POSITIVE),
            Vec2::new(1e30, -1e30),
        ] {
            let through = Affine::IDENTITY.apply(p) - Vec2::ZERO;
            assert_eq!(
                through.to_array().map(f32::to_bits),
                p.to_array().map(f32::to_bits),
                "the identity moved {p:?}"
            );
        }
        // The one exception, asserted rather than skipped: a negative zero comes
        // back positive, and nothing else about it changes.
        let z = Affine::IDENTITY.apply(Vec2::new(-0.0, -0.0)) - Vec2::ZERO;
        assert_eq!(
            z.to_array().map(f32::to_bits),
            [0, 0],
            "not a positive zero"
        );
        assert_eq!(z, Vec2::new(-0.0, -0.0), "and it still compares equal");
        // And the composition the two entry points actually differ by: the pen
        // hands the rasteriser the same point either way. `Pen::new` is
        // `Pen::mapped` at the identity and at the origin, so this reads both
        // through the real `at` rather than restating what it computes.
        let mut raster = Rasterizer::new(64, 64);
        let bare = Pen::new(&mut raster, 6.0, 14.0);
        let points: Vec<Point> = [(0.0, 0.0), (11.5, -3.25), (30.0, 9.75), (-2.5, 0.125)]
            .iter()
            .map(|&(x, y)| bare.at(x, y))
            .collect();
        let mut raster = Rasterizer::new(64, 64);
        let mapped = Pen::mapped(&mut raster, 6.0, 14.0, Affine::IDENTITY, Vec2::ZERO);
        for (i, &(x, y)) in [(0.0, 0.0), (11.5, -3.25), (30.0, 9.75), (-2.5, 0.125)]
            .iter()
            .enumerate()
        {
            let got = mapped.at(x, y);
            assert_eq!(
                (got.x.to_bits(), got.y.to_bits()),
                (points[i].x.to_bits(), points[i].y.to_bits()),
                "the mapped pen moved ({x}, {y})"
            );
        }
    }

    /// A rotation turns an ascender into width, so the box has to be measured
    /// *through* the map rather than measured once and scaled — and the ink has
    /// to land where the map put it.
    #[test]
    fn a_block_set_through_a_rotation_is_measured_through_it() {
        let lib = library();
        let b = block("Umber");
        let upright = set_map(&lib, &b, Affine::IDENTITY).expect("ink");
        let turned = set_map(
            &lib,
            &b,
            Affine {
                m: glam::Mat2::from_angle(std::f32::consts::FRAC_PI_2),
                t: Vec2::ZERO,
            },
        )
        .expect("ink");
        // A quarter turn swaps the axes, to within the trim's rounding.
        assert!(
            turned.setting.width.abs_diff(upright.setting.height) <= 2
                && turned.setting.height.abs_diff(upright.setting.width) <= 2,
            "{}x{} is not {}x{} turned",
            turned.setting.width,
            turned.setting.height,
            upright.setting.width,
            upright.setting.height
        );
        // And it went where the map sent it: the block's own space has its ink
        // at positive x, so a quarter turn about the origin puts it at negative
        // x. `Placed::at` is signed for exactly this.
        assert!(
            turned.at.x < 0,
            "a quarter turn left the ink at x = {}",
            turned.at.x
        );
    }

    /// Every map a `Transform` can hold still sets the block.
    ///
    /// This is the sweep that would have caught the signed-origin bug on the
    /// spot: `InkRect`'s origin is negative for most text and was being carried
    /// in a `u32`, so [`Pen::mapped`]'s `corner` came out at about four billion,
    /// every point was clamped onto the buffer's own origin and the whole block
    /// came back [`TextError::NoInk`]. Nothing here asserts a picture — the
    /// tests above do that — only that a map does not make the module answer
    /// "you typed nothing that makes a mark" about text that plainly does.
    #[test]
    fn every_map_a_transform_can_hold_still_sets_the_block() {
        let lib = library();
        // Short strings and left sidebearings under a pixel, which is what puts
        // the measured origin below zero.
        for text in ["A", "x", "Umber", "gj", "Wide line here\nx"] {
            let b = block(text);
            for s in [0.2_f32, 0.5, 1.0, 3.0, 20.0, -1.0] {
                for turn in [0.0_f32, 0.4, 1.7, -2.9] {
                    let map = Affine {
                        m: glam::Mat2::from_angle(turn) * glam::Mat2::from_diagonal(Vec2::splat(s)),
                        t: Vec2::new(-500.0 * s, 37.0),
                    };
                    let out = set_map(&lib, &b, map);
                    assert!(
                        matches!(out, Ok(_) | Err(TextError::TooLarge { .. })),
                        "{text:?} at scale {s} turned {turn}: {:?}",
                        out.map(|p| (p.setting.width, p.setting.height))
                    );
                }
            }
        }
    }

    /// [`Placed::layer_rect`] crops to the canvas and reads its **own** coverage
    /// while doing it, for a block hanging off the top left.
    ///
    /// The one piece of arithmetic in this module with a signed origin in it that
    /// is not exercised by anything else, on the function nothing calls yet. A
    /// block dragged off the top or the left has `at` negative, so the row index
    /// is `(y - y0) * w` for a negative `y0` — get that wrong by the sign and it
    /// reads somebody else's row, or panics, which is precisely the shape of the
    /// bug that made four tests fail on this branch.
    ///
    /// Driven against a hand-built [`Setting`] whose coverage is `x + y * w`, so
    /// every pixel names its own position and a misindexed read is a wrong
    /// *value* rather than a plausible one.
    #[test]
    fn the_layer_rect_of_a_block_off_the_top_left_reads_its_own_coverage() {
        let (w, h) = (10u32, 6u32);
        let setting = Setting {
            width: w,
            height: h,
            coverage: (0..w * h).map(|i| (i % 251) as u8).collect(),
            missing: Vec::new(),
            mixed_directions: false,
        };
        let doc = UVec2::new(8, 5);
        let white = Color::from_srgb_u8(255, 255, 255, 255);

        // Hanging off the top left by (3, 2), and off the right by 5.
        let placed = Placed {
            setting: setting.clone(),
            at: IVec2::new(-3, -2),
        };
        let (rect, pixels) = placed.layer_rect(white, doc).expect("some of it lands");
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (0, 0, 7, 4),
            "the crop is wrong: {rect:?}"
        );
        assert_eq!(pixels.len(), (rect.area() * 4) as usize);
        // Every pixel is the coverage of the block cell it came from, taken
        // through the same table the function builds. Reading the alpha is
        // enough: it is linear 8-bit and the colour is white, so the alpha *is*
        // the coverage the block held there.
        for row in 0..rect.height {
            for col in 0..rect.width {
                let block_x = col as i64 - placed.at.x as i64;
                let block_y = row as i64 - placed.at.y as i64;
                let cov = setting.coverage[(block_y * w as i64 + block_x) as usize];
                let got = pixels[((row * rect.width + col) * 4 + 3) as usize];
                let want = crate::docimport::srgb::encode_pixel([255, 255, 255, cov])[3];
                assert_eq!(got, want, "at ({col}, {row}) the block held {cov}");
            }
        }

        // Wholly off the canvas is `None` rather than an empty rectangle, on both
        // axes and in both directions.
        for at in [
            IVec2::new(-(w as i32), 0),
            IVec2::new(0, -(h as i32)),
            IVec2::new(doc.x as i32, 0),
            IVec2::new(0, doc.y as i32),
        ] {
            assert!(
                Placed {
                    setting: setting.clone(),
                    at,
                }
                .layer_rect(white, doc)
                .is_none(),
                "a block at {at:?} is not on a {doc:?} canvas"
            );
        }
    }

    /// No point a mapped pen is handed ever reaches its clamp.
    ///
    /// Named by [`MARGIN`] and by [`Pen::at`], and it is two claims because the
    /// guarantee is a composition of two:
    ///
    /// * [`Pen::at`] and [`BoundsPen::at`] compute the **identical** point, and
    ///   `at`'s clamp is the identity on it — read through the real `at`, into a
    ///   buffer sized from the measurement exactly as [`set_through`] sizes it.
    ///   Both halves at once, which is the only way either is worth asserting.
    /// * [`Bounds::pixels`] leaves [`MARGIN`] whole pixels outside the box on
    ///   every side, and `MARGIN` is three where the clamp bites at two.
    ///
    /// Together: every control point lands at least 2 inside the buffer and at
    /// most `size - 2` into it, so `clamp` is the identity on all of them. The
    /// flattened curve is inside the control hull, so it follows for every point
    /// the rasteriser actually sees.
    ///
    /// **This is not true for a coordinate past about 6.7e7**, and the limit is
    /// f32 rather than the logic: `ulp` exceeds 6 there, so `min - MARGIN == min`
    /// and `max + MARGIN == max`, the margin evaporates and the far point lands
    /// exactly on the clamp. No `Transform` reaches it — [`Bounds::pixels`]
    /// refuses past 2^24 for this reason — and the sweep below is well inside.
    #[test]
    fn no_point_a_mapped_pen_is_handed_ever_reaches_its_clamp() {
        let maps = [
            Affine::IDENTITY,
            scale(0.3),
            scale(17.0),
            Affine {
                m: glam::Mat2::from_angle(1.1) * glam::Mat2::from_diagonal(Vec2::new(4.0, -2.0)),
                t: Vec2::new(-90.0, 250.0),
            },
        ];
        // The points a glyph's outline might hand either pen, in font units.
        let outline = [(0.0, 0.0), (11.5, -3.25), (-40.0, 900.0), (300.0, 42.0)];
        for map in maps {
            // Half of it, and it must go **through `Pen::at`**. An earlier
            // version of this test re-typed `at`'s body into the assertion and
            // compared that against `BoundsPen` — which passes for any body
            // `Pen::at` might have, and is therefore a test of the transcription.
            // The way to reach the real `at` is to give the rasteriser the buffer
            // the real caller gives it: measure first, size the raster from the
            // measurement, pass the measured corner. That is what `set_through`
            // does, so this is the real composition rather than a stand-in, and
            // it lets the clamp be *exercised* instead of stepped around.
            for (dx, baseline) in [(0.0, 0.0), (3.5, 40.0), (-12.0, 7.25)] {
                let mut bp = BoundsPen {
                    map,
                    dx,
                    baseline,
                    seen: None,
                };
                for &(x, y) in &outline {
                    bp.at(x, y);
                }
                let box_ = bp.seen.expect("points").pixels().expect("a box");
                let corner = box_.min.as_vec2();
                let mut raster = Rasterizer::new(box_.size.x as usize, box_.size.y as usize);
                let pen = Pen::mapped(&mut raster, dx, baseline, map, corner);

                let mut each = BoundsPen {
                    map,
                    dx,
                    baseline,
                    seen: None,
                };
                for &(x, y) in &outline {
                    each.seen = None;
                    each.at(x, y);
                    let want = each.seen.expect("a point").max - corner;
                    let got = pen.at(x, y);
                    // The two pens agree, and the clamp did not bite: `at`
                    // returns the *unclamped* mapped point, which is only true
                    // because the buffer was sized from these very points.
                    assert_eq!(
                        (got.x, got.y),
                        (want.x, want.y),
                        "the clamp bit, or the pens disagree, at ({x}, {y}) under {map:?}"
                    );
                    // Said again in the clamp's own terms, so a future change to
                    // `at` that clamped differently cannot pass by agreeing with
                    // a differently-measured box.
                    let size = box_.size.as_vec2();
                    assert!(
                        got.x > 0.0 && got.y > 0.0 && got.x < size.x - 2.0 && got.y < size.y - 2.0,
                        "({x}, {y}) landed at {got:?}, on the clamp of {size:?}"
                    );
                }
            }
            // The other half: the box the measurement produces keeps `MARGIN`
            // on every side, so nothing inside it can reach `size - 2`.
            for (min, max) in [
                (Vec2::new(0.4, 12.0), Vec2::new(30.0, 40.0)),
                (Vec2::new(-800.5, -3.25), Vec2::new(-1.0, 0.0)),
                (Vec2::new(5.0, 5.0), Vec2::new(5.0, 5.0)),
            ] {
                let r = Bounds { min, max }.pixels().expect("a box");
                let lo = min - r.min.as_vec2();
                let hi = max - r.min.as_vec2();
                let size = r.size.as_vec2();
                assert!(
                    lo.x >= MARGIN - 1.0 && lo.y >= MARGIN - 1.0,
                    "{min:?} sits {lo:?} into a box that should hold it {MARGIN} in"
                );
                assert!(
                    hi.x <= size.x - (MARGIN - 1.0) && hi.y <= size.y - (MARGIN - 1.0),
                    "{max:?} at {hi:?} reaches the far edge of {size:?}"
                );
                // Which is the clamp's own bound, stated in its own terms.
                assert!(lo.x > 0.0 && hi.x < size.x - 2.0 && lo.y > 0.0 && hi.y < size.y - 2.0);
            }
        }
    }
}
