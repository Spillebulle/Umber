//! The flattened picture a document already carries.
//!
//! A file manager wants one small image per document and wants it in
//! milliseconds, on a machine that may be drawing a folder of two hundred. What
//! it must **not** provoke is a full import: [`super::import`] decodes every
//! layer into a canvas-sized buffer, which is 12.3 GB for one file this was
//! measured against, and it exists to hand a document to the *editor*.
//!
//! It does not have to. **Every format Umber reads already stores a flattened
//! preview**, because every application that wrote one needed the same thing:
//!
//! | | where |
//! |---|---|
//! | `.ora` | `mergedimage.png`, which the specification requires |
//! | `.kra` | `mergedimage.png`, beside the layer tiles |
//! | `.clip` | the `CanvasPreview` table's `ImageData`, a PNG |
//! | `.psd` | the composite section, which is what `Psd::rgba` returns |
//! | `.png` | itself |
//!
//! So this reads one entry and decodes one image. Nothing here walks a layer
//! stack, allocates a canvas, or touches the GPU — which is what makes it safe
//! to call from a Windows shell extension, where the code runs inside
//! Explorer's own surrogate process and a slow or crashing provider is
//! everybody's problem rather than Umber's.
//!
//! **This is deliberately not the same picture the composite would produce.**
//! It is whatever the writing application last saved, so a document edited by
//! something that did not refresh its preview shows a stale one, and a `.clip`
//! preview is Clip Studio's rendering rather than Umber's. That is the right
//! trade for a thumbnail and the wrong one for anything else: nothing that
//! decides pixels may read this.

use std::path::Path;

use glam::UVec2;

use super::container;
use super::{ImportError, SourceFormat};
use crate::sqlite::{Database, Value};

/// A flattened picture, straight-alpha sRGB RGBA8.
///
/// The same form [`super::ImportedLayer::pixels`] is *not* in — that one is
/// premultiplied, because it goes to a layer texture. This goes to a file
/// manager, so it is what a PNG holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub size: UVec2,
    pub rgba: Vec<u8>,
}

impl Preview {
    /// Build one, checking that the buffer is the size it claims.
    ///
    /// Private, and the check is why: every arm below gets its dimensions and
    /// its bytes from different places in somebody else's file, and a preview
    /// whose buffer is short is an out-of-bounds read in whatever draws it.
    fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ImportError> {
        let wanted = width as usize * height as usize * 4;
        if width == 0 || height == 0 || rgba.len() != wanted {
            return Err(ImportError::Malformed {
                format: SourceFormat::Png,
                detail: format!(
                    "its preview claims {width}×{height} and holds {} bytes rather than {wanted}",
                    rgba.len()
                ),
            });
        }
        Ok(Self {
            size: UVec2::new(width, height),
            rgba,
        })
    }

    /// Shrink so that neither edge is past `max_edge`, keeping the proportions.
    ///
    /// **Never enlarges.** A thumbnail host asks for a box to fit inside, and a
    /// small preview blown up to fill it is a blurred picture claiming detail
    /// the file does not have — the rule `thumbnail.wgsl`'s frame already keeps
    /// for a layer's own thumbnail.
    #[must_use]
    pub fn fit_within(self, max_edge: u32) -> Self {
        let (w, h) = (self.size.x, self.size.y);
        if max_edge == 0 || (w <= max_edge && h <= max_edge) {
            return self;
        }
        // Rounded up so neither edge collapses to nothing on a very long thin
        // canvas: 15000×5000 into a 64 box is 64×21, and a panorama into a
        // small box would otherwise be 64×0.
        let scale = f64::from(max_edge) / f64::from(w.max(h));
        let to_w = ((f64::from(w) * scale).round() as u32).max(1);
        let to_h = ((f64::from(h) * scale).round() as u32).max(1);

        // `image`'s resampler rather than a hand-written one: this is a
        // downscale of an ordinary sRGB image, which is exactly what that crate
        // is for, and it costs no new dependency and no codec feature —
        // `imageops` is not feature-gated, only the decoders are. A box filter
        // of our own would be a second resampler in a codebase that refuses
        // them everywhere else.
        let Some(source) = image::RgbaImage::from_raw(w, h, self.rgba) else {
            // Unreachable: `new` already checked the length. Returning the
            // original rather than panicking, because this runs inside
            // Explorer.
            return Self {
                size: UVec2::new(w, h),
                rgba: Vec::new(),
            };
        };
        let scaled =
            image::imageops::resize(&source, to_w, to_h, image::imageops::FilterType::Triangle);
        Self {
            size: UVec2::new(to_w, to_h),
            rgba: scaled.into_raw(),
        }
    }
}

/// The preview in the document at `path`.
///
/// Dispatches on the extension exactly as [`super::import`] does, and refuses
/// the same way for a name it does not know.
pub fn from_path(path: &Path) -> Result<Preview, ImportError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !super::supported_extensions().contains(&ext.as_str()) {
        return Err(ImportError::UnsupportedExtension(ext));
    }
    let bytes = std::fs::read(path)?;
    from_bytes(&bytes, format_of(&ext))
}

/// Which reader an extension belongs to.
///
/// Separate from the dispatch in [`from_path`] because the shell extension
/// arrives with **bytes and no name**: Windows hands a thumbnail provider an
/// `IStream`, so the format has to be nameable independently of a path.
pub fn format_of(extension: &str) -> SourceFormat {
    match extension.to_ascii_lowercase().as_str() {
        "kra" => SourceFormat::Krita,
        "psd" => SourceFormat::Photoshop,
        "clip" => SourceFormat::ClipStudio,
        "png" => SourceFormat::Png,
        // ORA is the fallback rather than an arm of its own, because it is
        // Umber's own format and the one a caller with nothing to go on should
        // try. A file that is not one fails in the reader with its own words.
        _ => SourceFormat::OpenRaster,
    }
}

/// The preview in `bytes`, read as `format`.
pub fn from_bytes(bytes: &[u8], format: SourceFormat) -> Result<Preview, ImportError> {
    match format {
        // Both are ZIPs carrying a `mergedimage.png`; ORA's specification
        // requires it and Krita writes one beside its tiles. One arm, because
        // two identical ones is the drift `docformat`'s "never a second ORA
        // reader" refuses in miniature.
        SourceFormat::OpenRaster | SourceFormat::Krita => merged_image(bytes, format),
        SourceFormat::ClipStudio => clip_preview(bytes),
        SourceFormat::Photoshop => psd_composite(bytes),
        SourceFormat::Png => decode_png(bytes, SourceFormat::Png),
    }
}

/// `mergedimage.png` out of an ORA or a `.kra`.
fn merged_image(bytes: &[u8], format: SourceFormat) -> Result<Preview, ImportError> {
    let mut zip = container::open(bytes, format)?;
    // Krita also writes `preview.png`, which is small and always present; ORA
    // does not. Tried second so a full-size merge wins where there is one.
    for name in ["mergedimage.png", "preview.png"] {
        if let Some(entry) = container::read_optional_entry(&mut zip, name, format)? {
            return decode_png(&entry, format);
        }
    }
    Err(ImportError::Unsupported {
        format,
        detail: "a document with no flattened preview in it".to_string(),
    })
}

/// The `CanvasPreview` row of a `.clip`.
///
/// One row, one PNG. Measured across 33 real documents: every one carries it,
/// at between 1250 and 1920 pixels on its long edge, which is more than any
/// thumbnail wants and is why [`Preview::fit_within`] exists.
fn clip_preview(bytes: &[u8]) -> Result<Preview, ImportError> {
    const FORMAT: SourceFormat = SourceFormat::ClipStudio;
    let malformed = |detail: &str| ImportError::Malformed {
        format: FORMAT,
        detail: detail.to_string(),
    };

    // The same chunk walk the document reader does, reused rather than copied.
    let database = super::clipstudio::database_chunk(bytes)?;
    let db = Database::open(database).map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: e.to_string(),
    })?;
    let table = super::clipstudio::table(&db, "CanvasPreview")?
        .ok_or_else(|| malformed("it has no CanvasPreview table"))?;
    let rows = db
        .rows(&table)
        .map_err(|e| malformed(&format!("its CanvasPreview table could not be read ({e})")))?;
    let row = rows
        .first()
        .ok_or_else(|| malformed("its CanvasPreview table is empty"))?;
    let data = table
        .column("ImageData")
        .map(|i| row.get(i))
        .and_then(Value::as_blob)
        .ok_or_else(|| malformed("its canvas preview holds no image"))?;
    // The stated `ImageWidth`/`ImageHeight` are deliberately not trusted over
    // the PNG's own header: they are two statements of one fact in a file
    // somebody else wrote, and the one the pixels actually have is the decoder's.
    decode_png(data, FORMAT)
}

/// A `.psd`'s composite section.
///
/// Every Photoshop file saved with "maximize compatibility" carries one, and
/// `Psd::rgba` is it — the same flattened picture Explorer would show if
/// Photoshop's own handler were installed. A file saved without it has no
/// layers either and this is then the only picture in it.
fn psd_composite(bytes: &[u8]) -> Result<Preview, ImportError> {
    let psd = psd::Psd::from_bytes(bytes).map_err(|e| ImportError::Malformed {
        format: SourceFormat::Photoshop,
        detail: e.to_string(),
    })?;
    Preview::new(psd.width(), psd.height(), psd.rgba())
}

/// Decode a PNG to straight-alpha RGBA8.
///
/// The `png` crate rather than `image`'s decoder, which is the rule the export
/// keeps for the writing direction and holds here for the same reason: one PNG
/// codec in this crate, and it is this one.
fn decode_png(bytes: &[u8], format: SourceFormat) -> Result<Preview, ImportError> {
    let malformed = |detail: String| ImportError::Malformed { format, detail };

    // `png` 0.18 wants `Read + Seek`; a slice is only `Read`.
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    // Ask the decoder for eight-bit RGBA whatever the file holds, so a
    // greyscale, palette or sixteen-bit preview needs no arm of its own.
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| malformed(format!("its preview is not a readable PNG ({e})")))?;
    // `None` where the frame's size does not fit a `usize`, which on a 32-bit
    // build is a preview too large to hold rather than a malformed one.
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("its preview is larger than this build can hold".to_string()))?;
    let mut buf = vec![0; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| malformed(format!("its preview could not be decoded ({e})")))?;
    buf.truncate(info.buffer_size());

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|g| [*g, *g, *g, 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf
            .chunks_exact(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        // `normalize_to_color8` expands a palette, so this is unreachable and
        // is a refusal rather than a panic for the reason everything here is.
        other => {
            return Err(malformed(format!(
                "its preview is {other:?}, which Umber cannot read"
            )));
        }
    };
    Preview::new(info.width, info.height, rgba)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{self, ClipLayer, OraLayer};
    use super::*;

    /// **The preview is the picture the file already had**, not one derived
    /// from the layers.
    ///
    /// The fixture's `CanvasPreview` is a size no canvas in it uses and a
    /// colour no layer holds, so a reader that composited, or took the first
    /// layer, or fell back to the canvas size, fails here rather than agreeing
    /// by coincidence. That is the whole claim this module rests on.
    #[test]
    fn a_clip_thumbnail_comes_out_of_the_preview_the_file_stores() {
        let bytes = fixtures::clip(
            300,
            300,
            &[ClipLayer::flat("Ink", 300, 300, [255, 0, 0, 255])],
        );
        let preview = from_bytes(&bytes, SourceFormat::ClipStudio).expect("a preview");
        assert_eq!(
            preview.size,
            UVec2::new(fixtures::CLIP_PREVIEW.0, fixtures::CLIP_PREVIEW.1)
        );
        assert_eq!(&preview.rgba[..4], &fixtures::CLIP_PREVIEW_PIXEL);
    }

    /// The same for an ORA, whose `mergedimage.png` the specification requires.
    #[test]
    fn an_ora_thumbnail_comes_out_of_the_merged_image() {
        let bytes = fixtures::ora(8, 8, &[OraLayer::new("only", 8, 8, &[255, 0, 0, 255])]);
        let preview = from_bytes(&bytes, SourceFormat::OpenRaster).expect("a preview");
        assert_eq!(preview.size, UVec2::new(8, 8));
        // The fixture's merged image is grey 9 and its one layer is red, so
        // this cannot have come from the layer.
        assert_eq!(&preview.rgba[..4], &[9, 9, 9, 255]);
    }

    /// **No import happens.** A thumbnail of a document with sixty layers on a
    /// large canvas must not allocate sixty canvases, which is what makes this
    /// safe to run inside a file manager.
    ///
    /// Driven at a canvas whose *import* is refused outright, so a preview that
    /// went anywhere near the import path could not merely be slow here, it
    /// would fail. The fixture stays small because a layer with a 1×1 bitmap
    /// costs nothing.
    ///
    /// **The refusal used to be the byte bound and no longer can be**, which is
    /// worth saying rather than quietly swapping: this was 64 layers at 10000²,
    /// refused as 25.6 GB, and the piece contract retired exactly that — those
    /// layers hold a pixel each and are now charged a pixel each. The canvas
    /// ceiling is the bound that still refuses off the header alone, and it is
    /// derived from `MAX_DIMENSION` rather than written out, so raising it
    /// moves this case with it instead of leaving one that no longer exercises
    /// itself.
    #[test]
    fn a_thumbnail_of_a_document_too_large_to_import_still_works() {
        let mut layers =
            vec![ClipLayer::flat("Ink", 1, 1, [255, 0, 0, 255]).placed((1, 1), (0, 0))];
        for _ in 0..63 {
            layers.push(ClipLayer::flat("More", 1, 1, [0, 255, 0, 255]).placed((1, 1), (0, 0)));
        }
        let edge = super::super::ImportedDocument::MAX_DIMENSION + 1;
        let bytes = fixtures::clip(edge, edge, &layers);

        assert!(
            super::super::clipstudio::read(&bytes, &|_, _| {}).is_err(),
            "this fixture is meant to be past what an import will accept"
        );
        let preview = from_bytes(&bytes, SourceFormat::ClipStudio).expect("a preview regardless");
        assert_eq!(
            preview.size,
            UVec2::new(fixtures::CLIP_PREVIEW.0, fixtures::CLIP_PREVIEW.1)
        );
    }

    /// A box is a bound, not a target: a preview smaller than the box comes
    /// back untouched rather than blown up into a blur.
    #[test]
    fn fitting_never_enlarges_and_keeps_the_proportions() {
        let small = Preview::new(4, 2, vec![9; 4 * 2 * 4]).expect("a preview");
        let same = small.clone().fit_within(256);
        assert_eq!(same, small, "a small preview is not scaled up");

        // 1920x1080 into 256 is 256x144, which is the ratio held to the pixel.
        let wide = Preview::new(1920, 1080, vec![0; 1920 * 1080 * 4]).expect("a preview");
        assert_eq!(wide.fit_within(256).size, UVec2::new(256, 144));

        // A long thin canvas must not lose an axis entirely. 2500x625 is the
        // real proportion of one of the documents this was measured against.
        let thin = Preview::new(2500, 625, vec![0; 2500 * 625 * 4]).expect("a preview");
        let fitted = thin.fit_within(64);
        assert_eq!(fitted.size, UVec2::new(64, 16));
        assert!(
            fitted.size.y >= 1,
            "an edge may never round away to nothing"
        );

        // The extreme of the same rule: a panorama whose short edge would
        // otherwise be zero.
        let panorama = Preview::new(10000, 5, vec![0; 10000 * 5 * 4]).expect("a preview");
        assert_eq!(panorama.fit_within(64).size, UVec2::new(64, 1));
    }

    /// **A page is taller than it is wide, and every case above is not.**
    ///
    /// "Neither edge is past `max_edge`" is a claim about both edges, and every
    /// preview the test above drives is landscape — so the scale could have
    /// been divided by the *width* rather than by the longer edge and nothing
    /// would have said so. Mutating `w.max(h)` to `w` left all 1,127 tests in
    /// this crate green, and would have given every A4-shaped document a
    /// thumbnail overflowing the box Explorer asked for: 181x256 into a 256
    /// box becomes 256x362.
    ///
    /// So this is portrait, and it asserts the bound rather than only the
    /// arithmetic — the same rule read the way a caller reads it. The square
    /// case is here for the same reason: it is the one shape under which the
    /// two readings agree, so its presence beside the others is what shows the
    /// others are doing work.
    #[test]
    fn a_page_taller_than_it_is_wide_still_fits_the_box() {
        // A4 at 181x256 is the proportion of the documents this is most often
        // asked for, and 256 is what a file manager asks for.
        let page = Preview::new(1810, 2560, vec![0; 1810 * 2560 * 4]).expect("a preview");
        assert_eq!(page.fit_within(256).size, UVec2::new(181, 256));

        // The mirror of the panorama above: a column, whose *width* would round
        // away to nothing.
        let column = Preview::new(5, 10000, vec![0; 5 * 10000 * 4]).expect("a preview");
        assert_eq!(column.fit_within(64).size, UVec2::new(1, 64));

        // Square is where dividing by the width and dividing by the longer edge
        // cannot be told apart, which is exactly why it is not on its own.
        let square = Preview::new(1000, 1000, vec![0; 1000 * 1000 * 4]).expect("a preview");
        assert_eq!(square.fit_within(64).size, UVec2::new(64, 64));

        // The property the caller actually depends on, over both orientations
        // and a box each side of the awkward ratios. The preview is built once
        // per shape and cloned per box rather than rebuilt: `fit_within`
        // consumes `self`, and two of these are 18.5 MB, so constructing inside
        // the inner loop is 150 MB of churn for a test whose subject is
        // arithmetic.
        for (w, h) in [(1810, 2560), (2560, 1810), (5, 10000), (10000, 5), (7, 9)] {
            let shape = Preview::new(w, h, vec![0; (w * h * 4) as usize]).expect("a preview");
            for box_edge in [1, 16, 64, 256] {
                let fitted = shape.clone().fit_within(box_edge);
                assert!(
                    fitted.size.x <= box_edge && fitted.size.y <= box_edge,
                    "{w}x{h} into {box_edge} came back {}x{}, which is past the \
                     box a thumbnail host asked it to fit inside",
                    fitted.size.x,
                    fitted.size.y
                );
                assert!(
                    fitted.size.x >= 1 && fitted.size.y >= 1,
                    "{w}x{h} into {box_edge} lost an axis"
                );
            }
        }
    }

    /// A buffer that does not match the size it claims is refused rather than
    /// handed on to whatever draws it.
    #[test]
    fn a_preview_shorter_than_it_claims_is_refused() {
        assert!(Preview::new(4, 4, vec![0; 10]).is_err());
        assert!(Preview::new(0, 4, vec![]).is_err());
        assert!(Preview::new(4, 4, vec![0; 4 * 4 * 4]).is_ok());
    }

    /// Every extension the importer reads has a format here, or a thumbnail
    /// would silently be attempted as an ORA.
    #[test]
    fn every_readable_extension_names_its_own_format() {
        for ext in super::super::supported_extensions() {
            let format = format_of(ext);
            let expected = match *ext {
                "ora" => SourceFormat::OpenRaster,
                "kra" => SourceFormat::Krita,
                "psd" => SourceFormat::Photoshop,
                "clip" => SourceFormat::ClipStudio,
                "png" => SourceFormat::Png,
                other => panic!("no format for .{other}"),
            };
            assert_eq!(format, expected, "for .{ext}");
        }
    }

    /// A name Umber does not read is refused by name, exactly as `import` does.
    #[test]
    fn a_file_umber_cannot_read_has_no_thumbnail() {
        let err = from_path(std::path::Path::new("drawing.mdp")).unwrap_err();
        assert!(matches!(err, ImportError::UnsupportedExtension(ref e) if e == "mdp"));
    }
}
