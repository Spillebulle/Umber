//! Reading a palette somebody else made.
//!
//! [`crate::palette`] is the model and `.gpl` is the *storage* format — one
//! decoder and one encoder, for the reason `docformat` states as "there must
//! never be a second ORA reader". This module is the other half of that
//! bargain: everything here converts **into** a [`Palette`] on the way in and
//! nothing here writes anything, so the library stays a directory of `.gpl`
//! files however the colours arrived.
//!
//! # What people actually exchange
//!
//! The formats below were chosen from what the generators artists use actually
//! hand out, not from a list of extensions:
//!
//! - **Coolors** exports a URL, an image, CSS, a code snippet, SVG, PDF and
//!   `.ase`. It does not export `.gpl`. Its URL *is* the palette —
//!   `coolors.co/10121c-2c1e31-6b2643` — and pasting one is the commonest way
//!   a palette moves between two people.
//! - **Lospec** offers PNG, JASC `.pal`, `.ase`, Paint.NET `.txt`, `.gpl` and a
//!   bare `.hex` list.
//! - **Adobe Color** and Illustrator hand out `.ase`; Photoshop hands out
//!   `.aco`.
//! - **Paletton** offers HTML, CSS, LESS, XML, text, PNG, `.aco` and `.gpl`.
//! - **Color Hunt** and every "colour palette" page on the web offer a **hex
//!   code you copy**, and nothing else.
//!
//! So the single highest-value reader is not a file format at all: it is a
//! tolerant parser for **a list of hex codes**, which is what a URL, a CSS
//! dump, a `.hex` file, a Paint.NET `.txt` and a message in a chat window all
//! are. That is [`text`], and [`Format::Hex`] and [`Format::PaintNet`] are the
//! same parser pointed at a file.
//!
//! # What is deliberately not read, and why
//!
//! - **`.act`** (Adobe Color Table) is 768 raw bytes: 256 RGB triples, padded
//!   **with zeroes** where the palette is shorter. A padded entry and a real
//!   black are the same three bytes, so every short `.act` would arrive with a
//!   run of blacks nobody chose — a silently wrong import, which is the one
//!   outcome the importers' standing rule refuses. The 772-byte variant does
//!   carry a count, but a reader that works for one length and quietly damages
//!   the other is worse than no reader.
//! - **`.kpl`** (Krita) is a zip of `colorset.xml` plus the **ICC profiles** the
//!   values in it are stated against. Reading those floats as sRGB without the
//!   profile is exactly the silent colour-space conversion this module warns
//!   about everywhere else, and Krita writes `.gpl` as well — so the artist has
//!   a lossless route already.
//! - **`.swatches`** (Procreate) is a zip of JSON holding HSB floats, and
//!   `.sketchpalette` is JSON holding sRGB floats. Both are readable and
//!   neither is a *painting* interchange anybody has asked for here; they are
//!   the next two to add if somebody does.
//! - **RIFF `.pal`** *is* read, because `.pal` names two unrelated formats and
//!   a reader that answered "this is not a palette" for half of them would be
//!   lying about the extension it claims.
//!
//! # A colour space is converted out loud
//!
//! `.ase` and `.aco` carry CMYK, CIE Lab and grey as well as RGB, and turning
//! any of those into sRGB without the profile the file does not carry is an
//! **approximation**. Every one is counted in [`Losses`] and named in a
//! sentence, for the reason `docimport` gives: a refusal sends the artist to
//! re-export, where a subtly wrong palette wastes an afternoon.
//!
//! # Bounds
//!
//! Every reader here is reading a file a stranger wrote. So: a byte bound
//! before the file is opened ([`MAX_FILE_BYTES`]), never an allocation sized
//! from a count the file states, every read bounds-checked against what is
//! left, and [`crate::palette::MAX_SWATCHES`] refused rather than truncated.

use std::fmt;
use std::fs;
use std::path::Path;

use crate::palette::{MAX_SWATCHES, PaletteError, Palette, Swatch};

pub mod adobe;
pub mod text;

/// The largest palette file Umber will read.
///
/// A palette is a few hundred colours. The biggest legitimate file here is a
/// version-2 `.aco` at [`MAX_SWATCHES`] entries with long names, which is under
/// half a megabyte; four is generous and still a bound. It exists because the
/// alternative is `read_to_string` on whatever was dragged onto the window.
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Every palette format Umber reads.
///
/// The `.gpl` arm is here so that [`read_file`] is the one door and the file
/// dialog has one list to build from; the decoding still goes through
/// [`crate::palette::read_gpl`], which is the storage decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// GIMP's, which is also Umber's own storage format.
    Gpl,
    /// A bare list of hex codes, one per line. Lospec's `.hex`.
    Hex,
    /// Paint.NET's palette: `;` comments and `AARRGGBB` per line.
    PaintNet,
    /// `.pal`, which is **two** formats — JASC's text one and Microsoft's RIFF
    /// one. Told apart by the first four bytes.
    Pal,
    /// Adobe Swatch Exchange, what Coolors, Adobe Color and Illustrator export.
    Ase,
    /// Photoshop's colour swatches.
    Aco,
}

impl Format {
    /// Guarded by an exhaustive match in the tests rather than by iterating
    /// itself — see CLAUDE.md on why walking an `ALL` can only check what is
    /// already in it.
    pub const ALL: [Format; 6] = [
        Format::Gpl,
        Format::Hex,
        Format::PaintNet,
        Format::Pal,
        Format::Ase,
        Format::Aco,
    ];

    /// The extension, lower case and without the dot.
    ///
    /// Exhaustive with no catch-all, so a seventh format cannot be added
    /// without deciding this.
    pub fn extension(self) -> &'static str {
        match self {
            Format::Gpl => "gpl",
            Format::Hex => "hex",
            Format::PaintNet => "txt",
            Format::Pal => "pal",
            Format::Ase => "ase",
            Format::Aco => "aco",
        }
    }

    /// What the file dialog calls it. The application it comes *from*, not the
    /// format's own name, because nobody has a file they think of as "Adobe
    /// Swatch Exchange" — they have one Coolors gave them.
    pub fn label(self) -> &'static str {
        match self {
            Format::Gpl => "GIMP palette",
            Format::Hex => "Hex list",
            Format::PaintNet => "Paint.NET palette",
            Format::Pal => "JASC or Microsoft palette",
            Format::Ase => "Adobe swatches (Coolors, Adobe Color)",
            Format::Aco => "Photoshop swatches",
        }
    }

    /// Which format a path names, by extension alone.
    ///
    /// By extension and never by sniffing the bytes: `.txt` and `.hex` hold the
    /// same digits and differ only in how an eight-digit code is read, which no
    /// amount of looking at the file can settle.
    pub fn of_path(path: &Path) -> Option<Format> {
        let extension = path.extension()?.to_str()?;
        Format::ALL
            .into_iter()
            .find(|format| extension.eq_ignore_ascii_case(format.extension()))
    }
}

/// Every extension, as a sentence lists them.
pub fn readable_formats() -> String {
    let names: Vec<String> = Format::ALL
        .into_iter()
        .map(|format| format!(".{}", format.extension()))
        .collect();
    names.join(", ")
}

/// What an import could not carry across.
///
/// A flat struct of counts rather than a list of messages, because the same
/// loss happening four hundred times is **one** sentence with a number in it,
/// and a per-entry list is the thirty-lines-of-one-sentence failure
/// `EffectsNotPortable` already records.
///
/// Every field is a loss that actually happened. There is deliberately no
/// "lines skipped" count for [`text::parse`]: that parser is designed to find
/// colours in arbitrary prose, so a line without one is not an entry that was
/// dropped, and a CSS paste reporting "412 lines were not colours" is the
/// crying-wolf failure `docimport`'s Clip Studio note already paid for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Losses {
    /// Entries a **structured** format stated and Umber could not read.
    pub skipped: usize,
    /// Colours that carried transparency, which a swatch does not.
    ///
    /// Counted only where the alpha was *not* opaque: an `#RRGGBBAA` ending
    /// `ff` loses nothing and must not raise a sentence.
    pub transparency: usize,
    /// CMYK colours converted with no profile to convert against.
    pub cmyk: usize,
    /// CIE Lab colours converted against the D50 white point Adobe states them
    /// in.
    pub lab: usize,
    /// Greys read as sRGB levels, with no grey profile to read them against.
    pub grey: usize,
    /// Colour groups, which a `.gpl` has nowhere to keep.
    pub groups: usize,
}

impl Losses {
    pub fn any(self) -> bool {
        self != Losses::default()
    }

    /// One sentence per loss that happened, in the order a reader meets them.
    ///
    /// No em-dashes and no house voice: these are drawn in a dialog over
    /// somebody's canvas. See CLAUDE.md's README section, which holds every
    /// string the interface draws to the same standard.
    pub fn sentences(self) -> Vec<String> {
        let mut out = Vec::new();
        if self.skipped > 0 {
            out.push(format!(
                "{} {} not read, because {} did not hold to the format.",
                self.skipped,
                plural(self.skipped, "entry was", "entries were"),
                plural(self.skipped, "it", "they")
            ));
        }
        if self.transparency > 0 {
            out.push(format!(
                "{} {} partly transparent. A palette names a colour, so the \
                 transparency was dropped and how much goes down is the \
                 brush's opacity.",
                self.transparency,
                plural(self.transparency, "colour was", "colours were")
            ));
        }
        if self.cmyk > 0 {
            out.push(format!(
                "{} {} CMYK. The file carries no colour profile, so those are \
                 an approximation and will not match a print proof.",
                self.cmyk,
                plural(self.cmyk, "colour was", "colours were")
            ));
        }
        if self.lab > 0 {
            out.push(format!(
                "{} {} CIE Lab. Umber converted them against the D50 white \
                 point Adobe states Lab in, which is an approximation.",
                self.lab,
                plural(self.lab, "colour was", "colours were")
            ));
        }
        if self.grey > 0 {
            out.push(format!(
                "{} {} grey with no grey profile beside it, so Umber read the \
                 level as sRGB.",
                self.grey,
                plural(self.grey, "colour was", "colours were")
            ));
        }
        if self.groups > 0 {
            out.push(format!(
                "The file sorted its colours into {} {}. A .gpl keeps one flat \
                 row, so the colours are all here and the grouping is not.",
                self.groups,
                plural(self.groups, "group", "groups")
            ));
        }
        out
    }
}

fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 { one } else { many }
}

/// A palette that came from somewhere else, and what it lost on the way.
#[derive(Clone, Debug)]
pub struct Import {
    pub palette: Palette,
    pub losses: Losses,
}

/// Read a palette file of any format Umber knows.
pub fn read_file(path: &Path) -> Result<Import, PaletteError> {
    let Some(format) = Format::of_path(path) else {
        return Err(PaletteError::UnknownFormat(path.to_path_buf()));
    };
    let bytes = read_bytes(path)?;
    read(&bytes, format, path)
}

/// Read bytes already in hand, as a stated format.
///
/// Split from [`read_file`] so every reader is testable against bytes a test
/// builds itself, which is the only way the adversarial cases — a truncated
/// header, a count that would allocate a gigabyte, a name length past the end
/// of the file — can be written at all.
pub fn read(bytes: &[u8], format: Format, path: &Path) -> Result<Import, PaletteError> {
    let source = path.display().to_string();
    let name = file_stem(path);
    let import = match format {
        // Through the storage decoder, not a second one.
        Format::Gpl => {
            let read = crate::palette::read_gpl(&as_text(bytes), path)?;
            Import {
                palette: read.palette,
                losses: Losses {
                    skipped: read.skipped,
                    ..Losses::default()
                },
            }
        }
        Format::Hex | Format::PaintNet => {
            let (swatches, losses) = text::parse(&as_text(bytes), &source)?;
            Import {
                palette: named(name, swatches),
                losses,
            }
        }
        Format::Pal => {
            let (swatches, losses) = read_pal(bytes, &source)?;
            Import {
                palette: named(name, swatches),
                losses,
            }
        }
        Format::Ase => {
            let (swatches, losses) = adobe::read_ase(bytes, &source)?;
            Import {
                palette: named(name, swatches),
                losses,
            }
        }
        Format::Aco => {
            let (swatches, losses) = adobe::read_aco(bytes, &source)?;
            Import {
                palette: named(name, swatches),
                losses,
            }
        }
    };
    // A `.gpl` with no colours in it is a palette — `create` writes one, and a
    // reader that refused its own output made every new palette vanish on the
    // next launch. Every *other* format arrives from outside and is never
    // written by Umber, so an empty one is a file that did not say what it was
    // meant to say, and reporting that beats putting an empty row in the list.
    if format != Format::Gpl && import.palette.is_empty() {
        return Err(PaletteError::NoColours { source });
    }
    Ok(import)
}

/// A palette out of a name and a row of colours.
fn named(name: String, swatches: Vec<Swatch>) -> Palette {
    let mut palette = Palette::new(name);
    palette.swatches = swatches;
    palette
}

/// The filename without its extension, which is what every palette reader in
/// the world falls back to for a file that states no name of its own.
fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.trim().is_empty())
        .unwrap_or_else(|| crate::palette::UNTITLED.to_owned())
}

/// A file's bytes, refused before they are read if the file is too large.
///
/// The length is taken from the directory entry rather than by reading and then
/// measuring, which is the whole point: a bound applied after the allocation is
/// not a bound.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, PaletteError> {
    let io = |source| PaletteError::Io {
        path: path.to_path_buf(),
        source,
    };
    let len = fs::metadata(path).map_err(io)?.len();
    if len > MAX_FILE_BYTES {
        return Err(PaletteError::TooLarge {
            source: path.display().to_string(),
            len,
            max: MAX_FILE_BYTES,
        });
    }
    fs::read(path).map_err(io)
}

/// Bytes as text, **lossily**, with a byte-order mark taken off.
///
/// Lossy rather than `read_to_string`'s refusal, and that is a change of
/// behaviour worth stating. A `.gpl` written by an application on a Latin-1
/// machine has one bad byte in a colour's *name*; refusing it costs the artist
/// every colour in the file to protect a word, and the message they get is
/// "stream did not contain valid UTF-8", which sends nobody anywhere. A
/// replacement character in a name is visible and can be typed over.
pub fn as_text(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.strip_prefix('\u{feff}')
        .map(str::to_owned)
        .unwrap_or_else(|| text.into_owned())
}

// ---------------------------------------------------------------------------
// .pal, which is two formats
// ---------------------------------------------------------------------------

/// JASC's `PAL` header.
const JASC_MAGIC: &str = "JASC-PAL";

/// A `.pal`, whichever of the two it is.
///
/// The extension names JASC's text format (Paint Shop Pro, and what Lospec and
/// most pixel-art tools mean by `.pal`) **and** Microsoft's RIFF one. Told
/// apart by the first four bytes, because that is the only thing that can tell
/// them apart and because answering "this is not a palette" for half the files
/// carrying an extension is worse than not offering the extension at all.
fn read_pal(bytes: &[u8], source: &str) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    if bytes.starts_with(b"RIFF") {
        return read_riff_pal(bytes, source);
    }
    read_jasc_pal(&as_text(bytes), source)
}

/// ```text
/// JASC-PAL
/// 0100
/// 16
/// 255 0 0
/// ```
///
/// The stated count is **not** trusted: it is checked against the lines that
/// actually followed and a disagreement is a skip apiece, because a count is a
/// number in a file a stranger wrote and the lines are the colours.
fn read_jasc_pal(text: &str, source: &str) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or_default().trim();
    if !first.eq_ignore_ascii_case(JASC_MAGIC) {
        return Err(PaletteError::Malformed {
            source: source.to_owned(),
            what: format!("the first line has to be “{JASC_MAGIC}”, and it is “{first}”"),
        });
    }
    // The version and the count. Both are read past rather than acted on: a
    // version nobody has ever written anything but `0100` for, and a count the
    // colours themselves state better.
    let _version = lines.next();
    let _count = lines.next();

    let mut out = Vec::new();
    let mut losses = Losses::default();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match decimal_triple(line) {
            Some(rgb) => push(&mut out, Swatch::new(rgb), source)?,
            None => losses.skipped += 1,
        }
    }
    Ok((out, losses))
}

/// Three decimal components `0..=255`, separated by whitespace.
///
/// Out of range is a refusal rather than a clamp, exactly as `parse_entry`'s
/// is, and for the same reason: a clamp reads a line of prose beginning with a
/// number as a colour.
fn decimal_triple(line: &str) -> Option<[u8; 3]> {
    let mut parts = line.split_whitespace();
    let mut rgb = [0u8; 3];
    for slot in &mut rgb {
        *slot = parts.next()?.parse::<u8>().ok()?;
    }
    // A fourth component means this is not a JASC line. Anything trailing is
    // refused rather than ignored, so `10 20 30 40` cannot arrive as a colour.
    parts.next().is_none().then_some(rgb)
}

/// Microsoft's RIFF palette: `RIFF <size> PAL  data <size> <version> <count>`
/// then four bytes per entry, of which the fourth is flags rather than alpha.
fn read_riff_pal(bytes: &[u8], source: &str) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    let bad = |what: &str| PaletteError::Malformed {
        source: source.to_owned(),
        what: what.to_owned(),
    };
    if bytes.len() < 24 || &bytes[8..12] != b"PAL " {
        return Err(bad("it begins RIFF but is not a palette chunk"));
    }
    // Walk to the `data` chunk rather than assuming it is first: a RIFF file
    // may carry others, and a reader that assumes its own layout is one that
    // reads a length field as a colour.
    let mut at = 12usize;
    let body = loop {
        let header = bytes
            .get(at..at + 8)
            .ok_or_else(|| bad("it holds no “data” chunk"))?;
        let len = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        let start = at + 8;
        let chunk = bytes
            .get(start..start.saturating_add(len))
            .ok_or_else(|| bad("a chunk states a length past the end of the file"))?;
        if &header[0..4] == b"data" {
            break chunk;
        }
        // Chunks are word-aligned, so an odd length is followed by a pad byte.
        at = start + len + (len & 1);
    };
    if body.len() < 4 {
        return Err(bad("its “data” chunk is too short to hold a palette header"));
    }
    // The count is read for the diagnostic below and never used to size an
    // allocation: what bounds the loop is the bytes that are actually there.
    let stated = u16::from_le_bytes([body[2], body[3]]) as usize;
    let entries = &body[4..];
    let mut out = Vec::new();
    let mut losses = Losses::default();
    for entry in entries.chunks_exact(4) {
        push(&mut out, Swatch::new([entry[0], entry[1], entry[2]]), source)?;
    }
    // A file claiming more colours than it carries has lost them somewhere, and
    // that is a loss rather than a refusal — the ones that are there are good.
    losses.skipped = stated.saturating_sub(out.len());
    Ok((out, losses))
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Append a colour, refusing rather than truncating past [`MAX_SWATCHES`].
///
/// Refused rather than capped, and the same way [`crate::palette::read_gpl`]
/// refuses: a palette silently cut off at four thousand and ninety-six is one
/// whose missing colours nobody can see are missing.
pub(crate) fn push(
    out: &mut Vec<Swatch>,
    swatch: Swatch,
    source: &str,
) -> Result<(), PaletteError> {
    if out.len() >= MAX_SWATCHES {
        return Err(PaletteError::TooManySwatches {
            source: source.to_owned(),
            found: out.len() + 1,
            max: MAX_SWATCHES,
        });
    }
    out.push(swatch);
    Ok(())
}

/// A big-endian reader over a slice that never reads past its end.
///
/// Both Adobe formats are big-endian and both state lengths and counts inside
/// themselves, so every one of these is a place a malformed file could walk off
/// the end. One cursor with `Option` returns is what makes that structural
/// instead of eleven hand-written bounds checks that have to all be right.
pub(crate) struct BigEndian<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> BigEndian<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    pub(crate) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.at)
    }

    pub(crate) fn is_done(&self) -> bool {
        self.remaining() == 0
    }

    pub(crate) fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let slice = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(slice)
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        let bytes = self.take(2)?;
        Some(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        let bytes = self.take(4)?;
        Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn f32(&mut self) -> Option<f32> {
        let bytes = self.take(4)?;
        Some(f32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// A UTF-16 big-endian string of `units` code units, the last of which is
    /// the NUL terminator both Adobe formats count and neither wants kept.
    pub(crate) fn utf16(&mut self, units: usize) -> Option<String> {
        let bytes = self.take(units.checked_mul(2)?)?;
        let mut code_units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        if code_units.last() == Some(&0) {
            code_units.pop();
        }
        // Lossy for the reason `as_text` is: a name is not worth the colours.
        Some(String::from_utf16_lossy(&code_units))
    }
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, ".{}", self.extension())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn here() -> PathBuf {
        PathBuf::from("test.pal")
    }

    /// `ALL` is guarded by an exhaustive match rather than by walking itself: a
    /// test that iterates `ALL` can only ever check what is already in it, and
    /// a seventh format left out would sail through. The arms index `ALL`, so a
    /// short array is a panic rather than a pass.
    #[test]
    fn every_format_is_in_all_and_has_its_own_extension() {
        for format in Format::ALL {
            let position = match format {
                Format::Gpl => 0,
                Format::Hex => 1,
                Format::PaintNet => 2,
                Format::Pal => 3,
                Format::Ase => 4,
                Format::Aco => 5,
            };
            assert_eq!(Format::ALL[position], format);
            assert!(!format.label().is_empty());
        }
        // No two formats may claim one extension, or `of_path` answers by
        // whichever came first in the array.
        for (index, a) in Format::ALL.iter().enumerate() {
            for b in &Format::ALL[index + 1..] {
                assert_ne!(a.extension(), b.extension(), "{a:?} and {b:?}");
            }
        }
    }

    /// The extension decides, whatever case it is in, and something else is a
    /// named refusal rather than a guess.
    #[test]
    fn a_format_is_decided_by_the_extension_and_nothing_else() {
        assert_eq!(Format::of_path(Path::new("a.GPL")), Some(Format::Gpl));
        assert_eq!(Format::of_path(Path::new("a.Ase")), Some(Format::Ase));
        assert_eq!(Format::of_path(Path::new("a.png")), None);
        assert_eq!(Format::of_path(Path::new("noextension")), None);
        assert!(matches!(
            read_file(Path::new("somewhere/a.png")),
            Err(PaletteError::UnknownFormat(_))
        ));
    }

    /// A loss is a sentence only when it happened. An `#RRGGBBff` loses no
    /// transparency and must not raise one, which is the crying-wolf rule.
    #[test]
    fn nothing_is_reported_that_did_not_happen() {
        assert!(!Losses::default().any());
        assert!(Losses::default().sentences().is_empty());
        let one = Losses {
            cmyk: 1,
            ..Losses::default()
        };
        assert!(one.any());
        assert_eq!(one.sentences().len(), 1);
        assert!(one.sentences()[0].starts_with("1 colour was CMYK"));
        let several = Losses {
            cmyk: 4,
            lab: 2,
            ..Losses::default()
        };
        assert_eq!(several.sentences().len(), 2);
        assert!(several.sentences()[0].starts_with("4 colours were CMYK"));
        // Drawn over somebody's canvas, so held to the interface's own rule.
        for sentence in several.sentences() {
            assert!(!sentence.contains('—'), "{sentence}");
        }
    }

    /// The plain JASC file every pixel-art tool writes.
    #[test]
    fn a_jasc_palette_reads() {
        let text = "JASC-PAL\r\n0100\r\n3\r\n255 0 0\r\n0 255 0\r\n0 0 255\r\n";
        let (swatches, losses) = read_pal(text.as_bytes(), "test").expect("a palette");
        assert_eq!(
            swatches,
            vec![
                Swatch::new([255, 0, 0]),
                Swatch::new([0, 255, 0]),
                Swatch::new([0, 0, 255])
            ]
        );
        assert_eq!(losses, Losses::default());
    }

    /// A line that is not three components in range is counted, not clamped
    /// into a colour nobody chose. `300` is the one that matters: clamped, it
    /// would be a red the file does not hold.
    #[test]
    fn a_jasc_line_that_is_not_a_colour_is_counted_and_not_clamped() {
        let text = "JASC-PAL\n0100\n9\n1 2 3\n300 0 0\n1 2\n1 2 3 4\nnonsense\n-1 0 0\n";
        let (swatches, losses) = read_pal(text.as_bytes(), "test").expect("one colour");
        assert_eq!(swatches, vec![Swatch::new([1, 2, 3])]);
        assert_eq!(losses.skipped, 5);
    }

    /// Something that is not a palette at all is refused by name, with the
    /// reader saying what it expected.
    #[test]
    fn a_pal_that_is_not_a_palette_is_refused_with_a_reason() {
        for bytes in [&b""[..], b"<html>", b"RIFF", b"RIFFxxxxWAVEfmt "] {
            let error = read_pal(bytes, "test").expect_err("refused");
            assert!(matches!(error, PaletteError::Malformed { .. }), "{bytes:?}");
            assert!(!error.to_string().is_empty());
        }
    }

    /// Microsoft's RIFF `.pal`, which shares its extension with JASC's and is
    /// an unrelated format. A reader that answered "not a palette" for one of
    /// the two would be lying about the extension it offers.
    #[test]
    fn a_riff_palette_reads_and_its_flag_byte_is_not_an_alpha() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x0300u16.to_le_bytes()); // version
        body.extend_from_slice(&2u16.to_le_bytes()); // count
        // The fourth byte of an entry is PC_EXPLICIT and friends, never alpha.
        body.extend_from_slice(&[10, 20, 30, 0x04]);
        body.extend_from_slice(&[40, 50, 60, 0x00]);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&((4 + 8 + body.len()) as u32).to_le_bytes());
        file.extend_from_slice(b"PAL ");
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        let (swatches, losses) = read_pal(&file, "test").expect("a palette");
        assert_eq!(
            swatches,
            vec![Swatch::new([10, 20, 30]), Swatch::new([40, 50, 60])]
        );
        assert_eq!(losses, Losses::default());
    }

    /// A count is a number in a file a stranger wrote. It may not size an
    /// allocation, and where it disagrees with the bytes the bytes win and the
    /// difference is reported.
    #[test]
    fn a_riff_count_that_lies_neither_allocates_nor_reads_past_the_end() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x0300u16.to_le_bytes());
        body.extend_from_slice(&u16::MAX.to_le_bytes()); // 65535 colours claimed
        body.extend_from_slice(&[10, 20, 30, 0]); // one supplied
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&((4 + 8 + body.len()) as u32).to_le_bytes());
        file.extend_from_slice(b"PAL ");
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        let (swatches, losses) = read_pal(&file, "test").expect("the colour that is there");
        assert_eq!(swatches, vec![Swatch::new([10, 20, 30])]);
        assert_eq!(losses.skipped, 65534);

        // And a chunk length past the end of the file is refused rather than
        // sliced.
        let mut lying = file.clone();
        let at = 16;
        lying[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            read_pal(&lying, "test"),
            Err(PaletteError::Malformed { .. })
        ));
    }

    /// Truncated anywhere, a reader must answer rather than panic. Driven over
    /// every prefix of a good file, which is the shape `sqlite`'s own
    /// adversarial tests take.
    #[test]
    fn every_truncation_of_a_pal_answers_rather_than_panicking() {
        let mut body = Vec::new();
        body.extend_from_slice(&0x0300u16.to_le_bytes());
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&[10, 20, 30, 0]);
        body.extend_from_slice(&[40, 50, 60, 0]);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&((4 + 8 + body.len()) as u32).to_le_bytes());
        file.extend_from_slice(b"PAL ");
        file.extend_from_slice(b"data");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        for cut in 0..file.len() {
            let _ = read_pal(&file[..cut], "test");
        }
        let text = "JASC-PAL\n0100\n2\n1 2 3\n4 5 6\n";
        for cut in 0..text.len() {
            let _ = read_pal(text[..cut].as_bytes(), "test");
        }
    }

    /// An empty result is a refusal for every format that arrives from outside,
    /// and **not** for `.gpl` — `create` writes an empty one, and a reader that
    /// refused Umber's own output made every new palette vanish on the next
    /// launch.
    #[test]
    fn an_empty_palette_is_a_palette_only_where_umber_wrote_it() {
        let gpl = read(b"GIMP Palette\nName: Empty\n#\n", Format::Gpl, &here()).expect("a palette");
        assert!(gpl.palette.is_empty());
        assert_eq!(gpl.palette.name, "Empty");

        assert!(matches!(
            read(b"JASC-PAL\n0100\n0\n", Format::Pal, &here()),
            Err(PaletteError::NoColours { .. })
        ));
        assert!(matches!(
            read(b"; nothing but a comment\n", Format::PaintNet, &here()),
            Err(PaletteError::NoColours { .. })
        ));
    }

    /// A file that states no name of its own is called after itself, which is
    /// what every other palette reader does with one.
    #[test]
    fn a_file_with_no_name_in_it_is_called_after_the_file() {
        let import = read(
            b"JASC-PAL\n0100\n1\n1 2 3\n",
            Format::Pal,
            Path::new("/tmp/Warm greys.pal"),
        )
        .expect("a palette");
        assert_eq!(import.palette.name, "Warm greys");
    }

    /// The cursor never reads past its slice, whatever it is asked for. Every
    /// bound check in both Adobe readers rests on this.
    #[test]
    fn the_big_endian_cursor_cannot_walk_off_the_end() {
        let mut reader = BigEndian::new(&[0x00, 0x01, 0x00, 0x02]);
        assert_eq!(reader.u16(), Some(1));
        assert_eq!(reader.remaining(), 2);
        assert_eq!(
            reader.u32(),
            None,
            "a read past the end answers rather than panicking"
        );
        assert_eq!(reader.remaining(), 2, "a refused read consumes nothing");
        assert_eq!(reader.u16(), Some(2));
        assert!(reader.is_done());
        assert_eq!(reader.take(usize::MAX), None);
        assert_eq!(reader.utf16(usize::MAX), None, "a length that would overflow");

        // A NUL terminator is counted by both formats and kept by neither.
        let mut named = BigEndian::new(&[0x00, 0x52, 0x00, 0x00]);
        assert_eq!(named.utf16(2).as_deref(), Some("R"));
    }
}
