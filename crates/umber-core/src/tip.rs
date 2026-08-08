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

    /// Build a mask from coverage that may be larger than [`MAX_SIZE`],
    /// reducing it to fit and saying whether it had to.
    ///
    /// [`from_picture`](Self::from_picture) **refuses** an oversized picture
    /// and must go on refusing it: there the alternative is the file on disk,
    /// so a stamp that came back softer than the one the artist chose, with
    /// nothing saying why, would be the quiet failure that rule exists to
    /// prevent. This is the opposite case. It is for a format that carries a
    /// small preview beside the real pixels —
    /// [`crate::brushimport::clipstudio`] — where refusing means falling back
    /// to a picture 300 pixels across, and where the caller *does* say so.
    ///
    /// **The cap is not simply raised**, and cannot be. Device limits are
    /// `downlevel_defaults`, which guarantees a `max_texture_dimension_2d` of
    /// exactly 2048; a wider mask is a texture the dab pass could not bind at
    /// all on a device Umber promises to run on. It is also four megabytes of
    /// coverage per brush in memory and a PNG per brush in the library, where
    /// every stamp any pack ships is 256 or smaller.
    ///
    /// Both axes are scaled by the same factor, so the proportions
    /// [`TipMask::aspect`] hands the dab pass are the material's own.
    ///
    /// **Reducing a sparse stamp lowers its peak**, and `CLAUDE.md` says what a
    /// lowered peak costs: a `max` stroke is capped at the mask's own brightest
    /// texel. That is exactly why the caller must measure
    /// [`stroke_coverage`] on the mask this returns rather than on the one that
    /// went in — [`crate::brushimport::clipstudio`] does, and a stamp the
    /// reduction thinned then arrives with `build_up` set, which is the
    /// mechanism that recovers strength without changing shape.
    pub fn reduced(
        width: u32,
        height: u32,
        coverage: Vec<u8>,
    ) -> Result<(Self, bool), PresetError> {
        let longest = width.max(height);
        if longest <= Self::MAX_SIZE {
            return Ok((Self::new(width, height, coverage)?, false));
        }
        if coverage.len() != width as usize * height as usize {
            return Err(malformed(format!(
                "brush tip has {} bytes, expected {}",
                coverage.len(),
                width as usize * height as usize
            )));
        }
        // `image`'s resampler rather than one of ours: it is already a
        // dependency, it scales the filter's support by the ratio — so a 3:1
        // reduction averages over three texels rather than point-sampling one
        // — and a resampler written here would be the second implementation
        // this codebase refuses everywhere else.
        let scale = f64::from(Self::MAX_SIZE) / f64::from(longest);
        let to = |v: u32| ((f64::from(v) * scale).round() as u32).clamp(1, Self::MAX_SIZE);
        let source = image::GrayImage::from_raw(width, height, coverage)
            .ok_or_else(|| malformed("the brush tip could not be read".to_string()))?;
        let small = image::imageops::resize(
            &source,
            to(width),
            to(height),
            image::imageops::FilterType::Triangle,
        );
        Ok((
            Self::new(small.width(), small.height(), small.into_raw())?,
            true,
        ))
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
                    Some(
                        px.chunks_exact(4)
                            .flat_map(|p| [p[0], p[1], p[2]])
                            .collect(),
                    ),
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
        let (width, height, rgba) = decode_picture(bytes)?;
        let (coverage, reading) = coverage_of(&rgba);
        Ok((Self::new(width, height, coverage)?, reading))
    }

    /// Read any picture Umber can decode as a **paper**.
    ///
    /// The same decoder [`from_picture`](Self::from_picture) uses and a
    /// different rule, and there is deliberately no reading to announce: see
    /// [`grain_of`]. A picture larger than [`TipMask::MAX_SIZE`] is refused for
    /// the same reason a tip that size is.
    pub fn from_paper(bytes: &[u8]) -> Result<Self, PresetError> {
        let (width, height, rgba) = decode_picture(bytes)?;
        Self::new(width, height, grain_of(&rgba))
    }
}

/// Decode a picture to straight-alpha, sRGB RGBA8.
///
/// PNG goes through the document importer's decoder rather than a second one of
/// this module's — [`TipMask::from_png`] is strict on purpose and reads only
/// what Umber itself wrote into `tips/`. Everything else goes through the
/// `image` crate, which is already carried for the flat export and covers JPEG,
/// TIFF, GIF and BMP.
fn decode_picture(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), PresetError> {
    /// The eight bytes every PNG starts with.
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    if bytes.starts_with(&PNG_MAGIC) {
        let image = crate::docimport::flat::decode_png(bytes, crate::docimport::SourceFormat::Png)
            .map_err(|e| malformed(e.to_string()))?;
        Ok((image.size.x, image.size.y, image.rgba))
    } else {
        let decoded = image::load_from_memory(bytes)
            .map_err(|e| malformed(format!("the picture could not be read ({e})")))?
            .to_rgba8();
        Ok((decoded.width(), decoded.height(), decoded.into_raw()))
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

/// The tile a picture makes when it is read as **paper**.
///
/// `rgba` is straight-alpha, sRGB-encoded RGBA8 — the same input
/// [`coverage_of`] takes.
///
/// **The rule is brightness, and there is nothing to decide.** A tip has to
/// guess between transparency and ink because a tip *is* a coverage mask and a
/// picture is four channels; a paper does not, because the grain multiplies
/// coverage — `mix(1.0, tile, strength)` — so the value at a texel already
/// means "how much of the dab this texel keeps". White keeps all of it and
/// black takes it away. That is what Umber's own three tiles hold, and it is
/// what a paper authored for any other application holds, for the plain reason
/// that a paper is a picture of paper and paper is light.
///
/// **Transparency is composited over white before the brightness is taken**, so
/// a hole in a tile is paper rather than a pit. The alternative reads an alpha
/// channel the author never used as a grain nobody drew — and since a pit
/// *removes* paint, the failure direction is a texture that erases most of
/// every stroke.
///
/// Note that this is very nearly the negative of [`coverage_of`]'s `Ink`
/// reading, and it must be: ink is where the paint goes and grain is where the
/// paint stays. Getting the two the wrong way round inverts somebody's paper,
/// which looks like a texture that bites in exactly the wrong places rather
/// than like a bug.
///
/// **Both steps happen in the stored, gamma-encoded values, and that is chosen
/// rather than overlooked.** The luminance is [`luminance`]'s perceptual one —
/// its own docs have the argument, and it is the same reading
/// [`Color::to_hsv`](crate::Color::to_hsv) takes — and the composite over white
/// rides on top of it, so half-transparent black comes out at 128 rather than
/// the 188 a linear-light composite would give. That is right for what this
/// value *is*: a paper is authored by eye, its texel is a *fraction of the dab
/// kept* rather than a light measurement, and the shader multiplies coverage by
/// it directly. This is the one place in Umber where a picture is read without
/// being linearised, and it is the one place where the number is not a colour.
pub fn grain_of(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .map(|px| {
            let a = px[3] as f32 / 255.0;
            let over_white = (1.0 - a) + a * luminance(px[0], px[1], px[2]);
            (over_white * 255.0 + 0.5) as u8
        })
        .collect()
}

/// How well a tile meets itself when it is repeated.
///
/// Grain is anchored to the *document* and wraps across it, so a tile whose
/// right edge does not continue into its left draws a **grid over the whole
/// canvas** — one hard line every `Brush::grain_scale` pixels, in every stroke
/// made with that brush. That is exactly the "subtly wrong pixels" this
/// codebase refuses to ship in silence, and it is invisible in the 56-point
/// thumbnail the brush editor shows.
///
/// Stated as a statistic rather than as an equality, because a paper is noise:
/// neighbouring texels differ everywhere, so "the edges match" is never true.
///
/// **The reading is a *column* step, signed, judged against the same reading
/// taken at every other offset in the tile** — and each of those three words
/// was arrived at by getting it wrong.
///
/// - **Signed**, because the artefact is a spatially *coherent* brightness
///   step and the noise it hides in is not. A mean of absolute differences was
///   the first attempt and is exactly the statistic that cancels the signal it
///   is looking for: for grain with a per-texel spread of σ the interior mean
///   absolute step is about `1.13σ`, so any tolerance loose enough not to
///   reject real papers swallows a systematic step of up to about `2σ` —
///   twenty or thirty levels on a photographed paper, which as a straight line
///   repeated across the canvas is unmissable to an eye that integrates a
///   coherent edge and invisible to a mean that does not.
/// - **A whole column against a whole column**, averaged down the join, which
///   is what turns per-texel noise of σ into `σ/√h` and leaves the step
///   standing at its full height.
/// - **Judged against every other offset, not against zero.** This is the
///   correction the shipped `canvas` tile forced: it is a woven grid, so
///   *every* column-to-column step in it is a real signed number of its own,
///   and a rule comparing the join against zero called a tile that provably
///   wraps — it is built from a sine of an exact multiple of the tile width —
///   a seam. What matters is not that the join steps, but that it steps by
///   more than the tile's own structure ever does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Seams {
    /// Mean **signed** step from the last column round to the first: the
    /// brightness the tile jumps by where its right edge meets its own left.
    pub across_x: f32,
    /// Mean of the same reading taken at every *interior* offset — the size of
    /// step this tile's own structure and noise routinely produce.
    pub inside_x: f32,
    pub across_y: f32,
    pub inside_y: f32,
}

impl Seams {
    /// Whether one axis joins without a step the eye would follow.
    ///
    /// Three times what the tile does elsewhere, plus one level. Both terms
    /// answer a different false alarm:
    ///
    /// - the multiple of `inside` is what keeps a *structured* paper from
    ///   being nagged about. A weave steps by the same amount at every thread,
    ///   so the join stepping by that much is the tile working, not failing.
    /// - the `+ 1` is what keeps a smooth, nearly seamless tile out of
    ///   trouble. Its interior steps can be a fraction of a level, so a
    ///   proportional tolerance alone would report a one-level dither mismatch
    ///   as a grid — and one level is the finest thing an 8-bit tile can
    ///   express at all.
    ///
    /// Three rather than the two the absolute reading needed is *not* a looser
    /// rule: these are averages down a whole edge, so the noise in them is
    /// smaller by `√h` and the same multiple bites far harder.
    fn axis_tiles(across: f32, inside: f32) -> bool {
        across.abs() <= inside * 3.0 + 1.0
    }

    /// Whether this tile repeats without a visible join on either axis.
    pub fn tiles(&self) -> bool {
        Self::axis_tiles(self.across_x, self.inside_x)
            && Self::axis_tiles(self.across_y, self.inside_y)
    }
}

/// Measure how `tile` meets itself. See [`Seams`].
///
/// One pass over the tile: a 2048² paper is four million texels and this runs
/// on an import, never on a frame — and the browser caches the answer, because
/// even an import's cost is not a per-row one.
pub fn seams(tile: &TipMask) -> Seams {
    let (w, h) = (tile.width(), tile.height());

    // The signed step from each line to the next, averaged down the whole
    // length of the join. The last entry is the wrap — the seam itself — and
    // the rest are what the tile does everywhere else.
    let steps = |count: u32, along: u32, at: &dyn Fn(u32, u32) -> u8| -> (f32, f32) {
        if count == 0 || along == 0 {
            return (0.0, 0.0);
        }
        let mut totals = vec![0.0f32; count as usize];
        for i in 0..count {
            let next = (i + 1) % count;
            let mut total = 0.0;
            for j in 0..along {
                total += at(next, j) as f32 - at(i, j) as f32;
            }
            totals[i as usize] = total / along as f32;
        }
        let across = totals[count as usize - 1];
        // The wrap is excluded from its own baseline, or a large seam would
        // raise the very figure it is being judged against.
        let inside = if count < 2 {
            0.0
        } else {
            totals[..count as usize - 1]
                .iter()
                .map(|v| v.abs())
                .sum::<f32>()
                / (count - 1) as f32
        };
        (across, inside)
    };

    let (across_x, inside_x) = steps(w, h, &|x, y| tile.at(x, y));
    let (across_y, inside_y) = steps(h, w, &|y, x| tile.at(x, y));

    Seams {
        across_x,
        inside_x,
        across_y,
        inside_y,
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

/// What a straight stroke reaches, under each of the two coverage rules the dab
/// pass offers.
///
/// The measurement that decides whether a stamp can be shipped, and the reason
/// it is a function rather than a judgement: a photographic texture stamp looks
/// dense and is not. See `docs/brush-sources.md`.
///
/// **Two things are measured into it and they take different statistics**, so
/// the fields say "coverage" rather than "peak". [`stroke_coverage`] reads a
/// tip and takes the **peak**; [`grain_coverage`] reads a paper and takes the
/// **mean**. Each of those documents why, and the choice is not cosmetic — the
/// peak rule cannot see a paper at all, which is exactly how a Clip Studio
/// brush came to paint at 27% of its own opacity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StrokeCoverage {
    /// Coverage the stroke reaches under the wet-layer `max`.
    ///
    /// Bounded above by the brightest texel it was measured from, whatever the
    /// spacing and however long the stroke — which is the entire problem.
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
    ///
    /// A tip's reading alone. A paper is not a mark and cannot be unusable —
    /// grain at any strength still lets the peaks through — so
    /// [`grain_coverage`]'s answer is never asked this.
    pub fn is_usable(&self) -> bool {
        self.under_build_up >= 0.5
    }
}

/// The stroke opacity that reproduces a per-dab one.
///
/// The third of this family, after [`stroke_coverage`] for a tip and
/// [`grain_coverage`] for a paper, and it exists for the same reason both of
/// those do: **MyPaint, Krita and Clip Studio state a per-dab alpha and
/// composite every dab, where `Brush::opacity` is applied once at commit over
/// coverage a `max` has already saturated.** Reading one as the other is not an
/// approximation, it is a different number — and on a dense brush it is out by
/// most of the mark.
///
/// `4H_pencil` states `opaque` 0.0257 with four dabs per radius. In MyPaint
/// those eight overlapping dabs build to about 0.19; read as a stroke opacity
/// it painted at 0.026, which is a pencil seven times fainter than its author
/// drew. Twenty-nine shipped presets sat under 35% for this reason, the
/// faintest at 0.015.
///
/// The simulation is the dab pass's own: dabs every `spacing` of a radius along
/// a line, each contributing `per_dab × falloff` at the point being measured,
/// composited `a += c(1 − a)`. Measured at the **centre line**, which is where
/// a stroke's strength is read from — the edges differ between the two rules
/// whatever is done here, because one has a built-up falloff and the other has
/// a `max`ed one, and no single number reconciles that.
///
/// **`per_dab` at or above 1.0 comes back as 1.0**, so every brush that already
/// paints solid is untouched — which is most of the library, whose median
/// opacity is exactly 1.0.
pub fn dab_stack_alpha(per_dab: f32, spacing: f32, hardness: f32) -> f32 {
    let per_dab = per_dab.clamp(0.0, 1.0);
    if per_dab <= 0.0 {
        return 0.0;
    }
    let mut alpha = 0.0f32;
    for coverage in centre_line_falloffs(spacing, hardness) {
        alpha += per_dab * coverage * (1.0 - alpha);
    }
    alpha.clamp(0.0, 1.0)
}

/// The dab pass's own falloff at the stroke's centre line: one weight per dab
/// whose footprint reaches the point being measured.
///
/// One statement of it, because [`dab_stack_alpha`] and [`stack_depth`] are the
/// forward and the reverse of the same simulation and a second copy of the walk
/// is exactly how the two would come to disagree about how deep a stroke is.
fn centre_line_falloffs(spacing: f32, hardness: f32) -> impl Iterator<Item = f32> {
    // In radii: a dab lands every `2 × spacing` of them, and one reaches 1.0.
    let step = (spacing.clamp(0.001, 4.0) * 2.0).max(1e-3);
    let reach = (1.0 / step).ceil() as i32;
    let inner = hardness.clamp(0.0, 1.0);
    (-reach..=reach).filter_map(move |i| {
        let d = (i as f32 * step).abs();
        if d >= 1.0 {
            return None;
        }
        // The fragment shader's falloff, with the antialiasing margin left out:
        // that is a per-pixel softening at the rim and this is the centre line.
        let t = if inner >= 1.0 {
            0.0
        } else {
            ((d - inner) / (1.0 - inner)).clamp(0.0, 1.0)
        };
        Some(1.0 - t * t * (3.0 - 2.0 * t))
    })
}

/// Where [`stack_depth`]'s fit is pinned, and it is a measured figure rather
/// than a round one.
///
/// The unfitted model — a flat stack as deep as the falloffs sum to — is exact
/// as the coverage approaches zero, because every term is then near its own
/// linearisation, and worst high up: 10.7 levels of 255 at a target of 0.88.
/// Pinning the fit spreads what is left either side of the pin, so the choice is
/// only which end to be exact at, and this is the one that measured smallest over
/// the whole surface. 0.85 leaves 9.0 levels and 0.45 leaves 7.7; this leaves
/// 6.4, and 2.4 at the 10% spacing a brush is actually likely to carry.
/// `examples/measure-buildup.rs` prints the sweep those come from.
const CALIBRATION: f32 = 0.6;

/// The depth of a flat stack of dabs that accumulates the way this brush's
/// overlapping dabs actually do, for [`per_dab_for_stroke`] to invert.
///
/// **Not simply how many dabs reach a point.** That count — the falloffs summed
/// — is what the accumulation behaves like for a faint dab, where every term is
/// small and the product is near its linearisation, and it overstates the depth
/// for a strong one, because a dab out at the rim of the overlap contributes a
/// fraction of a dab and the near ones have already saturated. So the depth is
/// *fitted*: the weighted product is solved once for the coverage that reaches
/// [`CALIBRATION`], and the flat depth that reaches the same place is returned.
///
/// The bisection is affordable because this is a **per-stroke** figure, not a
/// per-dab one — it depends on nothing but the spacing and the hardness. Forty
/// halvings of a monotone function is exact to a float, and the alternative
/// (solving the weighted product per dab) is a bisection on the drawing path.
///
/// At least one, because a dab always covers its own centre; and exactly one for
/// a brush whose dabs do not overlap at all, which makes the conversion the
/// identity.
pub fn stack_depth(spacing: f32, hardness: f32) -> f32 {
    let falloffs: Vec<f32> = centre_line_falloffs(spacing, hardness).collect();
    let stacked = |per_dab: f32| {
        let mut alpha = 0.0f32;
        for w in &falloffs {
            alpha += per_dab * w * (1.0 - alpha);
        }
        alpha
    };
    // A single dab already reaching the calibration point is a stack with
    // nothing to unpick, and the answer is exactly one.
    if stacked(1.0) <= CALIBRATION {
        return 1.0;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if stacked(mid) < CALIBRATION {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let per_dab = (0.5 * (lo + hi)).clamp(1e-6, 1.0 - 1e-6);
    // `n` from `1 - (1 - c)^n = CALIBRATION`.
    ((1.0 - CALIBRATION).ln() / (1.0 - per_dab).ln()).max(1.0)
}

/// [`dab_stack_alpha`] read backwards: the per-dab coverage whose accumulation
/// reaches `stroke_alpha`.
///
/// **This is what keeps [`crate::Brush::build_up`] from redefining what a
/// pressure-opacity curve means.** Under the wet-layer `max` the per-dab figure
/// *is* the stroke's, so `coverage_at` describes the mark and the brush editor's
/// graph of it is a picture of the mark. Build-up swaps that blend for
/// `a = cov + a(1 − cov)`, and then the same curve arrives at the canvas
/// compounded [`stack_depth`] times over: an author's 4% at a feather touch became
/// 31% of the layer, and everything above about a third of the pressure range
/// saturated flat at full. Both ends capped, the curve still drawing 0.04…1.0,
/// and — worse — by an amount decided by the **spacing**, which for an imported
/// brush whose file says "automatic" is a default constant nobody chose. Two
/// reports of the same brush a version apart, in opposite directions, are this.
///
/// So a building stroke converts on the way *out*: the dab gets the smaller
/// figure, the accumulation puts it back, and a tip's or a paper's own faintness
/// still builds because those are multiplied in by the shader afterwards. That
/// is the whole point of the split — build-up is for a mark made of many faint
/// *stamps*, never for undoing the artist's own opacity.
///
/// **Zero and one are exact fixed points, and a depth of one is the exact
/// identity**, so a brush that already paints solid — which is nearly every one
/// that sets `build_up` — is untouched, and every brush that does not set it
/// still carries its curve to the dab byte for byte.
///
/// `depth` comes from [`stack_depth`], which fits it to the way this brush's
/// dabs actually accumulate rather than counting them; the inverse here is then
/// the flat `1 − (1 − c)^depth`. Exact for a hard dab, where the falloff across
/// the overlap is flat, and out by at most **6.4 levels of 255** over every
/// spacing and hardness a brush can carry — 2.4 at a 10% spacing, and exactly
/// nothing above 50%, where dabs stop overlapping and the depth is one —
/// `a_converted_dab_builds_back_to_what_the_curve_asked_for` measures that
/// rather than assuming it, and `examples/measure-buildup.rs` prints the surface. The
/// remaining error is the same order as `dab_stack_alpha`'s own against the
/// `R8Unorm` scratch, and the alternative — solving the weighted product per
/// dab — is a bisection on the drawing path.
pub fn per_dab_for_stroke(stroke_alpha: f32, depth: f32) -> f32 {
    let target = stroke_alpha.clamp(0.0, 1.0);
    // A single dab deep is every brush that does not build up, and it has to
    // come back **byte for byte** rather than merely close: the `max` path is
    // what every pixel test in the suite is written against. `1 - (1 - x)` is
    // not the identity in floating point — at 0.03 it returns 0.029999971 — so
    // the early answer is the guarantee and not an optimisation, though it is
    // also what keeps `powf` off the ordinary drawing path.
    if depth <= 1.0 || target <= 0.0 {
        return target;
    }
    // `powf` on a zero base would otherwise decide whether a brush that already
    // paints solid stays solid, which is nearly every brush that sets build-up.
    if target >= 1.0 {
        return 1.0;
    }
    1.0 - (1.0 - target).powf(1.0 / depth)
}

/// What a stroke through `tile` reaches under each rule, at `strength`.
///
/// [`stroke_coverage`]'s twin for the *paper*, and it exists because the tip's
/// reading is structurally blind to a grain. Three things differ, and each of
/// them is why:
///
/// - **The statistic is the mean, not the peak.** A tip is stretched over its
///   dab, so a `max` stroke is capped at the mask's brightest texel and the
///   whole mark is capped with it — peak is the mark. A paper is anchored to
///   the *document*: it is sampled at the pixel rather than at the dab, so its
///   brightest texel survives whatever the strength and the peak agrees with
///   itself. What collapses is everything else. The tile that prompted this is
///   a dark grunge scatter with a mean of 0.272 and a maximum of 255 — peak
///   agreement 1.0, and a stroke at 27% of the opacity its author set.
/// - **There is no geometry.** Every dab reaching one document pixel is scaled
///   by the *same* texel, so the `max` rule is exactly `tile × strength'd`,
///   and build-up is `1 - (1 - t)^n` with `n` the dabs deep the spacing puts on
///   a point. No stamping loop, no mask centred in a square: those describe a
///   picture that moves with the brush, and this one does not.
/// - **`strength` is folded in first**, through `mix(1, t, strength)` — the dab
///   pass's own line, so a strength of zero is the exact identity here as well
///   and answers agreement 1.0 without a special case.
///
/// `n` is `1 / spacing`, the dabs whose footprint covers a point when they land
/// every `spacing × diameter`. Taken at full coverage: what is being asked is
/// whether the *rule* changes the mark, and a dab's own falloff scales both
/// sides of that.
pub fn grain_coverage(tile: &TipMask, strength: f32, spacing: f32) -> StrokeCoverage {
    let strength = strength.clamp(0.0, 1.0);
    let deep = (1.0 / spacing.clamp(0.01, 1.0)).round().max(1.0) as i32;

    let texels = tile.coverage();
    if texels.is_empty() {
        return StrokeCoverage {
            under_max: 1.0,
            under_build_up: 1.0,
        };
    }

    let mut under_max = 0.0f64;
    let mut under_build_up = 0.0f64;
    for texel in texels {
        let t = f64::from(*texel) / 255.0;
        let bite = 1.0 - f64::from(strength) * (1.0 - t);
        under_max += bite;
        under_build_up += 1.0 - (1.0 - bite).powi(deep);
    }
    let count = texels.len() as f64;

    StrokeCoverage {
        under_max: (under_max / count) as f32,
        under_build_up: (under_build_up / count) as f32,
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

    /// A mask within the cap must be the bytes that went in — not resampled,
    /// not "resampled by a factor of one". Every stamp any pack ships is well
    /// inside it, so this is the path that matters.
    #[test]
    fn a_mask_within_the_cap_is_not_touched_and_a_larger_one_keeps_its_shape() {
        let coverage: Vec<u8> = (0..(13 * 7)).map(|i| (i * 11 % 256) as u8).collect();
        let (mask, reduced) = TipMask::reduced(13, 7, coverage.clone()).expect("build");
        assert!(!reduced);
        assert_eq!(mask.coverage(), coverage);
        assert_eq!((mask.width(), mask.height()), (13, 7));

        // Over the cap on the long axis: both axes scale by the same factor, so
        // the proportions the dab pass is handed are the material's own.
        let (w, h) = (TipMask::MAX_SIZE * 2, TipMask::MAX_SIZE / 2);
        let (mask, reduced) = TipMask::reduced(w, h, vec![255; (w * h) as usize]).expect("build");
        assert!(reduced);
        assert_eq!((mask.width(), mask.height()), (TipMask::MAX_SIZE, 512));
        assert_eq!(mask.aspect(), (1.0, 0.25));
        // A solid stamp reduced is still solid: the resampler must not have
        // averaged the edge against nothing. Not an exact byte — that would be
        // a promise about `image`'s accumulation and rounding rather than about
        // anything here, and `CLAUDE.md` refuses that shape of assertion for
        // the composite pass for the same reason.
        assert!(mask.coverage().iter().all(|v| *v >= 250));

        // A buffer that does not match its dimensions is refused rather than
        // read past, on both sides of the cap.
        assert!(TipMask::reduced(4, 4, vec![0; 15]).is_err());
        assert!(TipMask::reduced(TipMask::MAX_SIZE + 1, 4, vec![0; 15]).is_err());
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
                "{name} is too faint to accumulate, so it would paint nothing"
            );
        }
    }

    /// Every shipped stamp has to paint at the strength it was drawn at, which
    /// after the build-up work is a measurable claim rather than a hope.
    ///
    /// **Both halves of the mark are measured, because either can cap it.** A
    /// tip is capped at its own brightest texel; a paper caps the stroke at the
    /// tile's mean wherever the grain bites. Reading only the first is what
    /// shipped six textured presets on the `max` path — and what let a Clip
    /// Studio sketch pencil arrive at 27% of its own opacity, which is where
    /// this was reported from.
    #[test]
    fn a_shipped_stamp_paints_at_the_strength_it_was_drawn_at() {
        let marked: Vec<_> = crate::preset::builtin()
            .iter()
            .filter(|p| p.tip.is_some() || p.brush.has_grain())
            .collect();
        assert!(
            !marked.is_empty(),
            "the shipped library carries no stamp or textured brush, so the \
             mechanism is untested"
        );

        for preset in marked {
            let mut wanted = false;
            let mut how = String::new();

            if let Some(name) = preset.tip.as_deref() {
                let mask = builtin(name)
                    .unwrap_or_else(|| panic!("{} names a tip nobody ships: {name}", preset.name));
                let measured = stroke_coverage(mask, preset.brush.spacing);
                wanted |= measured.needs_build_up();
                how.push_str(&format!(" tip {measured:?}"));
            }

            if preset.brush.has_grain() {
                let name = preset
                    .paper
                    .as_deref()
                    .unwrap_or_else(|| preset.brush.grain_pattern.key());
                let tile = pattern(name).unwrap_or_else(|| {
                    panic!("{} names a paper nobody ships: {name}", preset.name)
                });
                let measured = grain_coverage(tile, preset.brush.grain, preset.brush.spacing);
                wanted |= measured.needs_build_up();
                how.push_str(&format!(" paper {measured:?}"));
            }

            assert_eq!(
                preset.brush.build_up, wanted,
                "{} is shipped with build_up = {} but measures{how}",
                preset.name, preset.brush.build_up
            );
        }
    }

    /// Every shipped brush that names a paper has one to bite through, and no
    /// shipped *import* claims a grain without naming the tile it came from.
    ///
    /// A paper that resolves to nothing paints **flat**, which is the right
    /// answer for a library somebody copied without its `papers/` and the wrong
    /// one here: both halves are written by the same generator, so a name with
    /// no file is a generator that lost one — and the brush would then paint
    /// with the grain strength its author set and no texture at all, which is
    /// weaker and smoother than either the original or a plain brush.
    ///
    /// The second half is the direction that has actually gone wrong before.
    /// An imported brush carrying a grain with no paper of its own falls back
    /// to `Brush::grain_pattern`'s default — a shipped tile the author never
    /// chose, which is exactly the substitution that made a Clip Studio import
    /// paint at 78% of its stated opacity. Umber's own presets are exempt
    /// because for them the enum *is* the choice.
    #[test]
    fn a_shipped_papered_brush_has_a_paper_to_bite_through() {
        let papered: Vec<_> = crate::preset::builtin()
            .iter()
            .filter(|p| p.paper.is_some())
            .collect();
        assert!(
            !papered.is_empty(),
            "the shipped library carries no papered brush, so the mechanism is untested"
        );

        for preset in papered {
            let name = preset.paper.as_deref().expect("paper");
            assert!(
                pattern(name).is_some(),
                "{} names a paper nobody ships: {name}",
                preset.name
            );
            assert!(
                preset.brush.has_grain(),
                "{} names a paper and bites at {}",
                preset.name,
                preset.brush.grain
            );
        }

        for preset in crate::preset::builtin() {
            if preset.id.starts_with("umber/") || preset.paper.is_some() {
                continue;
            }
            assert!(
                !preset.brush.has_grain(),
                "{} was imported with a grain of {} and no paper of its own",
                preset.name,
                preset.brush.grain
            );
        }
    }

    /// The grain is anchored to the document and repeats across it, so a seam
    /// would draw a grid over every stroke that used it.
    ///
    /// Tested as a statistic rather than as an equality: the tiles are noise, so
    /// neighbouring texels differ everywhere. What must not happen is for the
    /// pair *across* the seam to differ by more than pairs inside the tile do.
    ///
    /// It covers the imported papers as well as Umber's own three, and that is
    /// deliberate rather than incidental: Krita tiles a pattern exactly as this
    /// does, so a seam here is a seam its author already paints with — but one
    /// that fails this is a tile nobody should be handed by a *shipped* brush,
    /// which is the same standard `build-brush-library.rs` holds everything to.
    #[test]
    fn every_shipped_pattern_tiles_without_a_seam() {
        assert_eq!(
            patterns().len(),
            crate::pattern_table::PATTERNS.len(),
            "a shipped pattern failed to decode"
        );

        for (name, tile) in patterns() {
            let measured = seams(tile);
            assert!(measured.tiles(), "{name} has a seam: {measured:?}");
        }
    }

    /// The check an imported texture is put through. Both directions matter:
    /// a tile that plainly does not join has to be caught, and one that does
    /// must not be nagged about, or the notice becomes noise nobody reads.
    #[test]
    fn a_tile_that_does_not_join_is_told_from_one_that_does() {
        // Two unrelated halves butted together: smooth inside, a cliff at the
        // join. This is what an unprepared photograph of paper looks like.
        let mut split = vec![0u8; 16 * 16];
        for y in 0..16 {
            for x in 0..16 {
                split[y * 16 + x] = if x < 8 { 40 } else { 220 };
            }
        }
        let measured = seams(&TipMask::new(16, 16, split).expect("build"));
        assert!(
            !measured.tiles(),
            "a hard vertical join should be caught: {measured:?}"
        );
        // The horizontal axis of that tile is perfect — every row is the same —
        // so it is the *pair* of readings that decides, not either alone.
        assert_eq!(measured.across_y, 0.0);

        // A gradient that wraps: every step is one level, including across the
        // join. Noise with a matched border behaves the same way.
        let ramp: Vec<u8> = (0..16 * 16)
            .map(|i| ((i % 16) * 16) as u8)
            .collect::<Vec<_>>();
        let measured = seams(&TipMask::new(16, 16, ramp).expect("build"));
        assert!(!measured.tiles(), "a saw-tooth ramp does not wrap");

        // Flat: nothing to see anywhere, and the `+ 1` is what keeps it from
        // being reported on a single level of rounding.
        let flat = TipMask::new(16, 16, vec![128; 256]).expect("build");
        assert!(seams(&flat).tiles());

        // A one-texel tile has no interior at all and must not divide by zero.
        let dot = TipMask::new(1, 1, vec![200]).expect("build");
        assert!(seams(&dot).tiles());
    }

    /// The two cases the tolerance actually adjudicates, neither of which the
    /// test above reaches: a *noisy* tile hiding a moderate step, and a smooth
    /// one with nothing wrong but a level of dither. The synthetic extremes are
    /// separable by any rule at all; these are the ones a real paper produces,
    /// and getting either wrong is what makes the notice noise nobody reads.
    #[test]
    fn a_step_hidden_in_grain_is_still_found_and_a_level_of_dither_is_not() {
        // Deterministic, so this cannot be flaky: a hash-like sequence with a
        // spread of about ±25 levels, which is a coarse paper.
        let noise = |x: u32, y: u32| {
            let n =
                (x.wrapping_mul(1_664_525) ^ y.wrapping_mul(1_013_904_223)).wrapping_mul(69_069);
            ((n >> 16) % 51) as i32 - 25
        };
        let side = 64u32;
        // A photographed paper: grain of about ±25 levels over a slow ramp of
        // `lift` levels across the tile. The ramp is the failure — it is
        // smooth everywhere inside and jumps back the whole way at the wrap,
        // which is precisely what uneven lighting on a scan produces and what
        // a purely absolute rule cannot see.
        let build = |lift: i32| {
            let texels: Vec<u8> = (0..side * side)
                .map(|i| {
                    let (x, y) = (i % side, i / side);
                    let ramp = lift * x as i32 / side as i32;
                    (128 + noise(x, y) + ramp).clamp(0, 255) as u8
                })
                .collect();
            TipMask::new(side, side, texels).expect("build")
        };

        // No ramp: the noise is the only thing there, and it must not be
        // reported. This is the false alarm that would put a warning on every
        // real paper anybody imports.
        let clean = seams(&build(0));
        assert!(
            clean.tiles(),
            "grain alone should not read as a seam: {clean:?}"
        );

        // A 25-level ramp — *smaller* than the grain it sits in, and the case
        // a mean of absolute differences cannot see at all: per texel the join
        // differs by about the same as any other pair, so the old rule passed
        // it. Averaged down the edge the noise falls away and the step stands.
        let seamed = seams(&build(25));
        assert!(
            !seamed.tiles(),
            "a step buried in grain was missed: {seamed:?}"
        );
        // And it really is buried: per-texel, the join is indistinguishable.
        let per_texel = |a: &TipMask, at: &dyn Fn(u32, u32) -> (u32, u32)| {
            let seam: f32 = (0..side)
                .map(|j| {
                    let (x0, y0) = at(side - 1, j);
                    let (x1, y1) = at(0, j);
                    a.at(x0, y0).abs_diff(a.at(x1, y1)) as f32
                })
                .sum::<f32>()
                / side as f32;
            seam
        };
        let ramped = build(25);
        let joined = per_texel(&ramped, &|i, j| (i, j));
        let inside: f32 = (0..side - 1)
            .map(|x| {
                (0..side)
                    .map(|y| ramped.at(x, y).abs_diff(ramped.at(x + 1, y)) as f32)
                    .sum::<f32>()
                    / side as f32
            })
            .sum::<f32>()
            / (side - 1) as f32;
        assert!(
            joined <= inside * 2.0 + 1.0,
            "this fixture has to be one the old absolute-difference rule passed, \
             or it proves nothing: {joined:.1} across, {inside:.1} inside"
        );

        // And the other end: a nearly flat tile whose edges disagree by one
        // level of dither is fine, which is what the `+ 1` is for. A purely
        // proportional tolerance would call this a grid.
        let dithered: Vec<u8> = (0..side * side)
            .map(|i| {
                let x = i % side;
                if x == 0 { 129 } else { 128 }
            })
            .collect();
        let smooth = seams(&TipMask::new(side, side, dithered).expect("build"));
        assert!(
            smooth.inside_x < 0.1,
            "the fixture is not smooth: {smooth:?}"
        );
        assert!(
            smooth.tiles(),
            "one level of dither is not a grid: {smooth:?}"
        );
    }

    /// The tile that forced the statistic to be relative rather than absolute.
    ///
    /// `canvas` is a woven grid built from a sine of an exact multiple of the
    /// tile width, so it provably wraps — and *every* column-to-column step in
    /// it is a real signed number, nineteen levels at the join included. A rule
    /// that compared the join against zero called it a seam. Pinned here as
    /// well as in the shipped sweep, because the sweep would go quiet the day
    /// somebody changed the tile rather than the rule.
    #[test]
    fn a_woven_tile_steps_at_every_thread_and_still_joins() {
        let canvas = pattern("canvas").expect("shipped");
        let measured = seams(canvas);
        assert!(
            measured.across_x.abs() > 5.0,
            "the fixture no longer demonstrates anything: {measured:?}"
        );
        assert!(measured.tiles(), "a weave is not a seam: {measured:?}");
    }

    /// The paper rule, and the reason it is not the tip's. Getting these the
    /// wrong way round inverts somebody's grain — the paper bites where the
    /// author drew a peak — which reads as a texture that behaves oddly rather
    /// than as a bug.
    #[test]
    fn a_paper_is_read_as_brightness_where_a_tip_is_read_as_ink() {
        // White keeps the whole dab, black takes it away. That is the opposite
        // end of the same picture from `coverage_of`'s ink reading.
        let opaque = [255, 255, 255, 255, 0, 0, 0, 255];
        assert_eq!(grain_of(&opaque), [255, 0]);
        assert_eq!(coverage_of(&opaque).0, [0, 255]);

        // Transparent is paper, not a pit: composited over white it is white,
        // so the dab passes through untouched.
        assert_eq!(grain_of(&[0, 0, 0, 0]), [255]);
        // Half-transparent black is half way there.
        let half = grain_of(&[0, 0, 0, 128])[0];
        assert!((half as i32 - 128).abs() <= 2, "got {half}");

        // And through a file, which is the door an import comes in by.
        let png = encode(2, 1, png::ColorType::Grayscale, &[255, 0]);
        let paper = TipMask::from_paper(&png).expect("import");
        assert_eq!(paper.coverage(), [255, 0]);
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

    /// **The tip's rule is structurally blind to a paper, and this is the shape
    /// of the brush that proved it.**
    ///
    /// A Clip Studio sketch pencil arrived nearly transparent: a 500×500 grunge
    /// scatter at `TextureDensity` 100, a mean of 0.272 and a brightest texel of
    /// 255. Under the `max` blend that is the whole stroke at 27% of the opacity
    /// its author set, for as long as the stroke lasts; Clip Studio composites
    /// every dab, so the same tile there reaches 77%.
    ///
    /// The fixture is that tile in miniature — a quarter of it white and the
    /// rest a tenth lit — and the first assertion is the one that matters:
    /// [`stroke_coverage`]'s peak agrees with itself on it, so no threshold on
    /// the tip's reading could ever have caught this.
    ///
    /// The faint texels are deliberately **not** black, and that is the rule's
    /// own boundary rather than a detail of the fixture: see the stencil at the
    /// foot of this test.
    #[test]
    fn a_paper_that_caps_a_stroke_asks_for_build_up_where_a_tips_peak_cannot() {
        let scatter = TipMask::new(4, 4, {
            let mut texels = vec![26u8; 16];
            texels[..4].fill(255);
            texels
        })
        .expect("build");

        // The reading that was already there, on the same picture. Its peak is
        // 1.0 under both rules, so it reports perfect agreement — correctly,
        // for a *tip*, and uselessly for a paper.
        assert!(!stroke_coverage(&scatter, 0.1).needs_build_up());

        let measured = grain_coverage(&scatter, 1.0, 0.1);
        assert!(
            (measured.under_max - 0.326).abs() < 1e-2,
            "the mean is the mark a `max` stroke makes, got {measured:?}"
        );
        assert!(
            measured.under_build_up > 0.7,
            "compositing builds the faint texels towards solid, got {measured:?}"
        );
        assert!(measured.needs_build_up());

        // Half the strength is half the bite, so the same tile at 0.5 still
        // caps the stroke and still asks.
        assert!(grain_coverage(&scatter, 0.5, 0.1).needs_build_up());

        // **A stencil is the boundary, and it answers no.** Where a tile is
        // only ever 0 or 1 there is nothing for compositing to build: a texel
        // at zero stays at zero however many dabs land on it, and one at full
        // is already solid. The two rules make the identical mark, so the
        // cheaper one is right — build-up is for a grain that is *faint*, not
        // for one that is merely dark.
        let stencil = TipMask::new(4, 4, {
            let mut texels = vec![0u8; 16];
            texels[..4].fill(255);
            texels
        })
        .expect("build");
        assert!(!grain_coverage(&stencil, 1.0, 0.1).needs_build_up());
    }

    /// A grain that takes nothing away asks for nothing, and the two ends of
    /// that are the identities the dab pass already promises.
    #[test]
    fn a_grain_that_bites_nothing_needs_no_build_up() {
        let scatter = TipMask::new(4, 4, {
            let mut texels = vec![0u8; 16];
            texels[..4].fill(255);
            texels
        })
        .expect("build");

        // `mix(1, tile, 0)` is exactly 1.0 — the shader's own line — so a brush
        // with a paper it cannot feel is on the `max` path, whatever the tile
        // happens to hold.
        let none = grain_coverage(&scatter, 0.0, 0.1);
        assert_eq!(none.under_max, 1.0);
        assert!(!none.needs_build_up());

        // And a tile that is white everywhere is the same identity from the
        // other side, at full strength.
        let blank = TipMask::new(4, 4, vec![255; 16]).expect("build");
        let full = grain_coverage(&blank, 1.0, 0.1);
        assert_eq!(full.under_max, 1.0);
        assert!(!full.needs_build_up());
    }

    /// [`per_dab_for_stroke`] against [`dab_stack_alpha`], which is the forward
    /// simulation it inverts, over every spacing and hardness a brush can carry.
    ///
    /// **It measures the worst case rather than asserting a bound out of
    /// nowhere.** [`stack_depth`] fits a flat depth to the way a brush's dabs
    /// actually accumulate, pinned at [`CALIBRATION`]; what is left over is the
    /// difference in *shape* between a flat stack and a stack of falloffs, and
    /// it is largest for a soft dab at a spacing wide enough that only two or
    /// three dabs overlap. A hard dab is exact, because its falloff across the
    /// overlap is flat and the flat model is then the real one.
    ///
    /// The bound is a whole level of 255 clear of what this measures, which
    /// leaves room for the figure to move without leaving room for the defect it
    /// exists to catch: the failure that made this necessary was 68 levels at
    /// the light end and 105 in the middle.
    #[test]
    fn a_converted_dab_builds_back_to_what_the_curve_asked_for() {
        let mut worst = 0.0f32;
        let mut worst_at = (0.0, 0.0, 0.0);
        for step in 1..=100 {
            let spacing = step as f32 / 100.0;
            for hardness in [0.0, 0.25, 0.5, 0.55, 0.81, 0.9, 1.0] {
                let depth = stack_depth(spacing, hardness);
                for step in 0..=100 {
                    let want = step as f32 / 100.0;
                    let got = dab_stack_alpha(per_dab_for_stroke(want, depth), spacing, hardness);
                    let error = (got - want).abs();
                    if error > worst {
                        worst = error;
                        worst_at = (spacing, hardness, want);
                    }
                }
            }
        }
        // Measured: 0.0250, which is 6.4 levels of 255, at spacing 0.30 and
        // hardness 0.10 — three dabs of very unequal weight. At the 10% spacing
        // a brush is actually likely to carry it is 2.4 levels, and every
        // hardness of 1.0 is exact to a float.
        assert!(
            worst < 0.03,
            "worst round trip {worst} at spacing/hardness/target {worst_at:?}"
        );

        // The two fixed points, exactly, at a depth that is genuinely stacking.
        let depth = stack_depth(0.1, 1.0);
        assert!(depth > 8.0, "a 10% spacing is nine dabs deep, not {depth}");
        assert_eq!(per_dab_for_stroke(0.0, depth), 0.0);
        assert_eq!(per_dab_for_stroke(1.0, depth), 1.0);

        // And a depth of one is the identity byte for byte, which is what keeps
        // every brush that does not build up on the path it was tested on.
        for step in 0..=255 {
            let v = step as f32 / 255.0;
            assert_eq!(per_dab_for_stroke(v, 1.0), v);
        }
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
