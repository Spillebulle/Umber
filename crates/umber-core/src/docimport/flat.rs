//! PNG decoding, and the flat single-layer import built on it.
//!
//! ORA stores every layer as a PNG, so this is shared code rather than a
//! convenience feature.

use glam::UVec2;

use super::{
    ImportError, ImportedDocument, ImportedLayer, PixelPiece, SourceFormat, StackSize,
    check_bounds, check_image_size, srgb,
};
use crate::document::Background;
use crate::layer::BlendMode;

/// A decoded image: straight-alpha, sRGB-encoded RGBA8.
pub struct Image {
    pub size: UVec2,
    pub rgba: Vec<u8>,
}

/// Decode a PNG to RGBA8, whatever colour type it was written in.
///
/// Palette, grey and 16-bit images are all normalised on the way through: the
/// png crate expands palettes and strips 16-bit samples, and the grey and
/// no-alpha cases are widened here. Colour management is deliberately not
/// attempted — PNG can carry an ICC profile or a gamma chunk, and honouring
/// those properly means a colour engine Umber does not have. ORA and Krita both
/// specify sRGB, which is also what a PNG without a profile means, so sRGB is
/// what is assumed.
///
/// **The size is checked off the header before the buffer is made**, because
/// `output_buffer_size` is a figure the file chooses and `vec![0; …]` on a
/// figure that large aborts rather than failing. [`check_image_size`] has the
/// whole argument, including why `png::Limits` does not reach it.
pub fn decode_png(bytes: &[u8], format: SourceFormat) -> Result<Image, ImportError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    decoder.set_limits(png::Limits {
        bytes: ImportedDocument::MAX_TOTAL_BYTES as usize,
    });

    let malformed = |detail: String| ImportError::Malformed { format, detail };

    let mut reader = decoder
        .read_info()
        .map_err(|e| malformed(format!("the PNG header could not be read ({e})")))?;
    // Before `output_buffer_size`, not after: that call is what would hand back
    // the figure, and this is the one place it can be refused without having
    // been believed first.
    let (width, height) = reader.info().size();
    check_image_size(width, height)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("the PNG is too large to decode".into()))?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| malformed(format!("the PNG could not be decoded ({e})")))?;

    let pixels = info.width as usize * info.height as usize;
    let rgba = match info.color_type {
        png::ColorType::Rgba => {
            buf.truncate(pixels * 4);
            buf
        }
        png::ColorType::Rgb => widen(&buf[..pixels * 3], 3, |px| [px[0], px[1], px[2], 255]),
        png::ColorType::GrayscaleAlpha => {
            widen(&buf[..pixels * 2], 2, |px| [px[0], px[0], px[0], px[1]])
        }
        png::ColorType::Grayscale => widen(&buf[..pixels], 1, |px| [px[0], px[0], px[0], 255]),
        // EXPAND turns indexed into RGB or RGBA, so this is unreachable in
        // practice; refusing beats guessing if the crate ever changes.
        other => {
            return Err(ImportError::Unsupported {
                format,
                detail: format!("a {other:?} PNG"),
            });
        }
    };

    Ok(Image {
        size: UVec2::new(info.width, info.height),
        rgba,
    })
}

fn widen(src: &[u8], stride: usize, f: impl Fn(&[u8]) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / stride * 4);
    for px in src.chunks_exact(stride) {
        out.extend_from_slice(&f(px));
    }
    out
}

/// Import a flat PNG as a one-layer document.
///
/// Worth having for its own sake: it is how a reference photo or a scan of a
/// pencil drawing gets onto the canvas, and it is the only import that can
/// never lose anything.
pub fn read_png(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let format = SourceFormat::Png;
    let image = decode_png(bytes, format)?;
    // A flat picture is one layer and no folders.
    let mut budget = check_bounds(
        format,
        image.size.x,
        image.size.y,
        StackSize::all_painted(1),
    )?;

    let mut pixels = image.rgba;
    srgb::encode_buffer(&mut pixels);

    // **One piece covering the canvas, because a flat picture *is* the
    // canvas.** There is nothing sparse to find: the PNG decoded to exactly
    // this rectangle and every pixel of it came out of the file.
    let layer = ImportedLayer::new(
        "Image",
        BlendMode::Normal,
        vec![PixelPiece::whole(image.size, pixels)],
    );
    budget.charge(&layer)?;

    Ok(ImportedDocument {
        format,
        size: image.size,
        layers: vec![layer],
        active: None,
        background: Background::Transparent,
        dpi: None,
        history: None,
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixtures;
    use super::*;

    #[test]
    fn a_flat_png_becomes_one_opaque_layer() {
        let png = fixtures::png_rgba(2, 1, &[255, 0, 0, 255, 0, 255, 0, 255]);
        let doc = read_png(&png).unwrap();

        assert_eq!(doc.size, UVec2::new(2, 1));
        assert_eq!(doc.layers.len(), 1);
        assert!(doc.warnings.is_empty(), "a flat PNG loses nothing");
        // Opaque pixels survive the colour-space conversion byte for byte.
        assert_eq!(
            doc.layers[0].dense(UVec2::new(2, 1)),
            vec![255, 0, 0, 255, 0, 255, 0, 255]
        );
    }

    #[test]
    fn greyscale_and_rgb_pngs_widen_to_rgba() {
        let grey = fixtures::png_grey(2, 1, &[0, 255]);
        let doc = read_png(&grey).unwrap();
        assert_eq!(
            doc.layers[0].dense(UVec2::new(2, 1)),
            vec![0, 0, 0, 255, 255, 255, 255, 255]
        );

        let rgb = fixtures::png_rgb(1, 1, &[10, 20, 30]);
        let doc = read_png(&rgb).unwrap();
        assert_eq!(doc.layers[0].dense(UVec2::new(1, 1)), vec![10, 20, 30, 255]);
    }

    #[test]
    fn transparency_is_premultiplied_on_the_way_in() {
        let png = fixtures::png_rgba(1, 1, &[255, 255, 255, 128]);
        let doc = read_png(&png).unwrap();
        let pixels = doc.layers[0].dense(UVec2::new(1, 1));
        assert!(
            (pixels[0] as i32 - 188).abs() <= 1,
            "got {pixels:?} — the layer texture wants premultiplied linear colour"
        );
    }

    #[test]
    fn a_file_that_is_not_a_png_is_refused() {
        let err = read_png(b"not a png at all").unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }

    /// **A PNG header alone cannot choose an allocation.**
    ///
    /// The fixture is a valid IHDR, an **empty** IDAT and an IEND — forty-odd
    /// bytes — claiming one pixel past [`ImportedDocument::MAX_DIMENSION`] on a
    /// side, where `output_buffer_size` answers `Some(4_295_098_372)` and
    /// `vec![0u8; …]` **aborts** rather than failing, taking the panic hook, the
    /// crash report and the autosave with it.
    ///
    /// **Refusing on the header rather than on the frame is what is being
    /// asserted, and the fixture is what makes that legible.** The IDAT is empty
    /// — present so `read_info` will parse the file at all, holding nothing — so
    /// a reader that allocated first and asked afterwards fails inside
    /// `next_frame` with a *decode* error, a different variant with the
    /// allocation already spent. Only a check off `reader.info()` produces
    /// `ImageTooLarge`. Demonstrated by mutation: delete the call and this reads
    /// `Malformed { detail: "the PNG could not be decoded (Corrupt deflate
    /// stream. InsufficientInput)" }`.
    ///
    /// The case is derived from the constant rather than written out, so raising
    /// the ceiling moves the case with it instead of leaving one that no longer
    /// exercises itself — `a_lock_that_saturates_stops_being_the_shape` is the
    /// recorded reason.
    #[test]
    fn a_png_header_alone_cannot_ask_for_an_allocation() {
        // `Image` holds a buffer and derives no `Debug`, so the refusal is read
        // out rather than unwrapped — which also keeps a failure from printing
        // a canvas.
        let refusal = |w: u32, h: u32| match decode_png(
            &fixtures::png_header_only(w, h),
            SourceFormat::Png,
        ) {
            Ok(_) => panic!("a {w}×{h} header decoded, which needs the file to hold pixels"),
            Err(e) => e,
        };

        let edge = ImportedDocument::MAX_DIMENSION + 1;
        for (w, h) in [(edge, 1), (1, edge), (edge, edge)] {
            let err = refusal(w, h);
            assert!(
                matches!(err, ImportError::ImageTooLarge { width, height } if width == w && height == h),
                "a {w}×{h} header was not refused off the header: {err:?}"
            );
        }

        // **And the bound is not off by one, which is the half that keeps the
        // reader from being stricter than the writer.** A picture exactly at the
        // ceiling gets past the size check and fails on the missing frame
        // instead. One pixel tall, so the assertion costs 131 KB rather than
        // four gigabytes.
        let err = refusal(ImportedDocument::MAX_DIMENSION, 1);
        assert!(
            matches!(err, ImportError::Malformed { .. }),
            "a picture at exactly the ceiling was refused for its size: {err:?}"
        );
    }
}
