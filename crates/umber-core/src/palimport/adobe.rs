//! Adobe's two swatch formats, `.ase` and `.aco`.
//!
//! These are the binary half of the research: **Coolors, Adobe Color and
//! Illustrator all hand out `.ase`**, Lospec offers it beside its `.gpl`, and
//! Photoshop and Paletton hand out `.aco`. Between them they are what a
//! designer's palette arrives as when it did not arrive as text.
//!
//! # A colour space is converted out loud
//!
//! Both formats carry CMYK, CIE Lab and grey as well as RGB, and **neither
//! carries the profile those are stated against**. So every conversion here is
//! an approximation, and every one is counted in [`Losses`] and named in a
//! sentence rather than performed quietly:
//!
//! - **CMYK** takes the naive `(1 − ink)(1 − black)` straight into sRGB bytes.
//!   There is no better answer without a profile, and there is deliberately no
//!   `Color` round trip on this path: the naive formula is defined *on* the
//!   encoded value, so pushing it through linear light and back would be a
//!   second, differently-wrong answer wearing the costume of rigour.
//! - **Lab** is converted against the **D50** white point, which is the one
//!   Adobe states Lab in, through the Bradford-adapted D50→sRGB matrix. That
//!   goes through linear [`Color`] and therefore through the one `to_srgb_u8`,
//!   never a second `powf`.
//! - **Grey** is read as an sRGB level. A grey with no grey profile beside it
//!   has no defined gamma, so this is a choice and it is named as one.
//!
//! # Reading a file a stranger wrote
//!
//! Every length, count and name size in both formats is a number inside the
//! file. None of them sizes an allocation, every read goes through
//! [`BigEndian`], and a structure that stops making sense stops the loop and is
//! **counted** rather than throwing away the colours that were already good.
//! A file whose *signature* is wrong is refused by name, because that is the
//! one case where nothing at all was understood.

use crate::color::{Color, Hsv};
use crate::palette::{PaletteError, Swatch};

use super::text::from_linear;
use super::{BigEndian, Losses, push};

/// What every `.ase` begins with.
const ASE_MAGIC: &[u8; 4] = b"ASEF";

/// An `.ase` block that is one colour.
const ASE_COLOUR: u16 = 0x0001;
/// An `.ase` block that opens a group.
const ASE_GROUP_START: u16 = 0xC001;
/// An `.ase` block that closes one.
const ASE_GROUP_END: u16 = 0xC002;

/// Adobe Swatch Exchange.
///
/// ```text
/// "ASEF" | u16 major | u16 minor | u32 blocks
/// per block: u16 type | u32 length | <length bytes>
///   a colour: u16 name units | utf-16be name | 4-byte space | floats | u16 kind
/// ```
///
/// The stated block count is read and **not** trusted to size anything: what
/// bounds the loop is the bytes that are actually there, and a count that
/// promised more than the file holds is a named loss.
pub fn read_ase(bytes: &[u8], source: &str) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    let mut reader = BigEndian::new(bytes);
    if reader.take(4) != Some(ASE_MAGIC) {
        return Err(PaletteError::Malformed {
            source: source.to_owned(),
            what: "it does not begin “ASEF”, so it is not an Adobe swatch file".to_owned(),
        });
    }
    // The version. Read past rather than refused: every file in the wild is
    // 1.0, and refusing an unseen minor version would turn a file that is
    // probably fine into one nobody can open.
    let _major = reader.u16();
    let _minor = reader.u16();
    let stated = reader.u32().unwrap_or(0) as usize;

    let mut out = Vec::new();
    let mut losses = Losses::default();
    let mut blocks = 0usize;
    while blocks < stated && !reader.is_done() {
        let Some(kind) = reader.u16() else { break };
        let Some(length) = reader.u32() else { break };
        // The block's own bytes, so a colour that under-reads or over-reads
        // cannot drag the cursor off the next block's header.
        let Some(body) = reader.take(length as usize) else {
            break;
        };
        blocks += 1;
        match kind {
            ASE_COLOUR => match ase_colour(body, &mut losses) {
                Some(swatch) => push(&mut out, swatch, source)?,
                None => losses.skipped += 1,
            },
            // The colours are all kept and only the grouping is dropped, which
            // is what a `.gpl` can carry. Counted once per group, not per
            // colour inside one.
            ASE_GROUP_START => losses.groups += 1,
            ASE_GROUP_END => {}
            _ => losses.skipped += 1,
        }
    }
    // A file that promised more blocks than it carried lost them somewhere
    // before it reached here, and the ones that are here are still good.
    losses.skipped += stated.saturating_sub(blocks);
    Ok((out, losses))
}

/// One `.ase` colour block.
fn ase_colour(body: &[u8], losses: &mut Losses) -> Option<Swatch> {
    let mut reader = BigEndian::new(body);
    let units = reader.u16()? as usize;
    let name = reader.utf16(units)?;
    let space = reader.take(4)?;
    let mut floats = [0f32; 4];
    let wanted = match space {
        b"RGB " | b"LAB " => 3,
        b"CMYK" => 4,
        b"Gray" => 1,
        _ => return None,
    };
    for slot in floats.iter_mut().take(wanted) {
        let value = reader.f32()?;
        // A NaN reaching `Hsv` or a `powf` is the class of bug this codebase
        // has already paid for once. Refused here, at the door.
        if !value.is_finite() {
            return None;
        }
        *slot = value;
    }
    // The colour kind — global, spot or normal — which changes nothing about
    // what the colour *is*, so it is read past rather than acted on.
    let _kind = reader.u16();

    let swatch = match space {
        b"RGB " => from_linear_srgb(floats[0], floats[1], floats[2]),
        b"CMYK" => {
            losses.cmyk += 1;
            from_cmyk(floats[0], floats[1], floats[2], floats[3])
        }
        b"LAB " => {
            losses.lab += 1;
            // Adobe states Lab with L in 0..100 and a/b in roughly -128..127.
            from_lab(floats[0] * 100.0, floats[1], floats[2])
        }
        b"Gray" => {
            losses.grey += 1;
            from_grey(floats[0])
        }
        _ => return None,
    };
    Some(Swatch {
        name: crate::palette::clean_line(&name),
        ..swatch
    })
}

// ---------------------------------------------------------------------------
// .aco
// ---------------------------------------------------------------------------

/// Photoshop's colour swatches.
///
/// ```text
/// u16 version | u16 count | per colour: u16 space | u16 w | u16 x | u16 y | u16 z
/// version 2 adds, after the four values: u32 name units | utf-16be name
/// ```
///
/// Photoshop writes a **version 1 block and then a version 2 block** in one
/// file, so that an older reader takes the first and stops. This reads the
/// first, then looks for the second and prefers it where it is there — that is
/// the only one carrying the colours' names.
pub fn read_aco(bytes: &[u8], source: &str) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    let mut reader = BigEndian::new(bytes);
    let version = reader.u16().ok_or_else(|| PaletteError::Malformed {
        source: source.to_owned(),
        what: "it is too short to hold even a version number".to_owned(),
    })?;
    if version != 1 && version != 2 {
        return Err(PaletteError::Malformed {
            source: source.to_owned(),
            what: format!(
                "it states version {version}, and Photoshop swatch files are version 1 or 2"
            ),
        });
    }
    let first = aco_block(&mut reader, version == 2, source)?;

    // The version 2 block, if this file carries one. Anything that does not
    // read as a whole second block is ignored rather than refused: the first
    // block is a complete palette and trailing bytes are not worth losing it
    // over.
    if version == 1
        && let Some(2) = reader.u16()
        && let Ok(second) = aco_block(&mut reader, true, source)
        && second.0.len() >= first.0.len()
    {
        return Ok(second);
    }
    Ok(first)
}

/// One `.aco` block: a count, then that many entries.
fn aco_block(
    reader: &mut BigEndian<'_>,
    named: bool,
    source: &str,
) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    let stated = reader.u16().ok_or_else(|| PaletteError::Malformed {
        source: source.to_owned(),
        what: "it states no colour count".to_owned(),
    })? as usize;
    let mut out = Vec::new();
    let mut losses = Losses::default();
    let mut read = 0usize;
    while read < stated {
        let Some(space) = reader.u16() else { break };
        let Some(values) = reader.take(8) else { break };
        let value = |at: usize| u16::from_be_bytes([values[at * 2], values[at * 2 + 1]]);
        // The name comes after the values in version 2, and has to be consumed
        // whether or not the colour reads, or every entry after a bad one lands
        // at the wrong offset.
        let name = if named {
            let Some(units) = reader.u32() else { break };
            let Some(name) = reader.utf16(units as usize) else {
                break;
            };
            crate::palette::clean_line(&name)
        } else {
            String::new()
        };
        read += 1;
        match aco_colour(space, [value(0), value(1), value(2), value(3)], &mut losses) {
            Some(swatch) => push(&mut out, Swatch { name, ..swatch }, source)?,
            None => losses.skipped += 1,
        }
    }
    losses.skipped += stated.saturating_sub(read);
    Ok((out, losses))
}

/// One `.aco` entry, whose four sixteen-bit values mean different things in
/// every colour space.
fn aco_colour(space: u16, values: [u16; 4], losses: &mut Losses) -> Option<Swatch> {
    /// A sixteen-bit channel as a fraction.
    fn unit(value: u16) -> f32 {
        value as f32 / 65535.0
    }
    match space {
        // RGB, each channel spread over the whole sixteen bits.
        0 => Some(from_linear_srgb(
            unit(values[0]),
            unit(values[1]),
            unit(values[2]),
        )),
        // HSB. Through the one `Hsv`, which is exact and is the same conversion
        // the colour picker uses — a second one here would be the drift this
        // codebase refuses everywhere.
        1 => Some(Swatch::of(
            Hsv::new(unit(values[0]) * 360.0, unit(values[1]), unit(values[2])).to_color(1.0),
        )),
        // CMYK, and the trap: Photoshop stores these **inverted**, so 0 is a
        // hundred per cent ink and 65535 is none. Reading them the obvious way
        // round turns every cyan into red.
        2 => {
            losses.cmyk += 1;
            Some(from_cmyk(
                1.0 - unit(values[0]),
                1.0 - unit(values[1]),
                1.0 - unit(values[2]),
                1.0 - unit(values[3]),
            ))
        }
        // Lab: L is 0..10000 for 0..100, and a and b are **signed**, 0..±12800
        // for 0..±128.
        7 => {
            losses.lab += 1;
            Some(from_lab(
                values[0] as f32 / 100.0,
                values[1] as i16 as f32 / 100.0,
                values[2] as i16 as f32 / 100.0,
            ))
        }
        // Grey, stated as 0..10000 for nought to a hundred per cent.
        8 => {
            losses.grey += 1;
            Some(from_grey((values[0] as f32 / 10000.0).clamp(0.0, 1.0)))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Colour spaces
// ---------------------------------------------------------------------------

/// An sRGB triple already stated as fractions.
///
/// Straight to bytes and **not** through linear light: these are already sRGB
/// values, so `from_linear` would encode a second time and lighten every
/// colour in the file.
fn from_linear_srgb(r: f32, g: f32, b: f32) -> Swatch {
    let level = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Swatch::new([level(r), level(g), level(b)])
}

/// The naive CMYK conversion, straight into sRGB bytes.
///
/// There is no better answer without the profile the file does not carry, and
/// the naive formula is defined on the *encoded* value — so routing it through
/// linear light and back would be a second, differently wrong answer that
/// looked more careful. It is counted and named instead; see the module docs.
fn from_cmyk(c: f32, m: f32, y: f32, k: f32) -> Swatch {
    let level = |ink: f32| {
        let value = (1.0 - ink.clamp(0.0, 1.0)) * (1.0 - k.clamp(0.0, 1.0));
        (value * 255.0 + 0.5) as u8
    };
    Swatch::new([level(c), level(m), level(y)])
}

/// A grey level with no grey profile beside it, read as sRGB.
fn from_grey(level: f32) -> Swatch {
    let byte = (level.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Swatch::new([byte, byte, byte])
}

/// CIE L\*a\*b\* against the **D50** white point, which is the one Adobe states
/// Lab in, to sRGB.
///
/// Lab → XYZ(D50) → linear sRGB through the Bradford-adapted matrix that the
/// sRGB ICC profile itself carries, then out through [`Color`] and therefore
/// through the one `to_srgb_u8`. Approximate because the file names no profile,
/// which is why every caller counts it.
fn from_lab(l: f32, a: f32, b: f32) -> Swatch {
    /// `(6/29)^3`, the point the cube root gives way to its linear tail.
    const EPSILON: f32 = 216.0 / 24389.0;
    /// `(29/3)^3`, the slope of that tail.
    const KAPPA: f32 = 24389.0 / 27.0;
    /// The D50 illuminant, normalised so Y is one.
    const WHITE: [f32; 3] = [0.964_22, 1.0, 0.825_21];

    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let inverse = |f: f32| {
        let cubed = f * f * f;
        if cubed > EPSILON {
            cubed
        } else {
            (116.0 * f - 16.0) / KAPPA
        }
    };
    let x = WHITE[0] * inverse(fx);
    let y = WHITE[1]
        * if l > KAPPA * EPSILON {
            fy * fy * fy
        } else {
            l / KAPPA
        };
    let z = WHITE[2] * inverse(fz);

    let (r, g, blue) = (
        3.133_856 * x - 1.616_866_7 * y - 0.490_614_6 * z,
        -0.978_768_4 * x + 1.916_141_5 * y + 0.033_454_0 * z,
        0.071_945_3 * x - 0.228_991_4 * y + 1.405_242_7 * z,
    );
    // Non-finite in means non-finite out, and a NaN reaching `to_srgb_u8` is
    // a byte nobody can predict.
    if !r.is_finite() || !g.is_finite() || !blue.is_finite() {
        return Swatch::of(Color::BLACK);
    }
    from_linear(r, g, blue)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an `.ase` the way Coolors and Illustrator do, so the reader is
    /// tested against the shape it will actually meet.
    fn ase(blocks: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = Vec::from(ASE_MAGIC);
        out.extend_from_slice(&1u16.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes());
        out.extend_from_slice(&(blocks.len() as u32).to_be_bytes());
        for (kind, body) in blocks {
            out.extend_from_slice(&kind.to_be_bytes());
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(body);
        }
        out
    }

    fn ase_name(name: &str) -> Vec<u8> {
        let units: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let mut out = Vec::from((units.len() as u16).to_be_bytes());
        for unit in units {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
    }

    fn ase_colour_block(name: &str, space: &[u8; 4], values: &[f32]) -> (u16, Vec<u8>) {
        let mut body = ase_name(name);
        body.extend_from_slice(space);
        for value in values {
            body.extend_from_slice(&value.to_be_bytes());
        }
        body.extend_from_slice(&2u16.to_be_bytes());
        (ASE_COLOUR, body)
    }

    /// The ordinary case: an RGB `.ase` with names on it, which is what
    /// Coolors, Adobe Color and Lospec all hand out.
    #[test]
    fn an_rgb_ase_reads_with_its_names_and_loses_nothing() {
        let file = ase(&[
            ase_colour_block(
                "Eerie black",
                b"RGB ",
                &[16.0 / 255.0, 18.0 / 255.0, 28.0 / 255.0],
            ),
            ase_colour_block(
                "Ochre",
                b"RGB ",
                &[204.0 / 255.0, 119.0 / 255.0, 34.0 / 255.0],
            ),
        ]);
        let (swatches, losses) = read_ase(&file, "test").expect("a palette");
        assert_eq!(swatches.len(), 2);
        assert_eq!(swatches[0].rgb, [16, 18, 28]);
        assert_eq!(swatches[0].name, "Eerie black");
        assert_eq!(swatches[1].rgb, [204, 119, 34]);
        assert_eq!(
            losses,
            Losses::default(),
            "an RGB file with names loses nothing at all"
        );
    }

    /// An RGB `.ase` states **sRGB** fractions, so pushing them through linear
    /// light would lighten every colour in the file. Driven over every byte,
    /// because a gamma error is a smooth curve that looks plausible at a
    /// glance and is exactly wrong in the midtones.
    #[test]
    fn an_ase_rgb_value_is_the_byte_it_was_written_as() {
        for byte in 0..=255u8 {
            let file = ase(&[ase_colour_block("", b"RGB ", &[byte as f32 / 255.0; 3])]);
            let (swatches, _) = read_ase(&file, "test").expect("a palette");
            assert_eq!(swatches[0].rgb, [byte, byte, byte], "{byte}");
        }
    }

    /// A colour space that is not RGB is converted **and named**. Silence here
    /// is the failure the standing rule is about: a CMYK swatch that quietly
    /// became a slightly wrong RGB one wastes an afternoon.
    #[test]
    fn a_converted_colour_space_is_always_named() {
        let file = ase(&[
            ase_colour_block("Cyan", b"CMYK", &[1.0, 0.0, 0.0, 0.0]),
            ase_colour_block("Mid", b"Gray", &[0.5]),
            ase_colour_block("White", b"LAB ", &[1.0, 0.0, 0.0]),
        ]);
        let (swatches, losses) = read_ase(&file, "test").expect("a palette");
        assert_eq!(swatches.len(), 3);
        assert_eq!(losses.cmyk, 1);
        assert_eq!(losses.grey, 1);
        assert_eq!(losses.lab, 1);
        assert_eq!(losses.sentences().len(), 3);
        for sentence in losses.sentences() {
            assert!(
                sentence.contains("approximation") || sentence.contains("sRGB"),
                "{sentence}"
            );
        }
        // Cyan is a cyan and not a red, which is the naive conversion's one
        // job. Lab white is white, which is what says the matrix is the right
        // way round rather than merely present.
        assert_eq!(swatches[0].rgb, [0, 255, 255]);
        assert_eq!(swatches[1].rgb, [128, 128, 128]);
        let white = swatches[2].rgb;
        assert!(
            white.iter().all(|&c| c >= 253),
            "Lab L=100 is white, and this is {white:?}"
        );
    }

    /// Lab's landmarks, which is the only way to say the D50 matrix is the
    /// right one rather than merely a matrix. Black and white are exact; a mid
    /// grey is a grey and nothing else.
    #[test]
    fn the_lab_conversion_lands_on_the_colours_it_should() {
        assert_eq!(from_lab(0.0, 0.0, 0.0).rgb, [0, 0, 0]);
        let white = from_lab(100.0, 0.0, 0.0).rgb;
        assert!(white.iter().all(|&c| c >= 253), "{white:?}");
        let grey = from_lab(50.0, 0.0, 0.0).rgb;
        assert!(
            grey[0] == grey[1] && grey[1] == grey[2] && (110..=130).contains(&grey[0]),
            "a neutral Lab is a neutral sRGB: {grey:?}"
        );
        // Positive a is towards red, positive b towards yellow. Getting either
        // sign wrong is a palette that looks plausible and is not the file's.
        let red = from_lab(54.0, 81.0, 70.0).rgb;
        assert!(red[0] > red[1] && red[1] >= red[2], "{red:?}");
        // Nothing here may produce a NaN byte.
        for bad in [f32::NAN, f32::INFINITY, -f32::INFINITY] {
            let _ = from_lab(bad, bad, bad);
        }
    }

    /// Groups are dropped and named. The colours inside one are all kept — a
    /// `.gpl` is one flat row and that is the whole of what is lost.
    #[test]
    fn ase_groups_are_flattened_and_the_flattening_is_named() {
        let file = ase(&[
            (ASE_GROUP_START, ase_name("Warm")),
            ase_colour_block("Ochre", b"RGB ", &[0.8, 0.46, 0.13]),
            (ASE_GROUP_END, Vec::new()),
            (ASE_GROUP_START, ase_name("Cool")),
            ase_colour_block("Slate", b"RGB ", &[0.17, 0.24, 0.31]),
            (ASE_GROUP_END, Vec::new()),
        ]);
        let (swatches, losses) = read_ase(&file, "test").expect("a palette");
        assert_eq!(swatches.len(), 2, "every colour survives the flattening");
        assert_eq!(losses.groups, 2);
        assert_eq!(losses.skipped, 0);
        assert!(losses.sentences()[0].contains("2 groups"));
    }

    /// Something that is not an `.ase` at all is refused by name, because
    /// nothing about it was understood.
    #[test]
    fn a_file_that_is_not_an_ase_is_refused_by_name() {
        for bytes in [&b""[..], b"<html>", b"ASE", b"ASFF\0\x01\0\0\0\0\0\x01"] {
            assert!(
                matches!(read_ase(bytes, "test"), Err(PaletteError::Malformed { .. })),
                "{bytes:?}"
            );
        }
    }

    /// **The bound that matters.** A block count and a block length are numbers
    /// in a file a stranger wrote. Neither may size an allocation, and a length
    /// past the end of the file must stop the read rather than slice past it —
    /// with the colours already read still good, and the difference named.
    #[test]
    fn an_ase_that_lies_about_its_own_size_neither_allocates_nor_reads_past_the_end() {
        // Four thousand million blocks promised, one supplied.
        let mut file = ase(&[ase_colour_block("Ochre", b"RGB ", &[0.8, 0.46, 0.13])]);
        file[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        let (swatches, losses) = read_ase(&file, "test").expect("the colour that is there");
        assert_eq!(swatches.len(), 1);
        assert_eq!(losses.skipped, u32::MAX as usize - 1);

        // A block whose stated length runs off the end.
        let mut lying = ase(&[ase_colour_block("Ochre", b"RGB ", &[0.8, 0.46, 0.13])]);
        let at = 12 + 2;
        lying[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let (swatches, losses) = read_ase(&lying, "test").expect("nothing, but no panic");
        assert!(swatches.is_empty());
        assert_eq!(losses.skipped, 1);

        // A name length past the end of its own block.
        let mut long_name = ase(&[ase_colour_block("Ochre", b"RGB ", &[0.8, 0.46, 0.13])]);
        let at = 12 + 6;
        long_name[at..at + 2].copy_from_slice(&u16::MAX.to_be_bytes());
        let (swatches, losses) = read_ase(&long_name, "test").expect("no panic");
        assert!(swatches.is_empty());
        assert_eq!(losses.skipped, 1);
    }

    /// Truncated at every byte, a reader answers rather than panicking.
    #[test]
    fn every_truncation_of_an_ase_answers_rather_than_panicking() {
        let file = ase(&[
            (ASE_GROUP_START, ase_name("Warm")),
            ase_colour_block("Ochre", b"RGB ", &[0.8, 0.46, 0.13]),
            ase_colour_block("Sea", b"CMYK", &[0.6, 0.1, 0.2, 0.05]),
            ase_colour_block("Lab", b"LAB ", &[0.5, 20.0, -30.0]),
            (ASE_GROUP_END, Vec::new()),
        ]);
        for cut in 0..file.len() {
            let _ = read_ase(&file[..cut], "test");
        }
        // And a float that is not a number never reaches a colour.
        let nan = ase(&[ase_colour_block("Bad", b"RGB ", &[f32::NAN, 0.0, 0.0])]);
        let (swatches, losses) = read_ase(&nan, "test").expect("no panic");
        assert!(swatches.is_empty());
        assert_eq!(losses.skipped, 1);
    }

    /// Build an `.aco` block: a count and that many entries.
    fn aco_entries(named: bool, entries: &[(u16, [u16; 4], &str)]) -> Vec<u8> {
        let mut out = Vec::from((entries.len() as u16).to_be_bytes());
        for (space, values, name) in entries {
            out.extend_from_slice(&space.to_be_bytes());
            for value in values {
                out.extend_from_slice(&value.to_be_bytes());
            }
            if named {
                let units: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
                out.extend_from_slice(&(units.len() as u32).to_be_bytes());
                for unit in units {
                    out.extend_from_slice(&unit.to_be_bytes());
                }
            }
        }
        out
    }

    /// Photoshop writes a version 1 block and then a version 2 one, so an old
    /// reader takes the first and stops. Umber reads both and keeps the second,
    /// because that is the only one carrying the names.
    #[test]
    fn a_photoshop_aco_keeps_the_names_out_of_its_second_block() {
        let entries = [
            (0u16, [0xCCCC, 0x7777, 0x2222, 0], "Ochre"),
            (0u16, [0x1010, 0x1212, 0x1c1c, 0], "Eerie black"),
        ];
        let mut file = Vec::from(1u16.to_be_bytes());
        file.extend_from_slice(&aco_entries(false, &entries));
        file.extend_from_slice(&2u16.to_be_bytes());
        file.extend_from_slice(&aco_entries(true, &entries));

        let (swatches, losses) = read_aco(&file, "test").expect("a palette");
        assert_eq!(swatches.len(), 2);
        assert_eq!(swatches[0].rgb, [204, 119, 34]);
        assert_eq!(swatches[0].name, "Ochre");
        assert_eq!(swatches[1].name, "Eerie black");
        assert_eq!(losses, Losses::default());

        // A version 1 file on its own is a palette with no names, not a
        // refusal: plenty of files in the wild are exactly that.
        let mut alone = Vec::from(1u16.to_be_bytes());
        alone.extend_from_slice(&aco_entries(false, &entries));
        let (swatches, _) = read_aco(&alone, "test").expect("a palette");
        assert_eq!(swatches.len(), 2);
        assert!(swatches[0].name.is_empty());
    }

    /// **Photoshop stores CMYK inverted**, so nought is a hundred per cent ink.
    /// Read the obvious way round, every cyan in the file becomes a red. This
    /// is the one thing about `.aco` that cannot be got right by reading the
    /// bytes carefully.
    #[test]
    fn an_aco_cmyk_value_is_inverted_and_a_cyan_is_a_cyan() {
        // Full cyan: 0 in the cyan channel, 65535 in the other three.
        let file = {
            let mut out = Vec::from(1u16.to_be_bytes());
            out.extend_from_slice(&aco_entries(
                false,
                &[(2u16, [0, 0xFFFF, 0xFFFF, 0xFFFF], "")],
            ));
            out
        };
        let (swatches, losses) = read_aco(&file, "test").expect("a palette");
        assert_eq!(swatches[0].rgb, [0, 255, 255], "a cyan came back as a red");
        assert_eq!(losses.cmyk, 1);
    }

    /// The other three `.aco` spaces, each of which states its numbers
    /// differently. HSB goes through the one `Hsv` rather than a second
    /// conversion written here.
    #[test]
    fn every_aco_colour_space_lands_where_it_should() {
        let entries = [
            (1u16, [0, 0xFFFF, 0xFFFF], [255, 0, 0]),      // HSB red
            (1u16, [0x5555, 0xFFFF, 0xFFFF], [0, 255, 0]), // HSB green
            (8u16, [10000, 0, 0], [255, 255, 255]),        // full grey
            (8u16, [0, 0, 0], [0, 0, 0]),                  // no grey
        ];
        for (space, values, want) in entries {
            let mut file = Vec::from(1u16.to_be_bytes());
            file.extend_from_slice(&aco_entries(
                false,
                &[(space, [values[0], values[1], values[2], 0], "")],
            ));
            let (swatches, _) = read_aco(&file, "test").expect("a palette");
            let got = swatches[0].rgb;
            assert!(
                got.iter().zip(&want).all(|(a, b)| a.abs_diff(*b) <= 1),
                "space {space} {values:?}: wanted {want:?}, got {got:?}"
            );
        }
        // Lab's a and b are signed, and reading them unsigned puts every
        // negative one at the far end of the axis.
        let mut file = Vec::from(1u16.to_be_bytes());
        file.extend_from_slice(&aco_entries(
            false,
            &[(7u16, [5000, (-4000i16) as u16, 0, 0], "")],
        ));
        let (swatches, losses) = read_aco(&file, "test").expect("a palette");
        assert_eq!(losses.lab, 1);
        let green = swatches[0].rgb;
        assert!(
            green[1] > green[0],
            "a negative a is towards green: {green:?}"
        );
    }

    /// A count is a number in a file a stranger wrote, and this one is the
    /// worst case: sixty-five thousand entries claimed and one supplied.
    #[test]
    fn an_aco_that_lies_about_its_count_neither_allocates_nor_panics() {
        let mut file = Vec::from(1u16.to_be_bytes());
        file.extend_from_slice(&u16::MAX.to_be_bytes());
        file.extend_from_slice(&0u16.to_be_bytes());
        file.extend_from_slice(&[0u8; 8]);
        let (swatches, losses) = read_aco(&file, "test").expect("the colour that is there");
        assert_eq!(swatches.len(), 1);
        assert_eq!(losses.skipped, u16::MAX as usize - 1);
    }

    /// Refused by name where nothing was understood, and answering rather than
    /// panicking at every truncation.
    #[test]
    fn an_aco_that_is_not_one_is_refused_and_every_truncation_answers() {
        for bytes in [&b""[..], b"\x00", b"\x00\x09\x00\x01"] {
            assert!(
                matches!(read_aco(bytes, "test"), Err(PaletteError::Malformed { .. })),
                "{bytes:?}"
            );
        }
        let mut file = Vec::from(2u16.to_be_bytes());
        file.extend_from_slice(&aco_entries(
            true,
            &[(0u16, [1, 2, 3, 0], "Ochre"), (2u16, [4, 5, 6, 7], "Sea")],
        ));
        for cut in 0..file.len() {
            let _ = read_aco(&file[..cut], "test");
        }
    }

    /// A palette past what Umber reads back is refused rather than truncated,
    /// in both binary readers — a palette silently cut off is one whose missing
    /// colours nobody can see are missing.
    #[test]
    fn both_binary_readers_refuse_a_palette_past_the_bound() {
        let blocks: Vec<(u16, Vec<u8>)> = (0..crate::palette::MAX_SWATCHES + 1)
            .map(|_| ase_colour_block("", b"RGB ", &[0.0, 0.0, 0.0]))
            .collect();
        assert!(matches!(
            read_ase(&ase(&blocks), "test"),
            Err(PaletteError::TooManySwatches { .. })
        ));

        let entries: Vec<(u16, [u16; 4], &str)> = (0..crate::palette::MAX_SWATCHES + 1)
            .map(|_| (0u16, [0, 0, 0, 0], ""))
            .collect();
        let mut file = Vec::from(1u16.to_be_bytes());
        file.extend_from_slice(&aco_entries(false, &entries));
        assert!(matches!(
            read_aco(&file, "test"),
            Err(PaletteError::TooManySwatches { .. })
        ));
    }
}
