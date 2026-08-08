//! Shared plumbing for the two ZIP-based formats, and for placing a layer's
//! rectangle onto the canvas.

use std::io::{Cursor, Read};

use glam::UVec2;
use zip::ZipArchive;

use super::{ImportError, ImportedDocument, SourceFormat};

pub type Zip<'a> = ZipArchive<Cursor<&'a [u8]>>;

/// Open a ZIP-container document.
pub fn open(bytes: &[u8], format: SourceFormat) -> Result<Zip<'_>, ImportError> {
    ZipArchive::new(Cursor::new(bytes)).map_err(|e| ImportError::Malformed {
        format,
        detail: format!("the file is not a readable archive ({e})"),
    })
}

/// Read one entry whole.
///
/// Refuses anything whose declared size is beyond what an import could use.
/// Both ORA and KRA are ZIPs supplied by strangers, and a 20 KB file that
/// claims to expand to 40 GB is a well-known way to knock over a program that
/// reads to the end without looking.
pub fn read_entry(
    zip: &mut Zip<'_>,
    name: &str,
    format: SourceFormat,
) -> Result<Vec<u8>, ImportError> {
    read_optional_entry(zip, name, format)?.ok_or_else(|| ImportError::Malformed {
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

/// Copy a layer's own rectangle into a canvas-sized buffer.
///
/// Source layers are stored at their bounding box with an offset, which may be
/// negative and may run past the canvas — Photoshop and Krita both keep pixels
/// outside the visible area, and a document that has been cropped is full of
/// them. Everything outside the canvas is dropped here rather than trusted to
/// arithmetic later.
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
