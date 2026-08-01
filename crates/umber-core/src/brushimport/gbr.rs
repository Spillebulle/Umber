//! GIMP `.gbr` brush tips.
//!
//! The oldest and simplest bitmap-brush format, and the one the CC0 stamp packs
//! are distributed in. Everything is **big-endian**, which is the mistake to
//! watch for: a little-endian read of a 64×64 brush reports a width of about a
//! billion and fails on the length check rather than producing garbage, so the
//! error at least points at the right place.
//!
//! ```text
//! offset  size  field
//! 0       4     header_size, including the name that follows
//! 4       4     version (1, 2 or 3)
//! 8       4     width
//! 12      4     height
//! 16      4     bytes per pixel: 1 = coverage mask, 4 = RGBA
//! 20      4     magic "GIMP"   (version >= 2 only)
//! 24      4     spacing, per cent of the brush size (version >= 2 only)
//! 28      ...   NUL-terminated name, filling out header_size
//! ```
//!
//! Then `width * height * bytes` of pixel data.
//!
//! A `.gpb` — GIMP's obsolete "pixmap brush" — is the same file with a whole
//! **pattern** appended: a `GPAT` header and `width × height × 3` RGB bytes
//! sharing the mask's dimensions. GIMP still reads one, and so does this, which
//! is why [`read_one`] has to know where a `.gbr` ends rather than assuming it
//! runs to the end of the buffer.
//!
//! # What is dropped
//!
//! - **Colour.** A 4-byte `.gbr` is a coloured stamp, and a `.gpb` carries its
//!   colour in a trailing pattern; Umber's scratch texture is a single coverage
//!   channel by design, so only the coverage survives. A pixmap brush therefore
//!   imports as its silhouette.
//! - **Spacing.** Carried out separately as [`GbrBrush::spacing`] rather than
//!   folded into the mask, because it belongs on the `Brush`, not the tip.
//!
//! `.gih` animated brushes are a sequence of these with a parameter header of
//! their own; see [`super::gih`].

use crate::brush::Brush;
use crate::preset::PresetError;
use crate::tip::TipMask;

/// `GIMP`, big-endian, at offset 20 of a version 2 or later file.
const MAGIC: u32 = u32::from_be_bytes(*b"GIMP");

/// `GPAT`, the magic of the pattern a `.gpb` appends to its mask.
const PATTERN_MAGIC: u32 = u32::from_be_bytes(*b"GPAT");

/// A GIMP pattern header is six big-endian words before its name.
const PATTERN_HEADER: usize = 24;

/// The largest header this will read. A `.gbr` header is a fixed 28 bytes plus
/// a name; anything claiming megabytes of name is corrupt or hostile.
const MAX_HEADER: u32 = 4096;

/// A tip and the one brush parameter the format carries alongside it.
#[derive(Clone, Debug)]
pub struct GbrBrush {
    pub name: String,
    pub tip: TipMask,
    /// Distance between stamps as a fraction of the brush size — already
    /// converted from GIMP's percentage, so it drops straight into
    /// [`crate::Brush::spacing`]. `None` for version 1 files, which do not
    /// record it.
    pub spacing: Option<f32>,
    /// The file described a colour as well as a coverage — a 4-byte RGBA stamp
    /// or a `.gpb`'s trailing pattern — and only the coverage came across.
    pub coloured: bool,
}

/// Decode a GIMP `.gbr` file.
pub fn from_gbr(bytes: &[u8]) -> Result<GbrBrush, PresetError> {
    read_one(bytes).map(|(brush, _)| brush)
}

/// Decode the `.gbr` at the start of `bytes`, and say how far it reached.
///
/// The length matters to exactly two callers and for two different reasons:
/// [`super::gih`] concatenates whole `.gbr` files and has to find the next one,
/// and a `.gpb`'s trailing pattern is only recognisable once the mask's end is
/// known. Everything else wants [`from_gbr`].
pub(crate) fn read_one(bytes: &[u8]) -> Result<(GbrBrush, usize), PresetError> {
    let header_size = be_u32(bytes, 0)?;
    let version = be_u32(bytes, 4)?;
    let width = be_u32(bytes, 8)?;
    let height = be_u32(bytes, 12)?;
    let depth = be_u32(bytes, 16)?;

    if !(1..=3).contains(&version) {
        return Err(PresetError::UnsupportedVersion(None, version));
    }

    // Version 1 has no magic and no spacing, so its header is 8 bytes shorter.
    let fixed_header: u32 = if version == 1 { 20 } else { 28 };
    if header_size < fixed_header || header_size > MAX_HEADER {
        return Err(malformed(format!(
            "header size {header_size} is not plausible for a .gbr"
        )));
    }
    if version >= 2 {
        let magic = be_u32(bytes, 20)?;
        if magic != MAGIC {
            return Err(malformed(
                "missing the GIMP magic; this is not a .gbr".to_string(),
            ));
        }
    }

    let spacing = if version >= 2 {
        // GIMP states spacing as a percentage of the brush size, and Umber as a
        // fraction of the diameter — the same quantity, off by a hundred.
        //
        // Zero means *unset*, not "stamp every hundredth of a diameter": GIMP's
        // own control has a minimum of 1, so a zero is a writer that never
        // filled the field in. Taking it literally turns a 500 px stamp into
        // five-pixel steps, which is a hundred dabs where the file meant ten.
        match be_u32(bytes, 24)? {
            0 => None,
            percent => Some((percent as f32 / 100.0).clamp(0.01, 4.0)),
        }
    } else {
        None
    };

    // The name fills whatever is left of the header, NUL-terminated. A brush
    // with no name at all is legal and common.
    let name = bytes
        .get(fixed_header as usize..header_size as usize)
        .map(|raw| {
            let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
            String::from_utf8_lossy(&raw[..end]).trim().to_string()
        })
        .unwrap_or_default();

    let pixels = bytes
        .get(header_size as usize..)
        .ok_or_else(|| malformed("the file ends before its pixel data".to_string()))?;

    let texels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| malformed(format!("{width}x{height} overflows")))?;

    let (coverage, mut consumed, mut coloured) = match depth {
        // A coverage mask, already in Umber's convention: 255 is full paint.
        1 => {
            if pixels.len() < texels {
                return Err(short_data(texels, pixels.len()));
            }
            (pixels[..texels].to_vec(), texels, false)
        }
        // A coloured stamp. Only the alpha survives — see the module docs.
        4 => {
            let needed = texels
                .checked_mul(4)
                .ok_or_else(|| malformed(format!("{width}x{height} RGBA overflows")))?;
            if pixels.len() < needed {
                return Err(short_data(needed, pixels.len()));
            }
            (
                pixels[..needed].chunks_exact(4).map(|px| px[3]).collect(),
                needed,
                true,
            )
        }
        other => {
            return Err(malformed(format!(
                "{other} bytes per pixel; only 1 (mask) and 4 (RGBA) exist in the wild"
            )));
        }
    };

    // A `.gpb` is a 1-byte `.gbr` with a pattern stapled to the end of it. The
    // colour is dropped like a 4-byte stamp's, but the *length* has to be right
    // or a `.gih` of `.gpb` frames would read the pattern as the next frame.
    if depth == 1
        && let Some(pattern) = pattern_length(&pixels[consumed..], width, height)
    {
        consumed += pattern;
        coloured = true;
    }

    Ok((
        GbrBrush {
            name,
            tip: TipMask::new(width, height, coverage)?,
            spacing,
            coloured,
        },
        header_size as usize + consumed,
    ))
}

/// Length of the GIMP pattern at the start of `bytes`, if there is one of
/// exactly `width × height`.
///
/// Deliberately strict about the dimensions: a `.gpb`'s pattern always matches
/// its mask, so anything else is the next thing in the file rather than a
/// pattern, and swallowing it would lose a frame of a pipe.
fn pattern_length(bytes: &[u8], width: u32, height: u32) -> Option<usize> {
    if be_u32(bytes, 20).ok()? != PATTERN_MAGIC
        || be_u32(bytes, 4).ok()? != 1
        || be_u32(bytes, 8).ok()? != width
        || be_u32(bytes, 12).ok()? != height
        || be_u32(bytes, 16).ok()? != 3
    {
        return None;
    }
    let header = be_u32(bytes, 0).ok()? as usize;
    if header < PATTERN_HEADER || header > MAX_HEADER as usize {
        return None;
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(3)?;
    let total = header.checked_add(pixels)?;
    (bytes.len() >= total).then_some(total)
}

/// Turn a decoded `.gbr` into the brush Umber will paint with, and the mask it
/// stamps.
///
/// The format carries a picture, a spacing and nothing else, so most of a
/// [`Brush`] comes from the defaults. The three that do not:
///
/// - **Size** is the mask's longer side in pixels, so a stamp lands at its
///   original scale until the user says otherwise. `Brush::size` describes the
///   long axis, which is exactly what the padded mask's side is.
/// - **Spacing** is the file's own where it has one. GIMP's default of 10 %
///   happens to be Umber's default too, so a version 1 file — which records no
///   spacing at all — is not being guessed at so much as agreed with.
/// - **Pressure is off.** A `.gbr` has no dynamics; GIMP stamps one at a
///   constant size unless a separate dynamics preset says otherwise. Leaving
///   Umber's pressure-to-size mapping on would shrink the stamp to 8 % of
///   itself at the start of every line, which is not what the file describes.
///
/// The mask is padded to a square: the dab stretches a tip over its bounding
/// box, so an unpadded portrait stamp would come out squashed. See
/// [`TipMask::padded_to_square`].
pub fn to_brush(brush: GbrBrush) -> (Brush, TipMask) {
    let tip = brush.tip.padded_to_square();
    let default = Brush::default();
    let parameters = Brush {
        size: (tip.width() as f32).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE),
        spacing: brush.spacing.unwrap_or(default.spacing),
        pressure_size: false,
        pressure_opacity: false,
        ..default
    };
    (parameters, tip)
}

/// What reading this `.gbr` will throw away.
///
/// Deliberately best-effort: a file this cannot parse returns nothing here and
/// fails properly in [`from_gbr`], so nothing is reported on twice. See
/// [`crate::brushimport::dropped_features`].
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    // A coloured stamp arrives as its silhouette — the stroke scratch is a
    // single coverage channel by design. Worth saying out loud: the brush works,
    // and it does not look like the picture in the file.
    //
    // The whole file rather than its header, because a `.gpb` states its depth
    // as 1 and hides the colour in a pattern at the far end. Decoding twice
    // costs a copy of a mask on an explicit import and nothing at all on the
    // drawing path.
    match from_gbr(bytes) {
        Ok(brush) if brush.coloured => vec![COLOURED],
        _ => Vec::new(),
    }
}

/// The one loss this format has, named once so `.gih` and `.bundle` report it
/// with the same words.
pub(crate) const COLOURED: &str = "coloured stamps";

fn be_u32(bytes: &[u8], offset: usize) -> Result<u32, PresetError> {
    bytes
        .get(offset..offset + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| malformed(format!("the file ends at byte {}", bytes.len())))
}

fn malformed(message: String) -> PresetError {
    PresetError::Malformed(None, message)
}

fn short_data(needed: usize, got: usize) -> PresetError {
    malformed(format!(
        "pixel data is {got} bytes, {needed} expected — is the file truncated, \
         or was it read little-endian?"
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Build a `.gbr` by hand. No CC0 `.gbr` pack has a licence statement that
    /// can be verified from the download itself yet (see
    /// `docs/brush-sources.md`), so there is no real file to check into the
    /// tests — which makes the round trip below the only thing pinning the
    /// byte layout down.
    pub(crate) fn gbr(
        version: u32,
        width: u32,
        height: u32,
        depth: u32,
        name: &str,
        data: &[u8],
    ) -> Vec<u8> {
        let fixed: u32 = if version == 1 { 20 } else { 28 };
        let name_bytes = {
            let mut v = name.as_bytes().to_vec();
            v.push(0);
            v
        };
        let header_size = fixed + name_bytes.len() as u32;

        let mut out = Vec::new();
        for field in [header_size, version, width, height, depth] {
            out.extend_from_slice(&field.to_be_bytes());
        }
        if version >= 2 {
            out.extend_from_slice(&MAGIC.to_be_bytes());
            out.extend_from_slice(&25u32.to_be_bytes()); // 25% spacing
        }
        out.extend_from_slice(&name_bytes);
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn a_greyscale_brush_decodes_to_its_mask() {
        let data = [0u8, 64, 128, 255];
        let file = gbr(2, 2, 2, 1, "Test", &data);
        let brush = from_gbr(&file).expect("decode");

        assert_eq!(brush.name, "Test");
        assert_eq!(brush.tip.width(), 2);
        assert_eq!(brush.tip.height(), 2);
        assert_eq!(brush.tip.coverage(), data);
        // 25% of the brush size, as a fraction.
        assert_eq!(brush.spacing, Some(0.25));
    }

    #[test]
    fn a_coloured_brush_keeps_only_its_alpha() {
        // Two RGBA texels: opaque red, then half-transparent blue.
        let data = [255, 0, 0, 255, 0, 0, 255, 128];
        let brush = from_gbr(&gbr(2, 2, 1, 4, "Colour", &data)).expect("decode");
        assert_eq!(brush.tip.coverage(), [255, 128]);
    }

    #[test]
    fn a_spacing_of_zero_means_unset_rather_than_one_hundredth() {
        // GIMP's own control cannot go below 1, so a zero is a writer that
        // never filled the field in. Taken literally it turns a 500 px stamp
        // into five-pixel steps — a hundred dabs where the file meant ten.
        let mut file = gbr(2, 2, 2, 1, "T", &[1, 2, 3, 4]);
        file[24..28].copy_from_slice(&0u32.to_be_bytes());
        let brush = from_gbr(&file).expect("decode");
        assert_eq!(brush.spacing, None);
        assert_eq!(to_brush(brush).0.spacing, Brush::default().spacing);
    }

    #[test]
    fn a_stamp_becomes_a_square_tip_at_its_own_size() {
        // Padded rather than stretched: the dab spreads a tip over its bounding
        // box, so a 4x2 stamp rendered unpadded would come out twice as tall as
        // the picture in the file.
        let brush = from_gbr(&gbr(2, 4, 2, 1, "Wide", &[255; 8])).expect("decode");
        let (parameters, tip) = to_brush(brush);
        assert_eq!((tip.width(), tip.height()), (4, 4));
        assert_eq!(parameters.size, 4.0);
        // 25% spacing, from the header the fixture writes.
        assert_eq!(parameters.spacing, 0.25);
        // A `.gbr` carries no dynamics, and GIMP stamps one at a constant size.
        assert!(!parameters.pressure_size);
        assert!(!parameters.pressure_opacity);
    }

    #[test]
    fn a_coloured_stamp_says_that_its_colour_was_dropped() {
        let coloured = gbr(2, 1, 1, 4, "T", &[1, 2, 3, 4]);
        assert_eq!(dropped_features(&coloured), ["coloured stamps"]);
        assert!(dropped_features(&gbr(2, 1, 1, 1, "T", &[9])).is_empty());
        // Best effort: a file this cannot parse says nothing, and fails
        // properly in `from_gbr` instead.
        assert!(dropped_features(b"short").is_empty());
    }

    /// A GIMP pattern, as a `.gpb` staples one to the end of its mask.
    pub(crate) fn pattern(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for field in [PATTERN_HEADER as u32 + 1, 1, width, height, 3] {
            out.extend_from_slice(&field.to_be_bytes());
        }
        out.extend_from_slice(&PATTERN_MAGIC.to_be_bytes());
        out.push(0); // the name, empty
        out.extend(std::iter::repeat_n(200u8, (width * height * 3) as usize));
        out
    }

    /// A `.gpb` is a mask with a whole colour pattern behind it. Umber keeps
    /// the mask, and — the part that is easy to get wrong — has to know where
    /// the pattern ends, or a `.gih` made of these reads one as the next frame.
    #[test]
    fn a_gpb_pixmap_brush_keeps_its_mask_and_says_it_dropped_the_colour() {
        let mut file = gbr(2, 2, 2, 1, "Pixmap", &[10, 20, 30, 40]);
        let mask_only = file.len();
        file.extend_from_slice(&pattern(2, 2));

        let (brush, consumed) = read_one(&file).expect("decode");
        assert_eq!(brush.tip.coverage(), [10, 20, 30, 40]);
        assert!(brush.coloured);
        assert_eq!(consumed, file.len(), "the pattern was not accounted for");
        assert_eq!(dropped_features(&file), ["coloured stamps"]);

        // And a plain `.gbr` must not grow a pattern it does not have.
        let (plain, consumed) = read_one(&file[..mask_only]).expect("decode");
        assert!(!plain.coloured);
        assert_eq!(consumed, mask_only);
    }

    /// A pattern whose dimensions do not match the mask is the *next thing in
    /// the file*, not this brush's colour. Swallowing it loses a frame.
    #[test]
    fn a_mismatched_pattern_is_left_where_it_is() {
        let mut file = gbr(2, 2, 2, 1, "T", &[1, 2, 3, 4]);
        let mask_only = file.len();
        file.extend_from_slice(&pattern(4, 4));
        let (brush, consumed) = read_one(&file).expect("decode");
        assert!(!brush.coloured);
        assert_eq!(consumed, mask_only);
    }

    #[test]
    fn a_decoded_brush_reports_where_it_ended() {
        // What `.gih` walks the file with. Off by one byte and every frame
        // after the first is rubbish.
        let file = gbr(2, 3, 2, 1, "T", &[7; 6]);
        assert_eq!(read_one(&file).expect("decode").1, file.len());
        let coloured = gbr(2, 3, 2, 4, "T", &[7; 24]);
        assert_eq!(read_one(&coloured).expect("decode").1, coloured.len());
    }

    #[test]
    fn a_version_1_brush_has_no_spacing_and_still_decodes() {
        let brush = from_gbr(&gbr(1, 2, 1, 1, "Old", &[10, 20])).expect("decode");
        assert_eq!(brush.spacing, None);
        assert_eq!(brush.tip.coverage(), [10, 20]);
        assert_eq!(brush.name, "Old");
    }

    #[test]
    fn an_unnamed_brush_is_fine() {
        let brush = from_gbr(&gbr(2, 1, 1, 1, "", &[7])).expect("decode");
        assert_eq!(brush.name, "");
    }

    #[test]
    fn a_little_endian_read_would_have_been_caught() {
        // The whole point of the guard: a file read the wrong way round must
        // fail loudly rather than produce a billion-pixel brush.
        let mut file = gbr(2, 2, 2, 1, "T", &[1, 2, 3, 4]);
        file[8..12].copy_from_slice(&2u32.to_le_bytes());
        assert!(from_gbr(&file).is_err());
    }

    #[test]
    fn a_file_without_the_magic_is_refused() {
        let mut file = gbr(2, 2, 2, 1, "T", &[1, 2, 3, 4]);
        file[20..24].copy_from_slice(b"PNG\0");
        assert!(from_gbr(&file).is_err());
    }

    #[test]
    fn truncation_is_an_error_rather_than_a_panic() {
        let file = gbr(2, 8, 8, 1, "T", &[255; 10]);
        assert!(from_gbr(&file).is_err());

        for cut in 0..40 {
            let full = gbr(2, 2, 2, 1, "T", &[1, 2, 3, 4]);
            let short = &full[..full.len().min(cut)];
            // Any of these may fail; none may panic.
            let _ = from_gbr(short);
        }
    }

    #[test]
    fn an_unknown_depth_is_refused() {
        // 2 (grey + alpha) and 3 (RGB) are not written by GIMP, so guessing at
        // a channel order would be inventing a format.
        assert!(from_gbr(&gbr(2, 2, 1, 2, "T", &[1, 2, 3, 4])).is_err());
        assert!(from_gbr(&gbr(2, 2, 1, 3, "T", &[1; 6])).is_err());
    }

    #[test]
    fn an_unknown_version_is_refused() {
        let mut file = gbr(2, 2, 2, 1, "T", &[1, 2, 3, 4]);
        file[4..8].copy_from_slice(&9u32.to_be_bytes());
        assert!(matches!(
            from_gbr(&file),
            Err(PresetError::UnsupportedVersion(_, 9))
        ));
    }
}
