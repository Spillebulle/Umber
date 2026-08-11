//! Shared plumbing for the two ZIP-based formats, and for placing a layer's
//! rectangle onto the canvas.

use std::io::{Cursor, Read};

use glam::UVec2;
use zip::ZipArchive;

use super::{ImportError, ImportedDocument, SourceFormat};
use crate::geom::PixelRect;

pub type Zip<'a> = ZipArchive<Cursor<&'a [u8]>>;

/// Open a ZIP-container document.
pub fn open(bytes: &[u8], format: SourceFormat) -> Result<Zip<'_>, ImportError> {
    ZipArchive::new(Cursor::new(bytes)).map_err(|e| ImportError::Malformed {
        format,
        detail: format!("the file is not a readable archive ({e})"),
    })
}

/// Largest `stack.xml` or `maindoc.xml` either ZIP reader will decompress.
///
/// **A document's structure entry is the third of the shape
/// [`read_optional_entry_bounded`] exists for**, and it arrived by the same
/// reasoning as the first two: its size follows a *count* the format does not
/// bound, so [`ImportedDocument::MAX_TOTAL_BYTES`]' sixteen gigabytes is the
/// wrong instrument. XML deflates around 1000:1, so at that figure a sixteen
/// megabyte archive entry is a `read_to_end` growing a `Vec` to 16 GiB — an
/// abort, from a small file, before quick-xml has seen a byte.
///
/// **The figure has to be generous, and that is not laziness.** Umber refuses a
/// stack past [`LayerStack::MAX`] and it must be *that* refusal an over-tall
/// document meets, not this one: a bound tight enough to catch a thousand-layer
/// file would tell an artist their file was unreadable when it merely has too
/// many layers, which is the wrong-bound failure `CanvasTooLarge`'s own docs
/// record. Nor can the honest size be derived, because a layer's *name* is
/// unbounded — it comes out of whatever wrote the file — so sixty-four legal
/// layers can carry an arbitrarily long `stack.xml`.
///
/// So it is stated as headroom rather than as a derivation, and the two ends are
/// what make it defensible. A full sixty-four-layer document Umber writes is
/// about fifteen kilobytes of `stack.xml`, so this is a thousand times what the
/// writer produces — `a_full_stacks_own_structure_is_far_inside_the_bound` is
/// the measurement rather than the claim. And a stack tall enough to be refused
/// for its layer count is roughly two hundred bytes an element, so this admits
/// tens of thousands of them: `TooManyLayers` is what such a file meets.
///
/// **What it does not bound is what the parse then builds.** A `stack.xml` of
/// nested `<stack>` elements pushes a `LayerSpec` and possibly a warning per
/// element before the count is checked, which is perhaps twenty times the text.
/// Sixteen mebibytes of that is a few hundred megabytes rather than sixteen
/// gigabytes; it is bounded and survivable where it was neither, and it is not
/// zero. Said out loud rather than left for the next reader to find.
///
/// [`LayerStack::MAX`]: crate::layer::LayerStack::MAX
pub const MAX_STRUCTURE_BYTES: u64 = 16 << 20;

/// Read one entry whole, at a bound the caller states.
///
/// Refuses anything whose declared size is beyond what its own content can be.
/// Both ORA and KRA are ZIPs supplied by strangers, and a 20 KB file that
/// claims to expand to 40 GB is a well-known way to knock over a program that
/// reads to the end without looking.
///
/// **The limit is a parameter and there is no unbounded form**, which is the
/// half that was missing: this used to read every required entry at
/// [`ImportedDocument::MAX_TOTAL_BYTES`], and both of its call sites are a
/// document's structure XML rather than a canvas. See [`MAX_STRUCTURE_BYTES`],
/// and [`read_optional_entry_bounded`] for the rule.
pub fn read_entry_bounded(
    zip: &mut Zip<'_>,
    name: &str,
    format: SourceFormat,
    limit: u64,
) -> Result<Vec<u8>, ImportError> {
    read_optional_entry_bounded(zip, name, format, limit)?.ok_or_else(|| ImportError::Malformed {
        format,
        detail: format!("the archive has no `{name}`"),
    })
}

/// Read one entry whole if it is present.
///
/// Bounded at [`ImportedDocument::MAX_TOTAL_BYTES`], which is a sanity bound
/// for a *canvas*. Anything whose own size is bounded by something much smaller
/// should say so — see [`read_optional_entry_bounded`].
pub fn read_optional_entry(
    zip: &mut Zip<'_>,
    name: &str,
    format: SourceFormat,
) -> Result<Option<Vec<u8>>, ImportError> {
    read_optional_entry_bounded(zip, name, format, ImportedDocument::MAX_TOTAL_BYTES)
}

/// The same, for an entry whose content has a bound of its own.
///
/// **A limit measured in gigabytes is the wrong one for a parameter record**,
/// and effects are the first entry in the archive whose *cardinality* is
/// unbounded by the format. `umber/effects/<n>.ron` at
/// [`ImportedDocument::MAX_TOTAL_BYTES`] is a decompression bomb with a very
/// good ratio: RON is about fifteen bytes per effect and deflates around 500:1,
/// so a 569 KB archive entry expands to 300 MB and twenty million effects, and
/// the `Vec` is fully materialised before any budget or duplicate check sees
/// it. Measured: 8 seconds and several gigabytes resident for that one, ~55
/// seconds at the 2 GiB ceiling, and sixty-four layers may each name the same
/// entry. A four-megabyte file that hangs the application is a worse outcome
/// than every malformed case this module handles well.
///
/// **A text record is the second entry of that shape**, and it arrived by the
/// same reasoning independently: `umber/text/<n>.json` is as long as somebody
/// typed, which the canvas does not bound either. Two callers with two figures
/// is what the parameter is for — [`crate::textobj::MAX_RECORD_BYTES`] is the
/// other one — and the fact that the second case reached this signature without
/// changing it is the check on the first.
///
/// **There are four now**, and the two that arrived last are the ones this
/// module read at the canvas bound for as long as it existed: a document's
/// structure XML ([`MAX_STRUCTURE_BYTES`]) and the undo history's manifest
/// (`docimport::history::MAX_MANIFEST_BYTES`). Both are counts rather than
/// canvases and neither had a figure of its own, which is the failure the first
/// two paragraphs describe arriving in the entries somebody would think of last.
///
/// So the caller states what its own content can be, and the check happens
/// against the *declared* size before a byte is decompressed as well as against
/// what actually arrives — the header is only a claim.
pub fn read_optional_entry_bounded(
    zip: &mut Zip<'_>,
    name: &str,
    format: SourceFormat,
    limit: u64,
) -> Result<Option<Vec<u8>>, ImportError> {
    let mut entry = match zip.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => {
            return Err(ImportError::Malformed {
                format,
                detail: format!("`{name}` could not be read ({e})"),
            });
        }
    };
    if entry.size() > limit {
        return Err(ImportError::Malformed {
            format,
            detail: format!("`{name}` claims to be {} bytes", entry.size()),
        });
    }

    let mut out = Vec::with_capacity(entry.size().min(limit).min(1 << 20) as usize);
    // `take` as well as the declared-size check: the header is only a claim,
    // and the actual stream can be longer than it says.
    entry
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut out)
        .map_err(|e| ImportError::Malformed {
            format,
            detail: format!("`{name}` could not be decompressed ({e})"),
        })?;
    if out.len() as u64 > limit {
        return Err(ImportError::Malformed {
            format,
            detail: format!("`{name}` is larger than Umber will read"),
        });
    }
    Ok(Some(out))
}

/// Both formats put an uncompressed `mimetype` entry first, the ODF convention.
///
/// Checked because the extension is only a hint: a `.kra` that is really an ORA
/// should say so clearly rather than fail three functions later on a missing
/// `maindoc.xml`.
pub fn check_mimetype(
    zip: &mut Zip<'_>,
    expected: &str,
    format: SourceFormat,
) -> Result<(), ImportError> {
    let Some(found) = read_optional_entry(zip, "mimetype", format)? else {
        // Some writers omit it. Not worth refusing a file over.
        return Ok(());
    };
    let found = String::from_utf8_lossy(&found);
    let found = found.trim();
    if found != expected {
        return Err(ImportError::Malformed {
            format,
            detail: format!("its mimetype is `{found}`, not `{expected}`"),
        });
    }
    Ok(())
}

/// The attributes of one XML element, decoded once.
///
/// Both ZIP formats describe their layers in XML, and quick-xml hands out
/// attributes as raw byte slices still carrying entity escapes. Decoding them
/// at each use site is where a `&amp;` in a layer name turns into rubbish, so
/// it happens once, here.
pub struct Attrs(Vec<(String, String)>);

impl Attrs {
    pub fn read(e: &quick_xml::events::BytesStart<'_>) -> Result<Self, String> {
        let mut out = Vec::new();
        for attr in e.attributes() {
            let attr = attr.map_err(|err| format!("a malformed attribute ({err})"))?;
            let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
            let value = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|err| format!("an unreadable attribute value ({err})"))?
                .into_owned();
            out.push((key, value));
        }
        Ok(Self(out))
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    pub fn string(&self, key: &str) -> Option<String> {
        self.get(key).map(str::to_string)
    }

    pub fn parse<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.get(key)?.trim().parse().ok()
    }
}

/// The part of a layer's own rectangle that lands on the canvas, as its own
/// tightly packed rectangle.
///
/// Source layers are stored at their bounding box with an offset, which may be
/// negative and may run past the canvas — Photoshop and Krita both keep pixels
/// outside the visible area, and a document that has been cropped is full of
/// them. Everything outside the canvas is dropped here rather than trusted to
/// arithmetic later.
///
/// `None` where nothing lands, which is a layer dragged entirely off the page.
/// That is not an error: the old dense reader produced a canvas of zeroes for
/// it, and no piece at all is the same picture — see [`PixelPiece`]'s rule 3.
///
/// **The clipping is [`blit`]'s, arithmetic for arithmetic.** They are the same
/// two intersections and a divergence between them is a layer landing a pixel
/// out; `cropping_agrees_with_the_dense_blit_it_replaced` drives both over the
/// same offsets and compares the assembled result.
///
/// [`PixelPiece`]: super::PixelPiece
pub fn crop(
    src: &[u8],
    src_size: UVec2,
    at: (i64, i64),
    canvas: UVec2,
) -> Option<(PixelRect, Vec<u8>)> {
    debug_assert_eq!(src.len(), src_size.x as usize * src_size.y as usize * 4);

    // **Saturating, because the offset is a number out of somebody else's
    // file.** An ORA's `x`/`y` are parsed as `i64` and `-i64::MIN` panics in a
    // debug build and wraps in a release one — into a rectangle that then
    // indexes past the source. Every clamp below refuses anything that does not
    // land, so saturating to the ends is exactly right: an offset that far out
    // reaches no canvas. `blit` has the same expressions and is left alone
    // because both its remaining call sites pass `(0, 0)`; if either ever takes
    // an offset off a file, it wants this too.
    let (ox, oy) = at;
    let y_from = oy.saturating_neg().clamp(0, src_size.y as i64);
    let y_to = (canvas.y as i64)
        .saturating_sub(oy)
        .clamp(0, src_size.y as i64);
    let x_from = ox.saturating_neg().clamp(0, src_size.x as i64);
    let x_to = (canvas.x as i64)
        .saturating_sub(ox)
        .clamp(0, src_size.x as i64);
    if y_to <= y_from || x_to <= x_from {
        return None;
    }

    let width = (x_to - x_from) as usize;
    let height = (y_to - y_from) as usize;
    let mut bytes = Vec::with_capacity(width * height * 4);
    for sy in y_from..y_to {
        let start = ((sy * src_size.x as i64 + x_from) * 4) as usize;
        bytes.extend_from_slice(&src[start..start + width * 4]);
    }
    Some((
        PixelRect {
            x: (x_from + ox) as u32,
            y: (y_from + oy) as u32,
            width: width as u32,
            height: height as u32,
        },
        bytes,
    ))
}

/// Copy a layer's own rectangle into a canvas-sized buffer.
///
/// The dense form of [`crop`], kept for the flattened fallbacks — a
/// `mergedimage.png` is one picture at the origin and there is nothing sparse
/// about it — and for the tests that hold the two in step.
///
/// Both buffers are plain RGBA8 in the same encoding; this is a copy, not a
/// composite.
pub fn blit(dst: &mut [u8], canvas: UVec2, src: &[u8], src_size: UVec2, at: (i64, i64)) {
    debug_assert_eq!(dst.len(), canvas.x as usize * canvas.y as usize * 4);
    debug_assert_eq!(src.len(), src_size.x as usize * src_size.y as usize * 4);

    let (ox, oy) = at;
    for sy in 0..src_size.y as i64 {
        let dy = sy + oy;
        if dy < 0 || dy >= canvas.y as i64 {
            continue;
        }
        // Clip the row once instead of testing every pixel.
        let x_from = (-ox).max(0);
        let x_to = (canvas.x as i64 - ox).min(src_size.x as i64);
        if x_to <= x_from {
            continue;
        }
        let src_start = ((sy * src_size.x as i64 + x_from) * 4) as usize;
        let src_end = ((sy * src_size.x as i64 + x_to) * 4) as usize;
        let dst_start = ((dy * canvas.x as i64 + x_from + ox) * 4) as usize;
        let len = src_end - src_start;
        dst[dst_start..dst_start + len].copy_from_slice(&src[src_start..src_end]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(size: UVec2) -> Vec<u8> {
        vec![0; size.x as usize * size.y as usize * 4]
    }

    /// **The one guard that says the sparse path did not move a pixel.**
    ///
    /// `crop` replaced `blit` on the ORA layer path, and the two are the same
    /// two intersections written twice — which is exactly the shape this
    /// codebase distrusts. So they are driven against each other over every
    /// offset that can arise from a real file: inside, hanging off each edge,
    /// hanging off two at once, and missing the canvas entirely. The source
    /// carries a different byte in every pixel, so a copy that is out by a row
    /// or a column cannot pass by accident.
    ///
    /// Demonstrated by mutation: change either clamp in `crop` by one and this
    /// fails; drop the `x_from` term from the destination and it fails.
    #[test]
    fn cropping_agrees_with_the_dense_blit_it_replaced() {
        let size = UVec2::new(7, 5);
        let src_size = UVec2::new(4, 3);
        // Every byte distinct, so a shift shows up rather than cancelling.
        let src: Vec<u8> = (0..(src_size.x * src_size.y * 4) as u8).collect();

        for oy in -4i64..=6 {
            for ox in -5i64..=8 {
                let mut dense = canvas(size);
                blit(&mut dense, size, &src, src_size, (ox, oy));

                let mut sparse = canvas(size);
                if let Some((rect, bytes)) = crop(&src, src_size, (ox, oy), size) {
                    assert!(
                        rect.x + rect.width <= size.x && rect.y + rect.height <= size.y,
                        "a crop must land inside the canvas: {rect:?} at ({ox}, {oy})"
                    );
                    assert_eq!(bytes.len() as u64, rect.area() * 4);
                    for row in 0..rect.height as usize {
                        let from = row * rect.width as usize * 4;
                        let to =
                            (rect.y as usize + row) * size.x as usize * 4 + rect.x as usize * 4;
                        let len = rect.width as usize * 4;
                        sparse[to..to + len].copy_from_slice(&bytes[from..from + len]);
                    }
                }
                assert_eq!(sparse, dense, "offset ({ox}, {oy})");
            }
        }
    }

    /// A layer entirely off the page yields no piece, which is the same picture
    /// the canvas of zeroes was — and is the case that makes "no piece means
    /// the empty value" load-bearing rather than decorative.
    ///
    /// **The two extremes are in the sweep, and they are not decoration.** An
    /// ORA's `x`/`y` are parsed as `i64` off a file a stranger wrote, and
    /// `-i64::MIN` panics in a debug build; the whole arithmetic here is
    /// saturating because of it. Nothing else in this module drives that value.
    #[test]
    fn a_layer_that_misses_the_canvas_yields_nothing_at_all() {
        let size = UVec2::new(4, 4);
        let src = vec![255u8; 2 * 2 * 4];
        for at in [
            (-2, 0),
            (4, 0),
            (0, -2),
            (0, 4),
            (-9, -9),
            (i64::MIN, 0),
            (0, i64::MIN),
            (i64::MAX, i64::MAX),
            (i64::MIN, i64::MAX),
        ] {
            assert!(
                crop(&src, UVec2::new(2, 2), at, size).is_none(),
                "a 2×2 layer at {at:?} does not reach a 4×4 canvas"
            );
        }
    }

    #[test]
    fn a_layer_lands_at_its_offset() {
        let size = UVec2::new(4, 4);
        let mut dst = canvas(size);
        let src = vec![255u8; 2 * 2 * 4];
        blit(&mut dst, size, &src, UVec2::new(2, 2), (1, 1));

        let px = |x: usize, y: usize| dst[(y * 4 + x) * 4];
        assert_eq!(px(0, 0), 0);
        assert_eq!(px(1, 1), 255);
        assert_eq!(px(2, 2), 255);
        assert_eq!(px(3, 3), 0);
    }

    #[test]
    fn negative_offsets_clip_instead_of_wrapping() {
        // A cropped document has layers hanging off the top left. Getting the
        // clip wrong wraps them onto the opposite edge, which looks like
        // corruption rather than like a bug.
        let size = UVec2::new(3, 3);
        let mut dst = canvas(size);
        let src = vec![255u8; 2 * 2 * 4];
        blit(&mut dst, size, &src, UVec2::new(2, 2), (-1, -1));

        assert_eq!(dst[0], 255, "the one visible pixel should be at 0,0");
        for i in 1..9 {
            assert_eq!(dst[i * 4], 0, "pixel {i} should be untouched");
        }
    }

    #[test]
    fn a_layer_entirely_outside_the_canvas_writes_nothing() {
        let size = UVec2::new(2, 2);
        let mut dst = canvas(size);
        let src = vec![255u8; 4];
        blit(&mut dst, size, &src, UVec2::new(1, 1), (5, 5));
        blit(&mut dst, size, &src, UVec2::new(1, 1), (-5, -5));
        assert!(dst.iter().all(|&b| b == 0));
    }

    #[test]
    fn a_layer_larger_than_the_canvas_is_cropped() {
        let size = UVec2::new(2, 2);
        let mut dst = canvas(size);
        let src = vec![255u8; 8 * 8 * 4];
        blit(&mut dst, size, &src, UVec2::new(8, 8), (-1, -1));
        assert!(dst.iter().all(|&b| b == 255));
    }
}
