//! Photoshop brushes (`.abr`).
//!
//! Four incompatible layouts share this extension — 1, 2, 6.1 and 6.2 — and all
//! of them are big-endian. The reference this is written against is GIMP's own
//! `app/core/gimpbrush-load.c`, which is GPL-3.0 like Umber and is the only
//! description of the format that has been checked against real files for
//! twenty years.
//!
//! ```text
//! u16 version        1, 2, 6 or 10
//! u16 count          the number of brushes — or, for 6 and 10, the sub-version
//! ```
//!
//! Versions 1 and 2 then list brushes directly, each a `u16` type and a `u32`
//! block size. Versions 6 and 10 wrap everything in Photoshop's `8BIM`
//! sections and put the brushes in `samp`.
//!
//! # Sampled brushes only, deliberately
//!
//! A Photoshop brush is either **sampled** — a bitmap, which is what every pack
//! anyone shares is made of — or **computed**, a handful of parameters. GIMP
//! reads only the sampled ones, and so does this, for the same reason: in
//! versions 6 and 10 the parameters do not live with the brush at all. They are
//! in a separate `8BIMdesc` section written in Photoshop's *descriptor* format,
//! a nested self-describing structure with a dozen type codes that would be a
//! second format implemented inside the first, for numbers Umber has four of.
//!
//! So a `.abr` brings its stamps and nothing else, and [`dropped_features`]
//! says so. Spacing, dynamics, angle and roundness all come out as Umber's
//! defaults, which for a stamp brush is what the `.gbr` reader does anyway.
//!
//! # What is dropped
//!
//! - **Computed brushes**, as above — skipped, and counted, so the import can
//!   say how many did not come.
//! - **Everything in the descriptor**: spacing, angle, roundness, scatter and
//!   the dual-brush and texture options.
//! - **Wide brushes.** A stamp taller than 16 384 rows uses a different row
//!   table that GIMP does not read either, and no brush is that tall.

use crate::brush::Brush;
use crate::preset::PresetError;
use crate::tip::TipMask;

/// The largest stamp this will build. `TipMask` refuses more anyway; catching
/// it here means a corrupt header cannot size an allocation first.
const MAX_SIDE: i64 = TipMask::MAX_SIZE as i64;

/// Refuse a file claiming more brushes than a real pack has. Adobe's own sets
/// run to a few hundred.
const MAX_BRUSHES: usize = 4096;

/// One sampled Photoshop brush.
#[derive(Clone, Debug)]
pub struct AbrBrush {
    /// Version 2 records a name per brush; 1, 6 and 10 do not.
    pub name: Option<String>,
    pub tip: TipMask,
    /// Fraction of the brush size, converted from Photoshop's percentage.
    /// `None` for versions 6 and 10, which keep it in the descriptor.
    pub spacing: Option<f32>,
}

/// Everything a `.abr` held.
#[derive(Debug)]
pub struct AbrFile {
    pub brushes: Vec<AbrBrush>,
    /// How many brushes were computed rather than sampled, and so skipped.
    pub computed: usize,
    /// True when the settings live in a descriptor section this does not read.
    pub settings_elsewhere: bool,
}

/// Decode a Photoshop `.abr` file.
pub fn from_abr(bytes: &[u8]) -> Result<AbrFile, PresetError> {
    let mut at = Reader::new(bytes);
    let version = at.u16()?;
    let count = at.u16()?;

    let mut file = AbrFile {
        brushes: Vec::new(),
        computed: 0,
        settings_elsewhere: false,
    };

    match version {
        1 | 2 => read_v12(&mut at, version, count as usize, &mut file)?,
        // For these the count field is the sub-version, and only 1 and 2 exist.
        6 | 10 if count == 1 || count == 2 => {
            file.settings_elsewhere = true;
            read_v6(&mut at, count, &mut file)?;
        }
        6 | 10 => {
            return Err(PresetError::UnsupportedVersion(
                None,
                version as u32 * 10 + count as u32,
            ));
        }
        other => return Err(PresetError::UnsupportedVersion(None, other as u32)),
    }

    if file.brushes.is_empty() {
        return Err(PresetError::Malformed(
            None,
            if file.computed > 0 {
                format!(
                    "all {} of its brushes are Photoshop's *computed* kind, which is a \
                     description rather than a stamp — Umber has no equivalent",
                    file.computed
                )
            } else {
                "it holds no brushes Umber can read".to_string()
            },
        ));
    }
    Ok(file)
}

/// Turn a decoded stamp into the brush Umber paints with.
///
/// Deliberately the same three decisions [`super::gbr::to_brush`] makes, for
/// the same reasons: the mask's long side is the size, the file's spacing is
/// used where there is one, and pressure is left off because the format carries
/// no dynamics.
pub fn to_brush(brush: AbrBrush) -> (Brush, TipMask) {
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

/// What reading this `.abr` will throw away.
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    let Ok(file) = from_abr(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if file.computed > 0 {
        out.push("Photoshop's computed brushes");
    }
    if file.settings_elsewhere {
        out.push("Photoshop brush settings");
    }
    out
}

// ---------------------------------------------------------------------------
// Versions 1 and 2
// ---------------------------------------------------------------------------

fn read_v12(
    at: &mut Reader<'_>,
    version: u16,
    count: usize,
    file: &mut AbrFile,
) -> Result<(), PresetError> {
    for _ in 0..count.min(MAX_BRUSHES) {
        let kind = at.u16()?;
        let size = at.u32()? as usize;
        let block = at.position();

        // 1 is a computed brush and 2 is a sampled one. Anything else is a
        // version of the format nobody has documented; skip it by its declared
        // length, which is what the length is for.
        if kind != 2 {
            if kind == 1 {
                file.computed += 1;
            }
            at.seek(block.saturating_add(size))?;
            continue;
        }

        let _misc = at.u32()?;
        let spacing = at.u16()?;
        let name = if version == 2 { Some(at.ucs2()?) } else { None };
        let _antialiasing = at.u8()?;
        // The short bounds are a legacy duplicate; the long ones are the truth.
        for _ in 0..4 {
            at.u16()?;
        }
        let (top, left, bottom, right) = (at.i32()?, at.i32()?, at.i32()?, at.i32()?);
        let depth = at.u16()?;

        let (width, height) = extent(left, top, right, bottom)?;
        if depth >> 3 != 1 {
            return Err(malformed(format!(
                "a {depth}-bit stamp; Photoshop's sampled brushes are 8-bit"
            )));
        }
        if height > 16384 {
            return Err(malformed(
                "a brush taller than 16384 rows, which uses a row table this does not read"
                    .to_string(),
            ));
        }

        let compressed = at.u8()?;
        let coverage = pixels(at, width, height, 1, compressed)?;

        file.brushes.push(AbrBrush {
            name: name.filter(|n| !n.trim().is_empty()),
            tip: TipMask::new(width as u32, height as u32, coverage)?,
            // Photoshop states spacing as a percentage of the brush size, the
            // same quantity `.gbr` records and off by the same hundred. Zero
            // means unset there and here.
            spacing: (spacing > 0).then(|| (spacing as f32 / 100.0).clamp(0.01, 4.0)),
        });

        // Back to the declared end of the block rather than wherever the reads
        // landed: a writer is allowed to pad, and following the length is what
        // keeps the *next* brush findable.
        at.seek(block.saturating_add(size))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Versions 6 and 10
// ---------------------------------------------------------------------------

fn read_v6(at: &mut Reader<'_>, subversion: u16, file: &mut AbrFile) -> Result<(), PresetError> {
    reach_8bim(at, b"samp")?;
    let section = at.u32()? as usize;
    let end = at.position().saturating_add(section).min(at.len());

    while at.position() < end && file.brushes.len() < MAX_BRUSHES {
        let size = at.u32()? as usize;
        // Brushes are padded to a four-byte boundary.
        let next = at.position() + size.next_multiple_of(4);

        // A fixed run of key, coordinates and unknown bytes. Two lengths,
        // because 6.1 and 6.2 differ by exactly this — it is the whole of the
        // difference between the sub-versions, and getting it wrong reads the
        // bounds out of the middle of a string.
        at.skip(if subversion == 1 { 47 } else { 301 })?;

        let (top, left, bottom, right) = (at.i32()?, at.i32()?, at.i32()?, at.i32()?);
        let depth = at.u16()? >> 3;
        let compressed = at.u8()?;

        let (width, height) = extent(left, top, right, bottom)?;
        if compressed == 1 && depth != 1 {
            return Err(malformed(
                "a compressed stamp deeper than 8 bits, which Photoshop does not write".to_string(),
            ));
        }
        if !(1..=2).contains(&depth) {
            return Err(malformed(format!("a {}-bit stamp", depth * 8)));
        }

        let coverage = pixels(at, width, height, depth, compressed)?;
        file.brushes.push(AbrBrush {
            name: None,
            tip: TipMask::new(width as u32, height as u32, coverage)?,
            spacing: None,
        });

        if at.position() > next {
            return Err(malformed("a brush that overran its own block".to_string()));
        }
        at.seek(next)?;
    }
    Ok(())
}

/// Walk Photoshop's `8BIM` sections until the one named `want`.
///
/// Stops *before* the section length, which the caller reads — the length is
/// how far the section runs, and only the caller knows whether it wants to skip
/// it or walk into it.
fn reach_8bim(at: &mut Reader<'_>, want: &[u8; 4]) -> Result<(), PresetError> {
    loop {
        let tag = at.tag()?;
        if &tag != b"8BIM" {
            return Err(malformed(
                "its sections are not Photoshop's, so this is not an .abr".to_string(),
            ));
        }
        if &at.tag()? == want {
            return Ok(());
        }
        let size = at.u32()? as usize;
        at.skip(size)?;
    }
}

// ---------------------------------------------------------------------------
// Shared
// ---------------------------------------------------------------------------

/// Width and height from a bounding box, checked before either sizes anything.
fn extent(left: i32, top: i32, right: i32, bottom: i32) -> Result<(i64, i64), PresetError> {
    let width = right as i64 - left as i64;
    let height = bottom as i64 - top as i64;
    if !(1..=MAX_SIDE).contains(&width) || !(1..=MAX_SIDE).contains(&height) {
        return Err(malformed(format!(
            "a stamp of {width}x{height}, which is not a plausible brush"
        )));
    }
    Ok((width, height))
}

/// The stamp itself: raw, 16-bit, or Photoshop's row-wise PackBits.
fn pixels(
    at: &mut Reader<'_>,
    width: i64,
    height: i64,
    depth: u16,
    compressed: u8,
) -> Result<Vec<u8>, PresetError> {
    let texels = (width * height) as usize;
    match (compressed, depth) {
        (0, 1) => Ok(at.take(texels)?.to_vec()),
        // Photoshop's 16-bit stamps are little-endian inside a big-endian file,
        // which is not a mistake in this code: GIMP reads them the same way.
        (0, 2) => Ok(at
            .take(texels * 2)?
            .chunks_exact(2)
            .map(|px| (u16::from_le_bytes([px[0], px[1]]) >> 8) as u8)
            .collect()),
        (1, _) => rle(at, width as usize, height as usize),
        _ => Err(malformed(format!(
            "compression method {compressed}, which is neither raw nor PackBits"
        ))),
    }
}

/// PackBits, one run per row, preceded by a table of the rows' compressed
/// lengths.
///
/// The same scheme a PSD uses. Written out rather than borrowed from the `psd`
/// crate because that one decodes a layer, not a loose buffer, and this is
/// twenty lines.
fn rle(at: &mut Reader<'_>, width: usize, height: usize) -> Result<Vec<u8>, PresetError> {
    let mut lengths = Vec::with_capacity(height);
    for _ in 0..height {
        lengths.push(at.u16()? as usize);
    }

    let mut out = Vec::with_capacity(width * height);
    for length in lengths {
        let row = at.take(length)?;
        let mut i = 0;
        while i < row.len() {
            let n = row[i] as i8;
            i += 1;
            if n == -128 {
                // Photoshop's documented no-op.
                continue;
            }
            if n < 0 {
                let repeat = (-(n as i32) + 1) as usize;
                let &value = row.get(i).ok_or_else(short)?;
                i += 1;
                if out.len() + repeat > width * height {
                    return Err(short());
                }
                out.extend(std::iter::repeat_n(value, repeat));
            } else {
                let run = n as usize + 1;
                let bytes = row.get(i..i + run).ok_or_else(short)?;
                i += run;
                if out.len() + run > width * height {
                    return Err(short());
                }
                out.extend_from_slice(bytes);
            }
        }
    }
    if out.len() != width * height {
        return Err(short());
    }
    Ok(out)
}

fn short() -> PresetError {
    malformed("a stamp whose compressed rows do not fill it".to_string())
}

fn malformed(message: String) -> PresetError {
    PresetError::Malformed(None, format!("the .abr describes {message}"))
}

// ---------------------------------------------------------------------------
// Bytes
// ---------------------------------------------------------------------------

/// A big-endian cursor that never panics and never trusts a length.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn position(&self) -> usize {
        self.at
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], PresetError> {
        let end = self.at.checked_add(n).ok_or_else(truncated)?;
        let out = self.bytes.get(self.at..end).ok_or_else(truncated)?;
        self.at = end;
        Ok(out)
    }

    fn skip(&mut self, n: usize) -> Result<(), PresetError> {
        self.take(n).map(|_| ())
    }

    /// Move to an absolute offset. Past the end is an error rather than a
    /// clamp: it means a length in the file was wrong, and carrying on would
    /// read the next brush out of the middle of this one.
    fn seek(&mut self, to: usize) -> Result<(), PresetError> {
        if to > self.bytes.len() {
            return Err(truncated());
        }
        self.at = to;
        Ok(())
    }

    fn u8(&mut self) -> Result<u8, PresetError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, PresetError> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, PresetError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, PresetError> {
        self.u32().map(|v| v as i32)
    }

    fn tag(&mut self) -> Result<[u8; 4], PresetError> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }

    /// A length-prefixed UTF-16BE string, as version 2 records a brush's name.
    fn ucs2(&mut self) -> Result<String, PresetError> {
        let chars = self.u32()? as usize;
        if chars > 1024 {
            return Err(malformed(format!("a {chars}-character brush name")));
        }
        let bytes = self.take(chars * 2)?;
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }
}

fn truncated() -> PresetError {
    PresetError::Malformed(
        None,
        "the .abr ends in the middle of a brush — is it truncated?".to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything here is built byte by byte from GIMP's reader, which is the
    /// only description of this format that has been checked against real files.
    /// No `.abr` is vendored: see `docs/brush-sources.md` for why none of the
    /// packs Umber fetches is one.
    fn be16(v: u16) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    fn be32(v: u32) -> Vec<u8> {
        v.to_be_bytes().to_vec()
    }

    /// A version 1 or 2 sampled brush block, header and all.
    fn sampled_v12(version: u16, name: &str, width: u32, height: u32, data: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend(be32(0)); // misc
        body.extend(be16(30)); // spacing, per cent
        if version == 2 {
            body.extend(be32(name.chars().count() as u32));
            for unit in name.encode_utf16() {
                body.extend(be16(unit));
            }
        }
        body.push(0); // antialiasing
        for _ in 0..4 {
            body.extend(be16(0)); // the legacy short bounds
        }
        for value in [0, 0, height, width] {
            body.extend(be32(value)); // top, left, bottom, right
        }
        body.extend(be16(8)); // depth
        body.push(0); // uncompressed
        body.extend_from_slice(data);

        let mut out = be16(2); // sampled
        out.extend(be32(body.len() as u32));
        out.extend(body);
        out
    }

    #[test]
    fn a_version_2_file_yields_its_stamps_with_their_names() {
        let mut file = be16(2);
        file.extend(be16(2)); // two brushes
        file.extend(sampled_v12(2, "Chalk", 2, 2, &[10, 20, 30, 40]));
        file.extend(sampled_v12(2, "Spatter", 1, 2, &[50, 60]));

        let read = from_abr(&file).expect("decode");
        assert_eq!(read.brushes.len(), 2);
        assert_eq!(read.brushes[0].name.as_deref(), Some("Chalk"));
        assert_eq!(read.brushes[0].tip.coverage(), [10, 20, 30, 40]);
        assert_eq!(read.brushes[0].spacing, Some(0.3));
        // The second is only found if the first block's length was honoured.
        assert_eq!(read.brushes[1].name.as_deref(), Some("Spatter"));
        assert_eq!(read.brushes[1].tip.coverage(), [50, 60]);
        assert_eq!(dropped_features(&file), ["Photoshop brush settings"; 0]);
    }

    /// Version 1 has no name field, so reading it as version 2 takes four bytes
    /// of the antialiasing flag and the bounds for a string length.
    #[test]
    fn version_1_has_no_per_brush_name() {
        let mut file = be16(1);
        file.extend(be16(1));
        file.extend(sampled_v12(1, "", 2, 1, &[7, 8]));
        let read = from_abr(&file).expect("decode");
        assert_eq!(read.brushes.len(), 1);
        assert!(read.brushes[0].name.is_none());
        assert_eq!(read.brushes[0].tip.coverage(), [7, 8]);
    }

    /// A computed brush is skipped by its declared length, and counted so the
    /// import can say how many did not arrive.
    #[test]
    fn a_computed_brush_is_skipped_and_reported() {
        let mut computed = be16(1); // type 1
        computed.extend(be32(6));
        computed.extend([0u8; 6]);

        let mut file = be16(2);
        file.extend(be16(2));
        file.extend(computed);
        file.extend(sampled_v12(2, "Real", 1, 1, &[99]));

        let read = from_abr(&file).expect("decode");
        assert_eq!(read.computed, 1);
        assert_eq!(read.brushes.len(), 1);
        assert_eq!(read.brushes[0].tip.coverage(), [99]);
        assert_eq!(dropped_features(&file), ["Photoshop's computed brushes"]);
    }

    /// The whole point of the length-driven walk: a block with padding after
    /// the pixels must not shift the next brush.
    #[test]
    fn padding_after_a_brush_does_not_move_the_next_one() {
        let mut padded = sampled_v12(2, "Padded", 1, 1, &[5]);
        // Grow the declared block size and add the bytes it now claims.
        let size = u32::from_be_bytes([padded[2], padded[3], padded[4], padded[5]]);
        padded[2..6].copy_from_slice(&(size + 8).to_be_bytes());
        padded.extend([0u8; 8]);

        let mut file = be16(2);
        file.extend(be16(2));
        file.extend(padded);
        file.extend(sampled_v12(2, "After", 1, 1, &[6]));

        let read = from_abr(&file).expect("decode");
        assert_eq!(read.brushes.len(), 2);
        assert_eq!(read.brushes[1].name.as_deref(), Some("After"));
    }

    /// PackBits, row by row, with a table of compressed row lengths in front.
    #[test]
    fn a_compressed_stamp_unpacks_row_by_row() {
        // Two rows of four: the first a run, the second a literal.
        let rows: [Vec<u8>; 2] = [vec![253, 77], vec![3, 1, 2, 3, 4]];
        let mut body = Vec::new();
        body.extend(be32(0));
        body.extend(be16(10));
        body.extend(be32(0)); // no name (version 1)
        // …that last one is wrong for version 1; build the block by hand.
        body.clear();
        body.extend(be32(0)); // misc
        body.extend(be16(10)); // spacing
        body.push(0); // antialiasing
        for _ in 0..4 {
            body.extend(be16(0));
        }
        for value in [0u32, 0, 2, 4] {
            body.extend(be32(value));
        }
        body.extend(be16(8));
        body.push(1); // compressed
        for row in &rows {
            body.extend(be16(row.len() as u16));
        }
        for row in &rows {
            body.extend_from_slice(row);
        }

        let mut block = be16(2);
        block.extend(be32(body.len() as u32));
        block.extend(body);

        let mut file = be16(1);
        file.extend(be16(1));
        file.extend(block);

        let read = from_abr(&file).expect("decode");
        // 253 as i8 is -3, so "repeat the next byte four times".
        assert_eq!(read.brushes[0].tip.coverage(), [77, 77, 77, 77, 1, 2, 3, 4]);
    }

    /// 6.1 and 6.2 differ by the length of one fixed run, and nothing else.
    /// Reading one as the other takes the bounds out of the middle of a string.
    #[test]
    fn the_two_sub_versions_of_6_differ_only_in_a_skip() {
        let brush = |subversion: u16, data: &[u8]| {
            let mut body = vec![0u8; if subversion == 1 { 47 } else { 301 }];
            for value in [0u32, 0, 1, 2] {
                body.extend(be32(value)); // top, left, bottom, right
            }
            body.extend(be16(8));
            body.push(0); // uncompressed
            body.extend_from_slice(data);

            let mut out = be32(body.len() as u32);
            out.extend(body);
            // Padded to four bytes, as the format requires.
            while !(out.len() - 4).is_multiple_of(4) {
                out.push(0);
            }
            out
        };

        for subversion in [1u16, 2] {
            let mut samp = brush(subversion, &[123, 45]);
            samp.extend(brush(subversion, &[67, 89]));

            let mut file = be16(6);
            file.extend(be16(subversion));
            file.extend_from_slice(b"8BIMdesc");
            file.extend(be32(3));
            file.extend([0u8; 3]);
            file.extend_from_slice(b"8BIMsamp");
            file.extend(be32(samp.len() as u32));
            file.extend(samp);

            let read = from_abr(&file).unwrap_or_else(|e| panic!("6.{subversion}: {e}"));
            assert_eq!(read.brushes.len(), 2, "6.{subversion}");
            assert_eq!(read.brushes[0].tip.coverage(), [123, 45]);
            assert_eq!(read.brushes[1].tip.coverage(), [67, 89]);
            // No spacing, because it is in the descriptor section.
            assert!(read.brushes[0].spacing.is_none());
            assert_eq!(dropped_features(&file), ["Photoshop brush settings"]);
        }
    }

    #[test]
    fn an_unsupported_version_is_refused_by_number() {
        assert!(matches!(
            from_abr(&[0, 9, 0, 1]),
            Err(PresetError::UnsupportedVersion(_, 9))
        ));
        // 6.3 does not exist; the message has to name it rather than 6.
        assert!(matches!(
            from_abr(&[0, 6, 0, 3]),
            Err(PresetError::UnsupportedVersion(_, 63))
        ));
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_panic() {
        assert!(from_abr(b"").is_err());
        assert!(from_abr(b"nope").is_err());
        assert!(dropped_features(b"nope").is_empty());

        let mut full = be16(2);
        full.extend(be16(1));
        full.extend(sampled_v12(2, "Chalk", 2, 2, &[10, 20, 30, 40]));
        for cut in 0..full.len() {
            // Any of these may fail; none may panic.
            let _ = from_abr(&full[..cut]);
        }

        // A little-endian read of the header, which is the classic mistake:
        // it must fail rather than allocate a gigabyte.
        let mut wrong = 2u16.to_le_bytes().to_vec();
        wrong.extend(1u16.to_le_bytes());
        wrong.extend([0u8; 32]);
        assert!(from_abr(&wrong).is_err());
    }

    #[test]
    fn a_stamp_becomes_a_square_tip_at_its_own_size() {
        let mut file = be16(2);
        file.extend(be16(1));
        file.extend(sampled_v12(2, "Wide", 4, 2, &[255; 8]));
        let read = from_abr(&file).expect("decode");
        let (brush, tip) = to_brush(read.brushes.into_iter().next().expect("one"));
        assert_eq!((tip.width(), tip.height()), (4, 4));
        assert_eq!(brush.size, 4.0);
        assert_eq!(brush.spacing, 0.3);
        assert!(!brush.pressure_size);
    }
}
