//! Bitmap brush tips.
//!
//! A tip is an 8-bit coverage mask stamped in place of the procedural round
//! falloff. It **modulates coverage**: the dab pass still writes a single
//! channel into the stroke scratch with a `max` blend, so the wet-layer
//! invariant in `CLAUDE.md` is untouched — a tipped stroke saturates at 1.0
//! under overlap exactly as a round one does, and stroke opacity is still
//! applied once at commit.
//!
//! # Coloured stamps
//!
//! A tip may also carry a **colour per texel** — a leaf, a spatter of two hues,
//! a texture that stamps its own palette rather than the brush's. That is one
//! extra plane beside the coverage ([`TipMask::colour`]) and it changes nothing
//! about the coverage half: the mask still modulates, still saturates, and a
//! coverage-only tip is byte for byte the thing it always was.
//!
//! Where the colour goes on the GPU is the part worth knowing before reaching
//! for a second scratch texture: **there is already one**. A smudging stroke
//! records a colour per dab in `CanvasRenderer`'s colour scratch, and that
//! scratch is a *texture* — it holds a colour per fragment, and a smudging dab
//! merely happens to write one flat colour across its own footprint. So a
//! coloured tip is a third source of per-dab colour beside pickup and colour
//! modulation, it reuses the pipelines that already exist for those two, and
//! neither `composite.wgsl` nor `commit.wgsl` learns that it happened.
//!
//! The colour is **straight sRGB**, three bytes a texel, exactly as
//! [`crate::clipboard::Clip`] holds a picture and for the same reason: it is
//! what a file holds, so a PNG round trip is byte for byte. Premultiplying is
//! the renderer's, on upload, in linear light — doing it here would lose the
//! colour of every texel the stamp only half covers, and doing it in sRGB is
//! the classic way to halo an edge.
//!
//! Plain bytes, no GPU types: `umber-render` uploads one of these to an
//! `R8Unorm` texture — and to an `Rgba8UnormSrgb` one where there is a colour —
//! and the engine stays testable without a device.
//!
//! On disk a tip is an 8-bit greyscale PNG in the brush library's `tips/`
//! directory, or an 8-bit **RGBA** one where it carries a colour. See
//! [`crate::preset::UserLibrary`] for why the library is a directory rather
//! than the single RON file it used to be.

use crate::preset::PresetError;
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

/// An 8-bit coverage mask, row-major from the top-left, optionally with a
/// colour of its own.
///
/// `0` is no paint and `255` is full paint. That is the same convention GIMP's
/// `.gbr` uses, which is why the importer needs no inversion — worth stating,
/// because half the brush formats in the world use the opposite one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipMask {
    width: u32,
    height: u32,
    coverage: Vec<u8>,
    /// A **coloured stamp**: straight sRGB, three bytes a texel, row-major
    /// beside `coverage`.
    ///
    /// `None` — the overwhelming majority — is a mask that takes the palette
    /// colour, and it costs nothing at all: no plane here, no texture on the
    /// GPU, and the stroke stays on the single-attachment dab pipeline.
    ///
    /// Straight rather than premultiplied so that the bytes are the file's own,
    /// and three channels rather than four because the fourth would be the
    /// coverage written down twice — two numbers that can disagree about the
    /// edge of a stamp.
    colour: Option<Vec<u8>>,
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
        Self::build(width, height, coverage, None)
    }

    /// A **coloured stamp**: coverage as [`TipMask::new`], plus `width * height
    /// * 3` bytes of straight sRGB colour.
    ///
    /// What this buys is a tip whose pixels *are* the mark — a leaf, a spatter
    /// of two hues — rather than a silhouette the palette fills in. It reaches
    /// the canvas through the per-dab colour path a smudging brush already
    /// uses, so nothing downstream of the dab pass knows the difference; see
    /// this module's docs.
    ///
    /// The colour of a texel the stamp does not cover is kept rather than
    /// zeroed. It is what the file held, it is never read where the coverage is
    /// zero, and throwing it away would make the PNG round trip lossy for a
    /// reason nobody could see.
    pub fn coloured(
        width: u32,
        height: u32,
        coverage: Vec<u8>,
        colour: Vec<u8>,
    ) -> Result<Self, PresetError> {
        Self::build(width, height, coverage, Some(colour))
    }

    fn build(
        width: u32,
        height: u32,
        coverage: Vec<u8>,
        colour: Option<Vec<u8>>,
    ) -> Result<Self, PresetError> {
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
        if let Some(colour) = &colour
            && colour.len() != expected * 3
        {
            return Err(PresetError::Malformed(
                None,
                format!(
                    "a coloured brush tip has {} colour bytes, expected {}",
                    colour.len(),
                    expected * 3
                ),
            ));
        }
        Ok(Self {
            width,
            height,
            coverage,
            colour,
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

    /// Row-major straight sRGB colour, `width * height * 3` bytes, for a
    /// coloured stamp. `None` for the ordinary mask, which takes the palette
    /// colour.
    pub fn colour(&self) -> Option<&[u8]> {
        self.colour.as_deref()
    }

    /// A coloured stamp as the GPU wants it: RGBA8, sRGB-encoded, **alpha
    /// premultiplied in linear light**, with the coverage in the fourth byte.
    ///
    /// The same form `ImportedLayer::pixels` hands over and produced by the same
    /// encoder, which is the point of it living here rather than in
    /// `umber-render`: the conversion is arithmetic, it has an exact inverse
    /// that other things depend on, and it is testable without a device.
    ///
    /// Why premultiplied at all, when the model holds the colour straight: this
    /// is what a bilinear tap may be taken of. Filtering straight colour across
    /// the edge of a stamp pulls in the colour of texels that are not there and
    /// haloes it, and doing the multiply in sRGB rather than linear is the
    /// classic second way to get the same halo.
    pub fn colour_premultiplied(&self) -> Option<Vec<u8>> {
        let colour = self.colour.as_ref()?;
        let mut out = Vec::with_capacity(self.coverage.len() * 4);
        for (px, &a) in colour.chunks_exact(3).zip(&self.coverage) {
            out.extend_from_slice(&crate::docimport::srgb::encode_pixel([
                px[0], px[1], px[2], a,
            ]));
        }
        Some(out)
    }

    /// Whether this tip stamps a colour of its own.
    ///
    /// **This is what puts a stroke on the per-dab colour path**, alongside
    /// `Brush::colours_dabs`. The tip is a *name* the editor resolves rather
    /// than a field of `Brush`, so the two have to be combined where the tip is
    /// known — exactly as `Brush::dab_has_angle` is combined with `Editor::tip`.
    pub fn is_coloured(&self) -> bool {
        self.colour.is_some()
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

    /// Encode as an 8-bit PNG — how a tip is stored in the library.
    ///
    /// **Greyscale for a mask and RGBA for a coloured stamp**, which is the
    /// smaller of the two in each case and, more to the point, is what
    /// [`TipMask::from_png`] can read back without being told which it is: a
    /// greyscale file has one meaning and an RGBA one has the other.
    ///
    /// PNG rather than the raw bytes because it carries its own dimensions and
    /// because the files are then ordinary images: a tip can be looked at,
    /// replaced or copied between machines with anything that opens a picture,
    /// which matters for a format whose whole content is somebody else's
    /// bitmap. `png` is already a dependency of this crate for document import,
    /// so it costs nothing to build.
    pub fn to_png(&self) -> Result<Vec<u8>, PresetError> {
        let (colour_type, data) = match &self.colour {
            None => (png::ColorType::Grayscale, self.coverage.clone()),
            Some(rgb) => {
                let mut rgba = Vec::with_capacity(self.coverage.len() * 4);
                for (px, &a) in rgb.chunks_exact(3).zip(&self.coverage) {
                    rgba.extend_from_slice(&[px[0], px[1], px[2], a]);
                }
                (png::ColorType::Rgba, rgba)
            }
        };
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, self.width, self.height);
            encoder.set_color(colour_type);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder
                .write_header()
                .map_err(|e| malformed(format!("the brush tip could not be written ({e})")))?;
            writer
                .write_image_data(&data)
                .map_err(|e| malformed(format!("the brush tip could not be written ({e})")))?;
        }
        Ok(out)
    }

    /// Decode a tip written by [`TipMask::to_png`].
    ///
    /// Greyscale is a coverage mask and **RGBA is a coloured stamp**: the alpha
    /// is the coverage and the RGB is what it stamps. There is nothing to guess
    /// in either — which is exactly why plain **RGB is still refused**. A file
    /// with no alpha channel says nothing about which of its three channels the
    /// coverage is, and a wrong guess is a brush that is quietly the wrong
    /// shape; [`TipMask::from_picture`] is where a picture with no answer of its
    /// own gets one, out loud.
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
        let (coverage, colour) = match info.color_type {
            png::ColorType::Grayscale => {
                buf.truncate(texels);
                (buf, None)
            }
            // Written by some editors when the image has an alpha channel it
            // does not use. The grey is still the coverage.
            png::ColorType::GrayscaleAlpha => (
                buf[..texels * 2].chunks_exact(2).map(|px| px[0]).collect(),
                None,
            ),
            // A coloured stamp: the alpha is the coverage and the colour is
            // what it puts down. No reading to choose between, which is what
            // separates this from `from_picture`.
            png::ColorType::Rgba => {
                let px = &buf[..texels * 4];
                (
                    px.chunks_exact(4).map(|p| p[3]).collect(),
                    Some(px.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()),
                )
            }
            other => {
                return Err(malformed(format!(
                    "a brush tip must be an 8-bit greyscale or RGBA PNG, and this one is {other:?}"
                )));
            }
        };
        Self::build(info.width, info.height, coverage, colour)
    }

    /// Coverage from the **alpha** of straight-alpha, sRGB RGBA8 pixels, with
    /// no reading to decide.
    ///
    /// This is the door a tip drawn inside Umber comes through — see
    /// [`authoring_document`] — and it is separate from [`from_picture`]
    /// precisely because there is nothing here to guess. The canvas starts
    /// transparent, so its alpha *is* the paint that was laid on it: a stroke
    /// at half opacity is coverage of a half, the eraser takes coverage back
    /// off, and covering the whole canvas gives a solid stamp rather than
    /// flipping a rule under the artist.
    ///
    /// Colour is discarded, and that is the whole shape of the feature rather
    /// than a shortcut: a tip modulates coverage and the colour comes from the
    /// palette at painting time, so a mark drawn in red and a mark drawn in
    /// black are the same stamp. What varies coverage is opacity, not grey.
    pub fn from_alpha(width: u32, height: u32, rgba: &[u8]) -> Result<Self, PresetError> {
        let texels = width as usize * height as usize;
        if rgba.len() < texels * 4 {
            return Err(malformed(format!(
                "a {width}x{height} tip needs {} bytes and got {}",
                texels * 4,
                rgba.len()
            )));
        }
        let coverage = rgba[..texels * 4].chunks_exact(4).map(|px| px[3]).collect();
        Self::new(width, height, coverage)
    }

    /// Read any picture Umber can decode as a tip, and say which reading was
    /// taken.
    ///
    /// PNG goes through the document importer's decoder rather than a second
    /// one of this module's — [`from_png`](Self::from_png) is strict on purpose
    /// and reads only what Umber itself wrote into `tips/`. Everything else
    /// goes through the `image` crate, which is already carried for the flat
    /// export and covers JPEG, TIFF, GIF and BMP.
    ///
    /// A picture larger than [`TipMask::MAX_SIZE`] is **refused** rather than
    /// resampled. Downscaling would be a second resampler nothing else calls,
    /// and — worse — it would change the mask silently, so a spatter stamp
    /// would come back softer than the one on disk with nothing saying why.
    ///
    /// **A picture never imports as a coloured stamp, and that is decided
    /// rather than missing.** Umber can carry one now ([`TipMask::coloured`]),
    /// but a black-on-transparent PNG is overwhelmingly somebody's *mask* — it
    /// is how every brush pack on the internet distributes one — and reading its
    /// colour would turn a stamp that has always painted in the palette colour
    /// into one that paints black whatever the palette says. So the rule below
    /// is unchanged and a colour arrives only where the file states it is one:
    /// a `.gbr`'s depth of 4, a `.gpb`'s trailing pattern, an RGBA tip in the
    /// library. An explicit "import as a colour stamp" is a control somebody has
    /// to be offered, not a reading to take behind their back.
    pub fn from_picture(bytes: &[u8]) -> Result<(Self, TipReading), PresetError> {
        /// The eight bytes every PNG starts with.
        const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

        let (width, height, rgba) = if bytes.starts_with(&PNG_MAGIC) {
            let image =
                crate::docimport::flat::decode_png(bytes, crate::docimport::SourceFormat::Png)
                    .map_err(|e| malformed(e.to_string()))?;
            (image.size.x, image.size.y, image.rgba)
        } else {
            let decoded = image::load_from_memory(bytes)
                .map_err(|e| malformed(format!("the picture could not be read ({e})")))?
                .to_rgba8();
            (decoded.width(), decoded.height(), decoded.into_raw())
        };

        let (coverage, reading) = coverage_of(&rgba);
        Ok((Self::new(width, height, coverage)?, reading))
    }
}

/// The side of the canvas a tip is drawn on inside Umber.
///
/// 256 pixels, and the number is a compromise with a reason on each side.
/// Below about 128 a stamp cannot hold detail a large brush will show, because
/// [`crate::Brush::size`] stretches the mask over the dab and a 64-texel tip
/// painted at 400 px is eight-fold magnification of its own antialiasing.
/// Above about 512 the canvas is bigger than any mark anybody draws a stamp
/// *as*, and the mask is stored and uploaded at whatever size it was drawn —
/// a 2048² tip is four megabytes of coverage per brush and the ceiling
/// [`TipMask::MAX_SIZE`] refuses anyway. 256 is also what the shipped stamps
/// and every CC0 pack Umber vendors sit at or below.
pub const AUTHORING_SIZE: u32 = 256;

/// The document a tip is drawn on.
///
/// **Square, [`AUTHORING_SIZE`] on a side, and transparent.**
///
/// Square because a stamp is stretched over the dab's bounding square and
/// [`TipMask::aspect`] narrows it back down from its own proportions — so a
/// square canvas is the one shape that says nothing the artist did not mean.
/// Somebody who wants a chisel draws a chisel across it and the empty margin
/// costs coverage of zero, which is the exact identity.
///
/// Transparent because that is what makes the conversion unambiguous. The
/// coverage is the alpha ([`TipMask::from_alpha`]), so what is painted is what
/// stamps: ink becomes coverage, the eraser takes coverage away, opacity is the
/// dial that makes a stamp fainter, and colour is discarded because a tip does
/// not carry one. A white-backed document reading darkness was the alternative
/// — it is how Photoshop defines a brush — and it is worse here twice over: a
/// stroke of white would mean "erase" while Umber has an eraser that means
/// something else, and a stamp that genuinely covers the whole canvas is
/// indistinguishable from a blank white page.
///
/// The resolution is left at the document default. Nothing downstream reads it
/// — a mask is texels, and [`crate::Brush::size`] is what decides how large the
/// mark is in document pixels.
pub fn authoring_document() -> crate::Document {
    crate::Document::new(AUTHORING_SIZE, AUTHORING_SIZE)
        .with_background(crate::Background::Transparent)
}

/// What a tip document is being drawn for.
///
/// Carried by the tab rather than by the editor, because it belongs to *that
/// document* and several documents are open at once — a painter who opens a
/// tip canvas, goes back to their picture and comes back must find the canvas
/// still knowing what it is for.
///
/// The brush is named **by id**, for [`crate::BrushPreset::id`]'s own reason: a
/// rename must not orphan it, and a position into the merged preset list means
/// nothing after a save, a delete or an import. The display name is carried
/// beside it only so the banner has something to say — it is what the brush was
/// called when the canvas was opened, and it is deliberately not re-derived,
/// because an id that no longer resolves has no name at all and the banner
/// still has to be able to name what the user is drawing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TipTarget {
    /// [`crate::BrushPreset::id`] of the brush this stamp is for.
    pub brush: String,
    /// What that brush was called when the canvas was opened.
    pub name: String,
}

impl TipTarget {
    pub fn new(brush: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            brush: brush.into(),
            name: name.into(),
        }
    }

    /// What the tab is called. Distinct from an "Untitled n" so the strip says
    /// what the document is without anybody having to click it.
    pub fn title(&self) -> String {
        format!("Tip for {}", self.name)
    }
}

/// Which reading of a picture was taken as coverage.
///
/// A tip *is* coverage, and a picture is four channels, so something has to
/// decide. There is no single answer that is right for every file — which is
/// exactly why [`TipMask::from_png`] refuses a colour PNG outright — so the
/// rule is stated, applied to the whole image at once, and then **said out
/// loud** by whoever imported it. A tip that is quietly the wrong shape is the
/// failure this exists to avoid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TipReading {
    /// The alpha channel. The picture has transparency in it, so it was drawn
    /// on nothing and what was painted is what stamps. Colour is discarded.
    ///
    /// This is also what a `.gbr` colour stamp gives up, and what
    /// [`TipMask::from_alpha`] takes unconditionally.
    Alpha,
    /// Darkness, `1 - luminance`. The picture is opaque in every pixel, so its
    /// transparency says nothing at all about what was drawn, and ink on paper
    /// is the only other thing a stamp can mean — a scan, a photograph of a
    /// mark, a JPEG. Black stamps at full coverage and white stamps at none.
    Ink,
}

impl TipReading {
    /// One clause naming what was taken, for the sentence an import shows.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Alpha => "its transparency",
            Self::Ink => "how dark it is",
        }
    }
}

/// Rec. 709 luminance over **sRGB** components, as a fraction.
///
/// Deliberately not linearised first, for [`crate::Color::to_hsv`]'s reason:
/// "how dark does this look" is a perceptual question, and a luminance computed
/// over linear values buries everything but the highlights, so a pencil drawing
/// would import as a nearly blank stamp.
fn luminance(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

/// The mask a picture makes, and which reading produced it.
///
/// `rgba` is straight-alpha, sRGB-encoded RGBA8 — what
/// `docimport::flat::decode_png` and `image`'s `to_rgba8` both hand back, and
/// what `CanvasRenderer::export_rgba` returns.
///
/// The rule, decided **once for the whole picture** rather than per pixel: if
/// any pixel is less than fully opaque the alpha is the coverage; otherwise the
/// darkness is. Per-pixel would produce a mask with two different meanings
/// inside it — the transparent margin of a stamp reading as alpha and its solid
/// black centre reading as ink, which is the same number twice by luck and a
/// different one everywhere else.
///
/// Note that the scan is what makes this decidable at all, and it is why a tip
/// *drawn in Umber* does not come through here: a tip document is transparent
/// to begin with, so an artist who covers the whole canvas would flip the rule
/// under themselves and get their stamp inverted. That path takes
/// [`TipMask::from_alpha`], which has nothing to decide.
pub fn coverage_of(rgba: &[u8]) -> (Vec<u8>, TipReading) {
    let opaque = rgba.chunks_exact(4).all(|px| px[3] == 255);
    let coverage = if opaque {
        rgba.chunks_exact(4)
            .map(|px| (((1.0 - luminance(px[0], px[1], px[2])) * 255.0) + 0.5) as u8)
            .collect()
    } else {
        rgba.chunks_exact(4).map(|px| px[3]).collect()
    };
    (
        coverage,
        if opaque {
            TipReading::Ink
        } else {
            TipReading::Alpha
        },
    )
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

    /// A PNG of the given colour type, for the import tests. The document
    /// importer has fixtures of its own and they are private to it; one
    /// four-line encoder here is cheaper than widening that module.
    fn encode(width: u32, height: u32, colour: png::ColorType, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, width, height);
        encoder.set_color(colour);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(data).expect("data");
        drop(writer);
        out
    }

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

    /// The same promise for a coloured stamp, and it is the one that decides
    /// whether the library can hold one at all: the colour is stored straight,
    /// so what comes back is the file's own bytes rather than something that
    /// has been through a premultiply and back.
    #[test]
    fn a_coloured_stamp_survives_a_png_round_trip_byte_for_byte() {
        let coverage: Vec<u8> = (0..12).map(|i| (i * 21) as u8).collect();
        let colour: Vec<u8> = (0..36).map(|i| (i * 5 + 3) as u8).collect();
        let tip = TipMask::coloured(4, 3, coverage.clone(), colour.clone()).expect("build");
        assert!(tip.is_coloured());

        let back = TipMask::from_png(&tip.to_png().expect("encode")).expect("decode");
        assert_eq!(back.coverage(), coverage);
        assert_eq!(back.colour(), Some(colour.as_slice()));
        assert_eq!(back, tip);

        // And a mask is still written as greyscale, so nothing already in
        // somebody's `tips/` directory grows three channels it does not use.
        let plain = TipMask::new(4, 3, coverage).expect("build");
        assert!(!plain.is_coloured());
        let png = plain.to_png().expect("encode");
        assert_eq!(TipMask::from_png(&png).expect("decode"), plain);
        // Greyscale is one byte a texel plus the PNG's own overhead; RGBA is
        // four. Checked as a size rather than by parsing the header, because
        // what matters is that the file did not grow.
        assert!(png.len() < tip.to_png().expect("encode").len());
    }

    #[test]
    fn a_stamp_and_its_colour_have_to_be_the_same_size() {
        assert!(TipMask::coloured(2, 2, vec![0; 4], vec![0; 12]).is_ok());
        assert!(TipMask::coloured(2, 2, vec![0; 4], vec![0; 11]).is_err());
        assert!(TipMask::coloured(2, 2, vec![0; 4], vec![0; 16]).is_err());
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

    /// The whole of what a tip drawn in Umber is: the alpha, and nothing else.
    /// Colour must not reach the mask, or a stamp would change shape depending
    /// on what colour happened to be in the palette when it was drawn.
    #[test]
    fn a_drawn_tip_takes_its_coverage_from_the_alpha_and_ignores_the_colour() {
        // Black, white and red, all at half alpha; then nothing at all.
        let rgba = [
            0, 0, 0, 128, // black
            255, 255, 255, 128, // white
            255, 0, 0, 128, // red
            0, 0, 0, 0, // untouched canvas
        ];
        let mask = TipMask::from_alpha(4, 1, &rgba).expect("build");
        assert_eq!(mask.coverage(), [128, 128, 128, 0]);

        // A tip that covers the whole canvas is solid, not inverted. This is
        // the case that rules out reading darkness here: every pixel is opaque,
        // so the guessing rule would flip and hand back the negative.
        let solid = TipMask::from_alpha(2, 1, &[0, 0, 0, 255, 255, 255, 255, 255]).expect("build");
        assert_eq!(solid.coverage(), [255, 255]);

        // Short buffers are refused rather than read past the end.
        assert!(TipMask::from_alpha(4, 1, &[0; 8]).is_err());
    }

    /// The one guess in the module, and the reason it is stated rather than
    /// per pixel: a picture means one thing all over.
    #[test]
    fn a_picture_with_transparency_reads_as_alpha_and_an_opaque_one_as_ink() {
        // A stamp drawn on nothing: black ink at the left, clear at the right.
        let drawn = [0, 0, 0, 255, 0, 0, 0, 0];
        let (coverage, reading) = coverage_of(&drawn);
        assert_eq!(reading, TipReading::Alpha);
        assert_eq!(coverage, [255, 0]);

        // A scan: black on white, opaque throughout. Alpha says nothing, so the
        // ink is the mark.
        let scanned = [0, 0, 0, 255, 255, 255, 255, 255];
        let (coverage, reading) = coverage_of(&scanned);
        assert_eq!(reading, TipReading::Ink);
        assert_eq!(coverage, [255, 0]);

        // Mid-grey is about half coverage — checked loosely, because the
        // luminance weights are perceptual rather than a third each.
        let (coverage, _) = coverage_of(&[128, 128, 128, 255]);
        assert!(
            (coverage[0] as i32 - 127).abs() <= 2,
            "got {coverage:?} for mid-grey"
        );

        // One transparent pixel is enough to settle the whole picture, which is
        // what stops a stamp's solid centre and its clear margin being read two
        // different ways.
        let mostly_opaque = [0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0];
        assert_eq!(coverage_of(&mostly_opaque).1, TipReading::Alpha);
        assert_eq!(coverage_of(&mostly_opaque).0, [255, 255, 0]);
    }

    /// A tip imported off disk has to come back as the picture that went in,
    /// through the decoder the document importer already has rather than a
    /// second one.
    #[test]
    fn a_picture_file_imports_as_a_tip() {
        let png = encode(2, 1, png::ColorType::Rgba, &[0, 0, 0, 255, 0, 0, 0, 0]);
        let (mask, reading) = TipMask::from_picture(&png).expect("import");
        assert_eq!((mask.width(), mask.height()), (2, 1));
        assert_eq!(mask.coverage(), [255, 0]);
        assert_eq!(reading, TipReading::Alpha);
        assert!(reading.describe().contains("transparency"));

        // A greyscale PNG has no alpha channel at all, so it is a scan.
        let grey = encode(2, 1, png::ColorType::Grayscale, &[0, 255]);
        let (mask, reading) = TipMask::from_picture(&grey).expect("import");
        assert_eq!(mask.coverage(), [255, 0]);
        assert_eq!(reading, TipReading::Ink);

        assert!(TipMask::from_picture(b"not a picture").is_err());
    }

    /// The canvas a tip is drawn on. Both halves are load-bearing and both are
    /// easy to change without noticing: a background that was not transparent
    /// would make `from_alpha` read the paper as paint, and a non-square canvas
    /// would put proportions into every stamp somebody drew.
    #[test]
    fn the_tip_document_is_a_square_transparent_canvas() {
        let doc = authoring_document();
        assert_eq!(doc.size.x, doc.size.y);
        assert_eq!(doc.size.x, AUTHORING_SIZE);
        // A canvas Umber offers and then refuses to read back would be a
        // feature that fails at the last step. Checked at compile time, since
        // both sides are constants.
        const { assert!(AUTHORING_SIZE <= TipMask::MAX_SIZE) };
        assert_eq!(doc.background, crate::Background::Transparent);
    }
}
