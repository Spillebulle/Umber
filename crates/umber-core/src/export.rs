//! Flat image export: which formats Umber writes, what each of them loses, and
//! the encoders themselves.
//!
//! The pixels are **not** this module's. They come off the GPU through
//! `CanvasRenderer::export_rgba`, which is the screen composite pass with an
//! export flag — the same pass `pick_colour`, the autosave capture and the
//! flattened preview in an `.ora` all reuse. There is exactly one place in
//! Umber that flattens a layer stack, and a second one here would be a second
//! copy of the blend maths and an export that could disagree with the screen.
//! Everything below is downstream of the bytes that function returns:
//! straight-alpha, sRGB-encoded RGBA8.
//!
//! That also means the background rule falls out for free. The document's
//! background composites under the stack *inside* that pass, so a white-backed
//! document arrives here opaque and a transparent one arrives with its alpha
//! intact. Nothing here has to know a `Background` exists.
//!
//! # Why any of this is in `umber-core`
//!
//! Everything a format *is* — what it can carry, what it costs, what it is
//! called, which extension names it, what a document's picture should be called
//! when it lands on disk — is a rule, and rules are testable without a window
//! or a device. That is the same division [`crate::document::CanvasCopy::plan`]
//! and [`crate::clipboard::Clip::place`] keep. The encoders sit beside them for
//! the same reason: they take bytes, so every test in this file runs on CI
//! whether or not the runner has an adapter.
//!
//! # Formats
//!
//! PNG keeps using the `png` crate directly rather than being routed through
//! `image`. It is the one format Umber has to get exactly right — it is what a
//! painter exports to show the picture — and the direct encoder is what lets
//! the file carry an `sRGB` chunk saying what the composite shader already did
//! on the way out. `image`'s PNG encoder writes no such chunk, so routing PNG
//! through it would quietly drop colour-space metadata to save a dozen lines.
//! Everything else goes through `image`, with only the four format features it
//! needs switched on.
//!
//! WebP is deliberately **not** offered. `image`'s encoder is lossless-only, so
//! the entry would read "WebP" and produce neither of the two things somebody
//! choosing it wants — the small lossy file, or a format more widely read than
//! PNG. A lossless WebP is a PNG with worse support, and Umber already writes
//! PNG.

use std::path::{Path, PathBuf};

use crate::color::{linear_to_srgb, srgb_to_linear};

/// A flat image format Umber can write.
///
/// The order is the order the dialog offers them in: the two everybody wants
/// first, then the three that exist because somebody's pipeline asks for them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Png,
    Jpeg,
    Tiff,
    Gif,
    Bmp,
}

impl ExportFormat {
    /// Every format, in the order they are offered.
    pub const ALL: [ExportFormat; 5] = [
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::Tiff,
        ExportFormat::Gif,
        ExportFormat::Bmp,
    ];

    /// What the format is called on a button.
    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG",
            ExportFormat::Jpeg => "JPEG",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::Gif => "GIF",
            ExportFormat::Bmp => "BMP",
        }
    }

    /// What the file dialog's filter is called.
    pub fn filter(self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG image",
            ExportFormat::Jpeg => "JPEG image",
            ExportFormat::Tiff => "TIFF image",
            ExportFormat::Gif => "GIF image",
            ExportFormat::Bmp => "Windows bitmap",
        }
    }

    /// One line saying what the format is for.
    pub fn note(self) -> &'static str {
        match self {
            ExportFormat::Png => "Lossless, keeps transparency. The one to post.",
            ExportFormat::Jpeg => "Small photographic files. Every save loses a little more.",
            ExportFormat::Tiff => "Lossless and keeps transparency, for print and archives.",
            ExportFormat::Gif => "256 colours. For somewhere that will take nothing else.",
            ExportFormat::Bmp => "Uncompressed and very large. For older tools that ask for it.",
        }
    }

    /// The extension a file of this format is written with.
    pub fn extension(self) -> &'static str {
        self.extensions()[0]
    }

    /// Every extension that names this format, the canonical one first.
    ///
    /// A user who typed `photo.jpeg` asked for a JPEG and must not be handed
    /// `photo.jpeg.jpg`.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            ExportFormat::Png => &["png"],
            ExportFormat::Jpeg => &["jpg", "jpeg"],
            ExportFormat::Tiff => &["tif", "tiff"],
            ExportFormat::Gif => &["gif"],
            ExportFormat::Bmp => &["bmp"],
        }
    }

    /// The format an extension names, if any. Case-insensitive.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|f| f.extensions().iter().any(|e| ext.eq_ignore_ascii_case(e)))
    }

    /// Whether the file can hold the document's transparency.
    ///
    /// GIF is in the `false` group even though the format has a transparent
    /// palette index: one index is a hole, not an alpha channel, so a soft edge
    /// would come out as a hard one chosen by a quantiser. Matting it is the
    /// predictable answer and it is the one that gets said out loud. BMP is
    /// there for a different reason — 32-bit BMP alpha exists and half the
    /// readers that accept a BMP at all ignore it, so writing one would produce
    /// a file whose transparency depends on who opens it.
    pub fn carries_alpha(self) -> bool {
        matches!(self, ExportFormat::Png | ExportFormat::Tiff)
    }

    /// Whether [`ExportOptions::quality`] means anything here.
    pub fn has_quality(self) -> bool {
        matches!(self, ExportFormat::Jpeg)
    }

    /// Whether the format reduces the picture to a palette.
    pub fn quantises(self) -> bool {
        matches!(self, ExportFormat::Gif)
    }
}

/// Something a format cannot carry, in the same spirit as
/// [`crate::docimport::ImportWarning`]: an operation that loses something has
/// to admit it before it happens rather than after.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportLoss {
    /// The document has transparency and the format has none, so the picture is
    /// composited onto the matte colour.
    Flattened,
    /// Reduced to 256 colours by a quantiser.
    Palette,
    /// Lossy compression: the pixels written are not the pixels held.
    Lossy,
}

impl std::fmt::Display for ExportLoss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Flattened => write!(
                f,
                "This format holds no transparency, so the picture is painted \
                 onto the colour below before it is written."
            ),
            Self::Palette => write!(
                f,
                "GIF holds 256 colours. Everything else is approximated by the \
                 nearest of them, and gradients will band."
            ),
            Self::Lossy => write!(
                f,
                "JPEG discards detail to make the file small, and discards more \
                 of it every time one is saved."
            ),
        }
    }
}

/// What choosing `format` for this document will cost.
///
/// `transparent` is whether the flattened document actually has any
/// transparency in it — which, because the background composites inside the
/// export pass, is exactly "the document's background is transparent". A
/// white-backed document loses nothing by going to JPEG's lack of alpha, and
/// saying it does would train people to ignore the warning that matters.
pub fn losses(format: ExportFormat, transparent: bool) -> Vec<ExportLoss> {
    let mut out = Vec::new();
    if transparent && !format.carries_alpha() {
        out.push(ExportLoss::Flattened);
    }
    if format.quantises() {
        out.push(ExportLoss::Palette);
    }
    if format == ExportFormat::Jpeg {
        out.push(ExportLoss::Lossy);
    }
    out
}

/// Whether the matte colour is a live control for this document and format.
///
/// False both when the format keeps alpha and when the document has none —
/// in either case the matte would be a knob that changes no pixel, and a
/// control that does nothing is worse than one that is not drawn.
pub fn needs_matte(format: ExportFormat, transparent: bool) -> bool {
    transparent && !format.carries_alpha()
}

/// Everything the encoder needs beyond the pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    /// JPEG quality, 1–100. Ignored by every other format.
    pub quality: u8,
    /// sRGB colour transparent pixels are composited onto, for a format that
    /// cannot carry alpha. White by default: silently flattening onto black is
    /// the classic version of this bug, and it is the one that makes a drawing
    /// look ruined rather than merely different.
    pub matte: [u8; 3],
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ExportFormat::Png,
            quality: 90,
            matte: [255, 255, 255],
        }
    }
}

/// Why an export could not be encoded.
#[derive(Debug)]
pub enum ExportError {
    /// The pixel buffer is not `width × height × 4` bytes.
    WrongSize { found: usize, expected: usize },
    /// The encoder refused. Its own message, already a sentence.
    Encoder(String),
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongSize { found, expected } => write!(
                f,
                "The flattened image came back as {found} bytes where {expected} were \
                 expected, so nothing was written."
            ),
            Self::Encoder(e) => write!(f, "The image could not be encoded: {e}"),
        }
    }
}

impl std::error::Error for ExportError {}

/// Encode a flattened document.
///
/// `pixels` is straight-alpha sRGB RGBA8 — exactly what
/// `CanvasRenderer::export_rgba` hands back — and the result is a whole file,
/// ready for `docformat::write_encoded` to put on disk in one piece. Encoding
/// to memory rather than to a file is what lets the atomic write already in the
/// tree be reused instead of a second temp-and-rename being invented here.
pub fn encode(
    pixels: &[u8],
    width: u32,
    height: u32,
    options: &ExportOptions,
) -> Result<Vec<u8>, ExportError> {
    let expected = width as usize * height as usize * 4;
    if pixels.len() != expected {
        return Err(ExportError::WrongSize {
            found: pixels.len(),
            expected,
        });
    }

    match options.format {
        ExportFormat::Png => encode_png(pixels, width, height),
        // Routed by what the format can carry rather than by naming the three
        // that cannot: a format added to the `false` group of `carries_alpha`
        // then gets the matte automatically, instead of silently writing an
        // alpha channel nothing will read.
        format if format.carries_alpha() => encode_via_image(
            pixels,
            width,
            height,
            image::ExtendedColorType::Rgba8,
            options,
        ),
        _ => {
            let rgb = matte_over(pixels, options.matte);
            encode_via_image(&rgb, width, height, image::ExtendedColorType::Rgb8, options)
        }
    }
}

/// Straight-alpha sRGB RGBA8 out as a PNG.
///
/// The `png` crate directly rather than `image`'s wrapper around it, for the
/// `sRGB` chunk: the composite shader gamma-encodes on the way out, so the
/// bytes *are* sRGB and the file should say so.
fn encode_png(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, ExportError> {
    let mut out = Vec::new();
    png_into(&mut out, pixels, width, height).map_err(|e| ExportError::Encoder(e.to_string()))?;
    Ok(out)
}

fn png_into(
    out: &mut Vec<u8>,
    pixels: &[u8],
    width: u32,
    height: u32,
) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.write_header()?.write_image_data(pixels)
}

/// The four formats `image` writes for us: RGBA8 for the one that keeps alpha,
/// RGB8 — already matted — for the three that do not.
fn encode_via_image(
    buffer: &[u8],
    width: u32,
    height: u32,
    colour: image::ExtendedColorType,
    options: &ExportOptions,
) -> Result<Vec<u8>, ExportError> {
    use image::ImageEncoder;

    let mut out = std::io::Cursor::new(Vec::new());
    let result = match options.format {
        // Clamped rather than trusted: `quality` is a `u8` and the encoder
        // panics on zero.
        ExportFormat::Jpeg => image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut out,
            options.quality.clamp(1, 100),
        )
        .write_image(buffer, width, height, colour),
        ExportFormat::Tiff => image::codecs::tiff::TiffEncoder::new(&mut out)
            .write_image(buffer, width, height, colour),
        ExportFormat::Bmp => {
            image::codecs::bmp::BmpEncoder::new(&mut out).write_image(buffer, width, height, colour)
        }
        ExportFormat::Gif => {
            image::codecs::gif::GifEncoder::new(&mut out).encode(buffer, width, height, colour)
        }
        // PNG never reaches here; it keeps the `png` crate directly.
        ExportFormat::Png => unreachable!("PNG is encoded by `encode_png`"),
    };
    result.map_err(|e| ExportError::Encoder(e.to_string()))?;
    Ok(out.into_inner())
}

/// Composite straight-alpha sRGB RGBA8 onto an opaque sRGB colour, giving RGB8.
///
/// The blend is done in **linear** light and encoded once at the end. Lerping
/// the sRGB bytes instead is the same mistake `probe_canvas` documents on the
/// way in — the mean of two gamma-encoded values is not the encoding of their
/// mean — and it shows up as a halo round every soft edge.
///
/// Every output byte is a function of one source byte, one alpha and the fixed
/// matte, so there are only 65 536 of them per channel: three tables built with
/// a few hundred thousand `powf` calls, and then a lookup per component rather
/// than a `powf` per component. On a 10 000² canvas that is the difference
/// between a table and three hundred million transcendental calls.
fn matte_over(rgba: &[u8], matte: [u8; 3]) -> Vec<u8> {
    let tables: Vec<Vec<u8>> = matte.iter().map(|m| matte_table(*m)).collect();
    let mut out = vec![0u8; rgba.len() / 4 * 3];
    for (px, dst) in rgba.chunks_exact(4).zip(out.chunks_exact_mut(3)) {
        let a = px[3] as usize * 256;
        for c in 0..3 {
            dst[c] = tables[c][a + px[c] as usize];
        }
    }
    out
}

/// `[alpha][value] -> byte` for one channel over one matte component.
fn matte_table(matte: u8) -> Vec<u8> {
    let back = srgb_to_linear(matte as f32 / 255.0);
    let mut t = vec![0u8; 256 * 256];
    for a in 0..256usize {
        let alpha = a as f32 / 255.0;
        for v in 0..256usize {
            let front = srgb_to_linear(v as f32 / 255.0);
            let mixed = front * alpha + back * (1.0 - alpha);
            t[a * 256 + v] = (linear_to_srgb(mixed) * 255.0 + 0.5) as u8;
        }
    }
    t
}

/// The name to suggest for a document titled `title`.
///
/// The tab is named after its file once it has one, so the extension has to
/// come off or an exported `sketch.ora` is offered as `sketch.ora.png`. Any
/// extension Umber reads or writes is dropped; anything else is left alone,
/// because a document deliberately called `study.v2` is not a `.v2` file.
pub fn default_file_name(title: &str, format: ExportFormat) -> String {
    let stem = title.rsplit_once('.').map_or(title, |(stem, ext)| {
        if !stem.is_empty() && KNOWN_EXTENSIONS.iter().any(|k| ext.eq_ignore_ascii_case(k)) {
            stem
        } else {
            title
        }
    });
    let stem = stem.trim();
    let stem = if stem.is_empty() { "untitled" } else { stem };
    format!("{stem}.{}", format.extension())
}

/// Extensions `default_file_name` will take off a document's title: everything
/// [`crate::docimport`] opens, plus everything this module writes.
const KNOWN_EXTENSIONS: &[&str] = &[
    "ora", "kra", "psd", "png", "jpg", "jpeg", "tif", "tiff", "gif", "bmp", "webp",
];

/// Where an export is actually going, once the chosen format has had its say
/// about the name that came back from the file dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportTarget {
    pub path: PathBuf,
    /// The format the typed name named, when that was not the format chosen.
    ///
    /// `Some` means the user typed `art.jpg` with PNG selected. The file is
    /// still written in the chosen format, under a name that ends in the chosen
    /// format's extension — but the disagreement is handed back so the caller
    /// can say what happened. Guessing the format from the name instead would
    /// mean a dialog whose format picker is silently overruled by a filename.
    pub named: Option<ExportFormat>,
}

/// Resolve a picked path against the chosen format.
///
/// The extension is **appended, never substituted**, which is the same rule
/// `app.rs`'s `with_extension` keeps for documents and for the same reason:
/// substituting would turn a deliberate `study.v2` into `study.png`, quietly
/// renaming the file somebody asked for. Not every platform's save dialog
/// appends the filter's extension either, so a file written as plain `art`
/// would be one nothing could identify by name.
pub fn target(path: &Path, format: ExportFormat) -> ExportTarget {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    if format
        .extensions()
        .iter()
        .any(|e| ext.eq_ignore_ascii_case(e))
    {
        return ExportTarget {
            path: path.to_path_buf(),
            named: None,
        };
    }
    let mut name = path.to_path_buf().into_os_string();
    name.push(".");
    name.push(format.extension());
    ExportTarget {
        path: PathBuf::from(name),
        named: ExportFormat::from_extension(&ext),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny checkerboard: one opaque red, one half-transparent green, one
    /// fully transparent, one opaque white.
    fn sample() -> Vec<u8> {
        vec![
            255, 0, 0, 255, // opaque red
            0, 255, 0, 128, // half-transparent green
            0, 0, 255, 0, // invisible blue
            255, 255, 255, 255, // opaque white
        ]
    }

    fn decode(bytes: &[u8]) -> image::RgbaImage {
        image::load_from_memory(bytes)
            .expect("the encoder produced something no decoder accepts")
            .to_rgba8()
    }

    #[test]
    fn every_format_has_a_distinct_extension_that_names_it_back() {
        for format in ExportFormat::ALL {
            for ext in format.extensions() {
                assert_eq!(
                    ExportFormat::from_extension(ext),
                    Some(format),
                    "{ext} did not name {format:?}"
                );
                // A file dialog on Windows hands back whatever case the user
                // typed, and `.PNG` is a PNG.
                assert_eq!(
                    ExportFormat::from_extension(&ext.to_uppercase()),
                    Some(format)
                );
            }
            assert!(
                format.extensions().contains(&format.extension()),
                "{format:?}'s canonical extension is not one of its own"
            );
        }
        assert_eq!(ExportFormat::from_extension("ora"), None);
        assert_eq!(ExportFormat::from_extension(""), None);
    }

    #[test]
    fn a_documents_title_loses_only_an_extension_umber_knows() {
        assert_eq!(
            default_file_name("sketch.ora", ExportFormat::Png),
            "sketch.png"
        );
        assert_eq!(
            default_file_name("sketch.ora", ExportFormat::Jpeg),
            "sketch.jpg"
        );
        assert_eq!(
            default_file_name("portrait.psd", ExportFormat::Tiff),
            "portrait.tif"
        );
        // Not an extension anybody reads: it is part of the name.
        assert_eq!(
            default_file_name("study.v2", ExportFormat::Png),
            "study.v2.png"
        );
        assert_eq!(
            default_file_name("Untitled", ExportFormat::Gif),
            "Untitled.gif"
        );
        // A dotfile is a name, not an extension with nothing in front of it.
        assert_eq!(default_file_name(".png", ExportFormat::Png), ".png.png");
        assert_eq!(default_file_name("  ", ExportFormat::Bmp), "untitled.bmp");
    }

    #[test]
    fn a_picked_name_only_gains_an_extension_it_does_not_already_have() {
        let png = ExportFormat::Png;
        assert_eq!(target(Path::new("a.png"), png).path, Path::new("a.png"));
        assert_eq!(target(Path::new("a.PNG"), png).path, Path::new("a.PNG"));
        assert_eq!(target(Path::new("a"), png).path, Path::new("a.png"));
        // Both spellings of JPEG are the format's own, so neither is doubled.
        for name in ["a.jpg", "a.jpeg"] {
            let t = target(Path::new(name), ExportFormat::Jpeg);
            assert_eq!(t.path, Path::new(name));
            assert_eq!(t.named, None);
        }
    }

    #[test]
    fn a_name_that_disagrees_with_the_format_is_reported_not_obeyed() {
        // The picker said PNG; the typed name says JPEG. The file is a PNG —
        // the format control is not overruled by a filename — and the caller
        // is handed what the name claimed so it can say so.
        let t = target(Path::new("art.jpg"), ExportFormat::Png);
        assert_eq!(t.path, Path::new("art.jpg.png"));
        assert_eq!(t.named, Some(ExportFormat::Jpeg));
        // An extension that names nothing Umber writes is just part of the
        // name, and there is nothing to report.
        let t = target(Path::new("study.v2"), ExportFormat::Png);
        assert_eq!(t.path, Path::new("study.v2.png"));
        assert_eq!(t.named, None);
    }

    #[test]
    fn a_format_only_admits_to_what_it_actually_costs_this_document() {
        // The whole point of taking `transparent`: an opaque document loses
        // nothing to JPEG's lack of alpha, and warning about it anyway is how
        // people learn to click through warnings.
        assert_eq!(losses(ExportFormat::Png, true), vec![]);
        assert_eq!(losses(ExportFormat::Tiff, true), vec![]);
        assert_eq!(losses(ExportFormat::Jpeg, false), vec![ExportLoss::Lossy]);
        assert_eq!(
            losses(ExportFormat::Jpeg, true),
            vec![ExportLoss::Flattened, ExportLoss::Lossy]
        );
        assert_eq!(losses(ExportFormat::Bmp, false), vec![]);
        assert_eq!(losses(ExportFormat::Bmp, true), vec![ExportLoss::Flattened]);
        // GIF quantises whether or not there is any transparency to lose.
        assert_eq!(losses(ExportFormat::Gif, false), vec![ExportLoss::Palette]);
        assert_eq!(
            losses(ExportFormat::Gif, true),
            vec![ExportLoss::Flattened, ExportLoss::Palette]
        );

        for format in ExportFormat::ALL {
            assert_eq!(
                needs_matte(format, true),
                !format.carries_alpha(),
                "{format:?}"
            );
            assert!(!needs_matte(format, false), "{format:?}");
        }
    }

    #[test]
    fn an_alpha_carrying_format_round_trips_a_transparent_pixel() {
        for format in ExportFormat::ALL.into_iter().filter(|f| f.carries_alpha()) {
            let options = ExportOptions {
                format,
                ..Default::default()
            };
            let bytes = encode(&sample(), 2, 2, &options).expect("encode");
            let back = decode(&bytes);
            assert_eq!(back.dimensions(), (2, 2), "{format:?}");
            assert_eq!(
                back.as_raw().as_slice(),
                sample().as_slice(),
                "{format:?} did not give back the pixels it was handed"
            );
        }
    }

    #[test]
    fn an_alpha_less_format_puts_the_matte_where_the_transparency_was() {
        // The other half of the pair above, and the reason the dialog says so:
        // these formats cannot come back with what they were given.
        for format in ExportFormat::ALL.into_iter().filter(|f| !f.carries_alpha()) {
            let options = ExportOptions {
                format,
                // Not white, so a matte that was silently ignored — or applied
                // as black — is a failure rather than a coincidence.
                matte: [0, 0, 255],
                quality: 100,
            };
            let bytes = encode(&sample(), 2, 2, &options).expect("encode");
            let back = decode(&bytes);
            assert_eq!(back.dimensions(), (2, 2), "{format:?}");
            for px in back.pixels() {
                assert_eq!(px.0[3], 255, "{format:?} claimed to keep transparency");
            }
            // The invisible pixel is the matte. GIF quantises and JPEG is
            // lossy, so this is "near" rather than "equal" — but a matte
            // ignored, or applied in the wrong space, misses by far more.
            let px = back.get_pixel(0, 1).0;
            for c in 0..3 {
                let d = (px[c] as i32 - options.matte[c] as i32).abs();
                assert!(d <= 8, "{format:?} put {px:?} where the matte should be");
            }
        }
    }

    #[test]
    fn a_matte_is_mixed_in_linear_light() {
        // Blending is linear, so half of white over black is sRGB ~188 rather
        // than 128 — the identity the composite tests are held to, and the one
        // that goes wrong if the bytes are lerped directly.
        let half_white = [255, 255, 255, 128];
        let out = matte_over(&half_white, [0, 0, 0]);
        assert!(
            (185..=191).contains(&out[0]),
            "half white over black came out {}",
            out[0]
        );

        // The two ends are exact, whatever the matte: nothing is composited at
        // all, so no rounding can creep in.
        for matte in [[255, 255, 255], [0, 0, 0], [17, 200, 90]] {
            let opaque = matte_over(&[10, 120, 250, 255], matte);
            assert_eq!(opaque, vec![10, 120, 250]);
            let gone = matte_over(&[10, 120, 250, 0], matte);
            assert_eq!(gone, matte.to_vec());
        }
    }

    #[test]
    fn jpeg_quality_is_a_control_that_does_something() {
        // A quality slider that changed no byte would be exactly the kind of
        // dead control the interface rules forbid.
        let pixels: Vec<u8> = (0..64 * 64 * 4)
            .map(|i| ((i * 37) % 251) as u8)
            .collect::<Vec<_>>();
        let small = encode(
            &pixels,
            64,
            64,
            &ExportOptions {
                format: ExportFormat::Jpeg,
                quality: 10,
                ..Default::default()
            },
        )
        .expect("encode");
        let large = encode(
            &pixels,
            64,
            64,
            &ExportOptions {
                format: ExportFormat::Jpeg,
                quality: 100,
                ..Default::default()
            },
        )
        .expect("encode");
        assert!(
            small.len() < large.len(),
            "{} bytes at quality 10 against {} at 100",
            small.len(),
            large.len()
        );
        // Zero is not a quality the encoder accepts, and a `u8` can hold it.
        assert!(
            encode(
                &pixels,
                64,
                64,
                &ExportOptions {
                    format: ExportFormat::Jpeg,
                    quality: 0,
                    ..Default::default()
                }
            )
            .is_ok()
        );
    }

    #[test]
    fn an_export_lands_whole_or_not_at_all() {
        // The export reuses `docformat::write_encoded` rather than writing a
        // second temp-and-rename of its own, so what is checked here is that
        // the reuse actually holds: the file appears complete, the temporary
        // neighbour is gone, and it was named after the *export's* extension
        // rather than after `.ora` — which is what stops it colliding with the
        // save of a document sitting beside it.
        let dir = std::env::temp_dir().join(format!("umber-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("landing.png");
        // Something already there, so this is a replacement rather than a
        // first write: the case where a half-written file would destroy work.
        std::fs::write(&path, b"an older picture").unwrap();

        let bytes = encode(&sample(), 2, 2, &ExportOptions::default()).expect("encode");
        crate::docformat::write_encoded(&path, &bytes).expect("write");

        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(decode(&std::fs::read(&path).unwrap()).dimensions(), (2, 2));
        assert!(
            !dir.join("landing.png.saving").exists(),
            "the temporary file was left behind"
        );
        assert!(
            !dir.join("landing.ora.saving").exists(),
            "the temporary was named after the document format, not the export"
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn a_buffer_that_is_not_the_canvas_is_refused_rather_than_encoded() {
        // The pixels come off the GPU, and a readback that came back short is
        // a bug worth a message rather than a file with a torn picture in it.
        let err = encode(&sample(), 4, 4, &ExportOptions::default()).unwrap_err();
        assert!(matches!(
            err,
            ExportError::WrongSize {
                found: 16,
                expected: 64
            }
        ));
    }
}
