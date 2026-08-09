//! Finding the colours in a piece of text.
//!
//! This is the reader that matters most, and it is not a file format. Research
//! into what artists actually exchange — see the module docs above — says the
//! commonest palette in the world is **a list of hex codes**: a Coolors URL
//! pasted into a chat window, the block of CSS variables a generator hands
//! back, the "copy" button on every colour-palette page, Lospec's `.hex`, and
//! Paint.NET's `.txt`. One tolerant parser reads all of them.
//!
//! # The rules, and why each one is needed
//!
//! - **A prefixed code may sit anywhere in a line.** `#RGB`, `#RGBA`,
//!   `#RRGGBB`, `#RRGGBBAA`, `0xRRGGBB`, `0xRRGGBBAA`, `rgb(…)` and `rgba(…)`.
//!   A `#` is a statement that what follows is a colour, so it is trusted in
//!   prose.
//! - **A bare code is a colour only on a line that holds nothing else.**
//!   `facade`, `beefed`, `accede` and `deadbeef` are all words made only of hex
//!   digits, so a bare run trusted in prose would put colours nobody chose into
//!   the palette — which is exactly the silently-wrong import the standing rule
//!   refuses. A `.hex` file is one bare code per line and passes; a sentence
//!   with the word "facade" in it does not.
//! - **Eight digits are read by whether they were prefixed.** `#RRGGBBAA` is
//!   CSS, which is what a `#` means everywhere on the web. A bare `AARRGGBB` is
//!   Paint.NET, which is the only thing that writes eight bare digits and puts
//!   the alpha first. One rule, right in both worlds, and the alternative — a
//!   mode flag per file type — would have been wrong for whichever of the two
//!   somebody pasted rather than opened.
//! - **A URL is unwrapped and read on its own.** `coolors.co/10121c-2c1e31` is
//!   a palette; the rest of the sentence around it is not. Each URL-looking
//!   token is taken out of the line, reduced to its last path segment and read
//!   as a line of bare codes, and what is left of the line is read normally.
//! - **A name is taken off the line only where the line holds exactly one
//!   colour**, so `--brand-primary: #cc7722;` arrives named and
//!   `coolors.co/a-b-c` does not arrive with three copies of the URL.
//!
//! # What it does not report
//!
//! There is no "lines skipped" count here, deliberately, and the reason is the
//! crying-wolf rule. This parser is *designed* to find colours in arbitrary
//! text, so a line without one is not an entry that was dropped — it is prose.
//! A pasted stylesheet reporting "412 lines were not colours" would be a notice
//! nobody reads, which costs the losses that matter. Transparency **is**
//! reported, because an `#RRGGBBAA` that was not opaque genuinely lost
//! something.

use std::borrow::Cow;

use crate::color::Color;
use crate::palette::{PaletteError, Swatch};

use super::{Losses, MAX_FILE_BYTES, push};

/// The longest leftover text that is still plausibly a colour's *name*.
///
/// Past this it is prose that happened to have a hex code in it, and a
/// paragraph in the name column of a palette is worse than no name.
const NAME_LIMIT: usize = 64;

/// Find every colour in a piece of text.
///
/// `source` is only ever used to phrase an error, so a caller with a paste and
/// no file passes a phrase — "the pasted text" — rather than a made-up path
/// somebody would go looking for.
pub fn parse(text: &str, source: &str) -> Result<(Vec<Swatch>, Losses), PaletteError> {
    // The same bound a file gets, applied to text that came off no file. An
    // unbounded parse of whatever was in the clipboard is the thing this stops.
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err(PaletteError::TooLarge {
            source: source.to_owned(),
            len: text.len() as u64,
            max: MAX_FILE_BYTES,
        });
    }

    let mut out = Vec::new();
    let mut losses = Losses::default();
    for raw in text.lines() {
        let line = raw.trim_start_matches('\u{feff}').trim();
        // `;` is Paint.NET's comment marker and `//` is every code snippet's.
        // A `#` is *not* a comment marker here and never can be: it is how a
        // colour is spelled.
        if line.is_empty() || line.starts_with(';') || line.starts_with("//") {
            continue;
        }
        read_line(line, &mut out, &mut losses, source)?;
    }
    Ok((out, losses))
}

/// One line: its URLs on their own, then whatever is left.
fn read_line(
    line: &str,
    out: &mut Vec<Swatch>,
    losses: &mut Losses,
    source: &str,
) -> Result<(), PaletteError> {
    if !line.split_whitespace().any(looks_like_url) {
        return collect(line, out, losses, source);
    }
    // A URL is read as a line of its own, because the sentence it was pasted
    // into is not part of the palette: "Look at coolors.co/10121c-2c1e31, nice"
    // has a palette in it and six words that are not colours, and the bare-code
    // rule would throw the palette away with them.
    let mut rest = String::new();
    for token in line.split_whitespace() {
        match unwrap_url(token) {
            Some(segment) => collect(&segment, out, losses, source)?,
            None => {
                rest.push_str(token);
                rest.push(' ');
            }
        }
    }
    collect(rest.trim(), out, losses, source)
}

/// One line with no URLs left in it.
fn collect(
    line: &str,
    out: &mut Vec<Swatch>,
    losses: &mut Losses,
    source: &str,
) -> Result<(), PaletteError> {
    let (found, prose) = scan(line);
    // The bare-code rule: a run of hex digits is a colour only where the line
    // holds nothing but colours and separators. See the module docs for the
    // words this exists to refuse.
    let keep: Vec<&Found> = found
        .iter()
        .filter(|found| found.prefixed || !prose)
        .collect();
    if keep.is_empty() {
        return Ok(());
    }
    // A name only where there is exactly one colour to give it to. Two colours
    // on a line have no way to divide one piece of text between them, and
    // guessing would put a URL in every swatch of a pasted palette.
    let name = (keep.len() == 1)
        .then(|| leftover_name(line, keep[0]))
        .flatten()
        .unwrap_or_default();
    for found in keep {
        if found.alpha != u8::MAX {
            losses.transparency += 1;
        }
        push(
            out,
            Swatch {
                rgb: found.rgb,
                name: name.clone(),
            },
            source,
        )?;
    }
    Ok(())
}

/// A colour found in a line, and where it sat.
struct Found {
    start: usize,
    end: usize,
    rgb: [u8; 3],
    /// Kept so a *lost* transparency can be counted and an opaque one cannot —
    /// a `#RRGGBBff` loses nothing and must raise no sentence.
    alpha: u8,
    /// Whether a `#`, an `0x` or an `rgb(` said this was a colour. A bare run
    /// of hex digits did not.
    prefixed: bool,
}

/// Every colour in a line, in the order they appear, and whether the line held
/// anything else.
fn scan(line: &str) -> (Vec<Found>, bool) {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    let mut prose = false;
    let mut at = 0usize;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'#' {
            let end = hex_run_end(bytes, at + 1);
            match hex_swatch(&line[at + 1..end], true) {
                Some((rgb, alpha)) => {
                    found.push(Found {
                        start: at,
                        end,
                        rgb,
                        alpha,
                        prefixed: true,
                    });
                    at = end;
                }
                // A `#` followed by nothing usable is punctuation. The digits
                // after it, if any, are consumed so they cannot then be read as
                // a bare code of the wrong length.
                None => {
                    prose |= end > at + 1;
                    at = end.max(at + 1);
                }
            }
            continue;
        }
        if byte == b'0'
            && matches!(bytes.get(at + 1), Some(b'x' | b'X'))
            && at_word_start(bytes, at)
        {
            let end = hex_run_end(bytes, at + 2);
            if let Some((rgb, alpha)) = hex_swatch(&line[at + 2..end], true) {
                found.push(Found {
                    start: at,
                    end,
                    rgb,
                    alpha,
                    prefixed: true,
                });
                at = end;
                continue;
            }
        }
        // `r` or `R` first, and **not** merely "the previous byte was not
        // alphanumeric": that reads true half way through a multi-byte
        // character, and `rgb_function` slices the line at `at`. Slicing inside
        // a character is a panic, and a panic on somebody's pasted text is the
        // worst thing this parser could do.
        if matches!(byte, b'r' | b'R')
            && at_word_start(bytes, at)
            && let Some((rgb, alpha, end)) = rgb_function(line, at)
        {
            found.push(Found {
                start: at,
                end,
                rgb,
                alpha,
                prefixed: true,
            });
            at = end;
            continue;
        }
        if byte.is_ascii_alphanumeric() {
            let end = word_end(bytes, at);
            let word = &line[at..end];
            match hex_swatch(word, false) {
                Some((rgb, alpha)) => found.push(Found {
                    start: at,
                    end,
                    rgb,
                    alpha,
                    prefixed: false,
                }),
                None => prose = true,
            }
            at = end;
            continue;
        }
        // A byte over 0x7f is the first of a multi-byte character, which is
        // certainly not a hex digit and certainly is text. Advancing by one
        // byte is safe because nothing below ever slices here: every span this
        // function records is ASCII and was found at an ASCII position.
        if byte >= 0x80 {
            prose = true;
        }
        at += 1;
    }
    (found, prose)
}

/// Whether the byte before `at` is one that could continue a word.
fn at_word_start(bytes: &[u8], at: usize) -> bool {
    at == 0 || !bytes[at - 1].is_ascii_alphanumeric()
}

/// One past the last hex digit of the run starting at `at`.
fn hex_run_end(bytes: &[u8], at: usize) -> usize {
    let mut end = at;
    while bytes.get(end).is_some_and(u8::is_ascii_hexdigit) {
        end += 1;
    }
    end
}

/// One past the last letter or digit of the word starting at `at`.
fn word_end(bytes: &[u8], at: usize) -> usize {
    let mut end = at;
    while bytes.get(end).is_some_and(u8::is_ascii_alphanumeric) {
        end += 1;
    }
    end
}

/// A run of hex digits as a colour and the alpha it carried.
///
/// The three-and-four digit short forms are **prefixed only**. A bare `abc` is
/// a word far more often than it is a colour, and a bare `f0f` is a filename.
/// Six and eight digits are accepted either way, and eight is the one place the
/// prefix changes the answer — see the module docs.
fn hex_swatch(digits: &str, prefixed: bool) -> Option<([u8; 3], u8)> {
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(&digits[at..at + 2], 16).ok();
    let wide = |at: usize| {
        let digit = u8::from_str_radix(&digits[at..at + 1], 16).ok()?;
        Some(digit * 17)
    };
    match (digits.len(), prefixed) {
        (3, true) => Some(([wide(0)?, wide(1)?, wide(2)?], u8::MAX)),
        (4, true) => Some(([wide(0)?, wide(1)?, wide(2)?], wide(3)?)),
        (6, _) => Some(([byte(0)?, byte(2)?, byte(4)?], u8::MAX)),
        // CSS puts the alpha last; Paint.NET puts it first and never writes a
        // `#`. So the prefix decides, and both files are read correctly.
        (8, true) => Some(([byte(0)?, byte(2)?, byte(4)?], byte(6)?)),
        (8, false) => Some(([byte(2)?, byte(4)?, byte(6)?], byte(0)?)),
        _ => None,
    }
}

/// `rgb(r, g, b)`, `rgba(r, g, b, a)`, and CSS 4's space-separated
/// `rgb(r g b / a)`, with or without percentages.
///
/// Returns the colour, its alpha and one past the closing bracket.
fn rgb_function(line: &str, at: usize) -> Option<([u8; 3], u8, usize)> {
    let rest = &line[at..];
    let open = rest.find('(')?;
    let head = &rest[..open];
    if !head.eq_ignore_ascii_case("rgb") && !head.eq_ignore_ascii_case("rgba") {
        return None;
    }
    let close = rest.find(')')?;
    if close < open {
        return None;
    }
    let args: Vec<&str> = rest[open + 1..close]
        .split([',', '/', ' ', '\t'])
        .filter(|piece| !piece.trim().is_empty())
        .collect();
    if args.len() < 3 || args.len() > 4 {
        return None;
    }
    let mut rgb = [0u8; 3];
    for (slot, arg) in rgb.iter_mut().zip(&args) {
        *slot = component(arg, 255.0)?;
    }
    // An alpha states a fraction rather than a level: `rgba(0,0,0,0.5)`.
    let alpha = match args.get(3) {
        Some(arg) => component(arg, 1.0)?,
        None => u8::MAX,
    };
    Some((rgb, alpha, at + close + 1))
}

/// One `rgb()` argument. `full` is what a bare `1.0` means: 255 for a colour
/// component, 1.0 for an alpha.
fn component(arg: &str, full: f32) -> Option<u8> {
    let arg = arg.trim();
    let (number, scale) = match arg.strip_suffix('%') {
        Some(number) => (number.trim(), 255.0 / 100.0),
        None => (arg, 255.0 / full),
    };
    let value: f32 = number.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some((value * scale).round().clamp(0.0, 255.0) as u8)
}

/// Whether a token is a web address rather than a word.
///
/// A dot **before** the first slash is what tells `coolors.co/10121c-2c1e31`
/// from `C:/Users/somebody/palette.gpl`, which has its dot the other side and
/// must not be unwrapped.
fn looks_like_url(token: &str) -> bool {
    if token.contains("://") {
        return true;
    }
    match token.find('/') {
        Some(slash) => token[..slash].contains('.'),
        None => false,
    }
}

/// A web address reduced to the part that might be a palette: its last path
/// segment, with any query or fragment taken off.
///
/// Coolors puts the palette in the path — `coolors.co/10121c-2c1e31-6b2643` —
/// and pasting that link is, by every account of how these are shared, the
/// commonest way one palette reaches another person.
///
/// A segment that is **entirely** hex digits and a whole number of colours long
/// is split into six-digit groups, because some sites run them together with no
/// separator at all. That reading is confined to a URL path segment on purpose:
/// nothing else puts a twenty-four-character hex string there, where in
/// ordinary text it would be a rule that turns any long identifier into four
/// colours.
fn unwrap_url(token: &str) -> Option<Cow<'_, str>> {
    if !looks_like_url(token) {
        return None;
    }
    let cut = token.find(['?', '#']).unwrap_or(token.len());
    let path = &token[..cut];
    let segment = path.rsplit('/').find(|piece| !piece.is_empty())?;
    let hex = segment.len() >= 12
        && segment.len() % 6 == 0
        && segment.bytes().all(|b| b.is_ascii_hexdigit());
    if hex {
        let groups: Vec<&str> = (0..segment.len() / 6)
            .map(|group| &segment[group * 6..group * 6 + 6])
            .collect();
        return Some(Cow::Owned(groups.join("-")));
    }
    Some(Cow::Borrowed(segment))
}

/// What is left of a line once its one colour is taken out, as a name.
///
/// `None` where nothing is left, where it is long enough to be prose rather
/// than a name, or where it holds more than one statement — a `{`, a `}` or an
/// inner `;` says the line was a block of CSS and not a labelled colour, and
/// "body { color: ; background: red }" is not a name anybody wrote.
fn leftover_name(line: &str, found: &Found) -> Option<String> {
    let mut rest = String::with_capacity(line.len());
    rest.push_str(&line[..found.start]);
    rest.push(' ');
    rest.push_str(&line[found.end..]);
    let trimmed = rest.trim().trim_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | ';' | ',' | '=' | '"' | '\'' | '-' | '_' | '*')
    });
    if trimmed.is_empty()
        || trimmed.chars().count() > NAME_LIMIT
        || trimmed.contains(['{', '}', ';'])
    {
        return None;
    }
    // Through the file writer's own rule, so what the panel shows is what a
    // save and a reopen give back. `Swatch::name`'s standing rule.
    let name = crate::palette::clean_line(trimmed);
    (!name.is_empty()).then_some(name)
}

/// A linear colour as a swatch, for the readers that arrive with one.
///
/// Through [`Swatch::of`] and therefore through the one `to_srgb_u8`, never a
/// second `powf` — the rule the whole module answers to.
pub(crate) fn from_linear(r: f32, g: f32, b: f32) -> Swatch {
    Swatch::of(Color::new(
        r.clamp(0.0, 1.0),
        g.clamp(0.0, 1.0),
        b.clamp(0.0, 1.0),
        1.0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn colours(text: &str) -> Vec<Swatch> {
        parse(text, "test").expect("readable").0
    }

    fn rgbs(text: &str) -> Vec<[u8; 3]> {
        colours(text).into_iter().map(|s| s.rgb).collect()
    }

    /// Every spelling of a hex code somebody might paste, in one place, because
    /// this is the parser the whole feature rests on.
    #[test]
    fn every_spelling_of_a_hex_code_reads() {
        assert_eq!(rgbs("#CC7722"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("#cc7722"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("cc7722"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("0xCC7722"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("0XCC7722"), vec![[0xCC, 0x77, 0x22]]);
        // The short form doubles each digit, which is CSS's rule and is what
        // makes `#fff` white rather than a very dark grey.
        assert_eq!(rgbs("#C72"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("#fff"), vec![[255, 255, 255]]);
        assert_eq!(rgbs("#000"), vec![[0, 0, 0]]);
        assert_eq!(rgbs("#C72F"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("rgb(204, 119, 34)"), vec![[204, 119, 34]]);
        assert_eq!(rgbs("rgb(204 119 34)"), vec![[204, 119, 34]]);
        assert_eq!(rgbs("RGBA(204, 119, 34, 0.5)"), vec![[204, 119, 34]]);
        assert_eq!(rgbs("rgb(204 119 34 / 50%)"), vec![[204, 119, 34]]);
        assert_eq!(rgbs("rgb(0%, 50%, 100%)"), vec![[0, 128, 255]]);
    }

    /// The short form is prefixed only, and this is the case that makes the
    /// whole bare rule safe: a bare `abc` is a word.
    #[test]
    fn a_bare_short_form_is_not_a_colour() {
        assert!(rgbs("abc").is_empty());
        assert!(rgbs("f0f").is_empty());
        assert!(rgbs("abcd").is_empty());
        // Five and seven digits are not a colour in any spelling.
        assert!(rgbs("#abcde").is_empty());
        assert!(rgbs("#abcdefa").is_empty());
        assert!(rgbs("abcdefa").is_empty());
    }

    /// **The rule the whole parser rests on.** A run of hex digits in prose is
    /// a word far more often than a colour: `facade`, `beefed`, `accede`,
    /// `deface` and `deadbeef` are all made only of hex digits. Reading one as
    /// a colour puts a colour nobody chose into somebody's palette, which is
    /// the silently-wrong import the standing rule refuses.
    #[test]
    fn a_word_made_of_hex_digits_is_not_a_colour_in_prose() {
        for prose in [
            "The facade was repainted",
            "beefed up the accede deface",
            "deadbeef is a well known word",
            "commit deadbeef fixed it",
        ] {
            assert!(rgbs(prose).is_empty(), "{prose}");
        }
        // But the same word alone on a line, which is what a `.hex` file is,
        // reads as the colour it plainly is.
        assert_eq!(rgbs("facade"), vec![[0xfa, 0xca, 0xde]]);
        // And a prefixed code in prose is trusted, because the `#` is the
        // statement that it is a colour.
        assert_eq!(rgbs("The facade was #CC7722"), vec![[0xCC, 0x77, 0x22]]);
    }

    /// Lospec's `.hex` download: bare six-digit codes, one per line, no `#`.
    /// The whole of that file goes through this parser.
    #[test]
    fn a_lospec_hex_file_reads_whole() {
        let text = "10121c\n2c1e31\n6b2643\nac2847\nec273f\n";
        assert_eq!(
            rgbs(text),
            vec![
                [0x10, 0x12, 0x1c],
                [0x2c, 0x1e, 0x31],
                [0x6b, 0x26, 0x43],
                [0xac, 0x28, 0x47],
                [0xec, 0x27, 0x3f],
            ]
        );
        // No names, so nothing invented one.
        assert!(colours(text).iter().all(|s| s.name.is_empty()));
    }

    /// Paint.NET's `.txt`: `;` comments and **`AARRGGBB`**, alpha first. The
    /// eight-digit rule is what makes this and CSS both read correctly, and it
    /// is the one place a prefix changes the answer rather than only being
    /// permission.
    #[test]
    fn eight_digits_are_read_by_whether_they_were_prefixed() {
        // Paint.NET: bare, alpha first. Reading these as RRGGBBAA would make
        // every one of them a bright red.
        let paintnet = "; paint.net Palette File\n;\nFF10121C\nFF2C1E31\n";
        assert_eq!(rgbs(paintnet), vec![[0x10, 0x12, 0x1c], [0x2c, 0x1e, 0x31]]);
        // CSS: prefixed, alpha last. Reading these as AARRGGBB would make the
        // first a blue.
        assert_eq!(rgbs("#2c1e31ff"), vec![[0x2c, 0x1e, 0x31]]);
        assert_eq!(rgbs("0x2c1e31ff"), vec![[0x2c, 0x1e, 0x31]]);
    }

    /// Transparency is a loss and is counted — but only where there was some.
    /// A code ending `ff` lost nothing, and a sentence about it would be the
    /// notice nobody reads.
    #[test]
    fn only_a_transparency_that_was_really_there_is_reported() {
        let (_, opaque) = parse("#2c1e31ff\n#112233\nFF445566\n", "test").expect("read");
        assert_eq!(opaque.transparency, 0);
        assert!(!opaque.any(), "an opaque paste loses nothing at all");

        let (swatches, lost) =
            parse("#2c1e3180\nrgba(1, 2, 3, 0.5)\n#C72A\n", "test").expect("read");
        assert_eq!(lost.transparency, 3);
        assert_eq!(swatches.len(), 3);
        assert_eq!(lost.sentences().len(), 1);
        assert!(lost.sentences()[0].contains("3 colours were partly transparent"));
    }

    /// A Coolors link is how a palette actually travels between two people, and
    /// the palette is in the path.
    #[test]
    fn a_coolors_link_is_a_palette() {
        for link in [
            "https://coolors.co/10121c-2c1e31-6b2643",
            "coolors.co/10121c-2c1e31-6b2643",
            "https://coolors.co/palette/10121c-2c1e31-6b2643",
            "https://coolors.co/palette/10121c-2c1e31-6b2643?utm=share",
        ] {
            assert_eq!(
                rgbs(link),
                vec![[0x10, 0x12, 0x1c], [0x2c, 0x1e, 0x31], [0x6b, 0x26, 0x43]],
                "{link}"
            );
        }
        // Pasted into a sentence, which is how one arrives in a chat window.
        // The bare-code rule would throw the palette away with the words, so
        // the URL is taken out and read on its own.
        assert_eq!(
            rgbs("Look at https://coolors.co/10121c-2c1e31 nice one").len(),
            2
        );
        // A run with no separators is split only inside a URL segment, where
        // nothing else puts a twenty-four character hex string.
        assert_eq!(
            rgbs("https://colorhunt.co/palette/222831393e4652948979f2f2"),
            vec![
                [0x22, 0x28, 0x31],
                [0x39, 0x3e, 0x46],
                [0x52, 0x94, 0x89],
                [0x79, 0xf2, 0xf2]
            ]
        );
        // And the same run in ordinary text is not four colours, because there
        // it is an identifier.
        assert!(rgbs("222831393e4652948979f2f2").is_empty());
    }

    /// A link that is not a palette contributes nothing rather than guessing,
    /// and a Windows path is not a link at all — its dot is on the wrong side
    /// of the slash.
    #[test]
    fn something_that_is_not_a_palette_link_yields_nothing() {
        for link in [
            "https://coolors.co/generate",
            "https://example.com/",
            "https://example.com/some-page-name",
            "C:/Users/somebody/palette.gpl",
            "/home/somebody/palette.gpl",
        ] {
            assert!(rgbs(link).is_empty(), "{link}");
        }
    }

    /// The block of CSS variables a generator hands back, with the names on it.
    #[test]
    fn a_css_dump_arrives_with_its_names() {
        let text = "--eerie-black: #10121cff;\n--dark-purple: #2c1e31ff;\n";
        let swatches = colours(text);
        assert_eq!(swatches.len(), 2);
        assert_eq!(swatches[0].rgb, [0x10, 0x12, 0x1c]);
        assert_eq!(swatches[0].name, "eerie-black");
        assert_eq!(swatches[1].name, "dark-purple");

        // A name beside a colour, either way round.
        assert_eq!(colours("#CC7722 Ochre")[0].name, "Ochre");
        assert_eq!(colours("Ochre: #CC7722")[0].name, "Ochre");
        assert_eq!(colours("$ochre: #CC7722;")[0].name, "$ochre");
    }

    /// A name is what somebody wrote beside a colour, not whatever else was on
    /// the line. Prose, a whole CSS rule and a line with two colours on it all
    /// arrive unnamed rather than arriving wrong.
    #[test]
    fn a_name_is_refused_where_the_line_is_not_a_labelled_colour() {
        // More than one statement: a `{` says this was a block.
        assert_eq!(colours("body { color: #fff; background: red }")[0].name, "");
        // Two colours have no way to divide one piece of text between them.
        assert!(
            colours("Warm pair #CC7722 #10121C")
                .iter()
                .all(|s| s.name.is_empty())
        );
        // Prose past the limit is prose, not a name.
        let long = format!("{} #CC7722", "word ".repeat(30));
        assert_eq!(colours(&long)[0].name, "");
        // And a name goes through the writer's own cleaning rule, so what is
        // held is exactly what a save and a reopen give back.
        assert_eq!(colours("#CC7722 \u{7}")[0].name, "");
    }

    /// A `.hex` file is bare codes; a `.txt` has comments; a chat message has
    /// commas. All three are the same paste.
    #[test]
    fn separators_and_comments_do_not_change_the_answer() {
        let expected = vec![[0x10, 0x12, 0x1c], [0x2c, 0x1e, 0x31]];
        for text in [
            "#10121c, #2c1e31",
            "#10121c\n#2c1e31",
            "#10121c #2c1e31",
            "10121c,2c1e31",
            "10121c | 2c1e31",
            "; a comment\n10121c\n2c1e31",
            "// a comment\n#10121c\n#2c1e31",
            "\u{feff}10121c\n2c1e31\n",
            "10121c\r\n2c1e31\r\n",
        ] {
            assert_eq!(rgbs(text), expected, "{text:?}");
        }
    }

    /// Text is bounded, because a paste is whatever was in the clipboard, and
    /// a palette is bounded, because the file it becomes has to be readable
    /// back. Neither is a truncation: both say so.
    #[test]
    fn a_paste_is_bounded_at_both_ends() {
        let huge = "a".repeat(MAX_FILE_BYTES as usize + 1);
        assert!(matches!(
            parse(&huge, "the pasted text"),
            Err(PaletteError::TooLarge { .. })
        ));

        let mut many = String::new();
        for n in 0..=crate::palette::MAX_SWATCHES {
            many.push_str(&format!("#{:06x}\n", n % 0xffffff));
        }
        let error = parse(&many, "the pasted text").expect_err("refused");
        assert!(matches!(error, PaletteError::TooManySwatches { .. }));
        // The phrase, not a path somebody would go looking for.
        assert!(error.to_string().starts_with("the pasted text holds"));
    }

    /// Nothing in a paste is not an error here — `palimport::read` is what
    /// decides that a file holding no colours is a refusal, so this stays a
    /// plain answer and the paste's own control can say "no colours found".
    #[test]
    fn a_paste_with_no_colours_in_it_is_an_answer_rather_than_an_error() {
        for text in ["", "   \n\n", "just some words", "; only a comment"] {
            assert!(colours(text).is_empty(), "{text:?}");
        }
    }

    /// Malformed in every way a person can be, and the parser answers rather
    /// than panicking. Slicing a line at a byte that is not a character
    /// boundary is the failure this is really about.
    #[test]
    fn awkward_text_answers_rather_than_panicking() {
        for text in [
            "#",
            "##",
            "#g",
            "0x",
            "0xzz",
            "rgb(",
            "rgb()",
            "rgb(1)",
            "rgb(1,2,3,4,5)",
            "rgb(nan, inf, 3)",
            "rgba(1,2,3,)",
            ")rgb(1,2,3",
            "#\u{e9}\u{e9}\u{e9}",
            "ochre \u{2014} #CC7722",
            "\u{1f3a8}#CC7722\u{1f3a8}",
            "#CC7722#10121C",
            "0x0x0x",
        ] {
            let _ = parse(text, "test").expect("no bound was reached");
        }
        // The two that do carry a colour still find it.
        assert_eq!(rgbs("ochre \u{2014} #CC7722"), vec![[0xCC, 0x77, 0x22]]);
        assert_eq!(rgbs("\u{1f3a8}#CC7722\u{1f3a8}"), vec![[0xCC, 0x77, 0x22]]);
    }

    /// Every byte, through the parser and back, so the tolerant path cannot
    /// move a level. It goes nowhere near a colour space; this is the guard
    /// that says so.
    #[test]
    fn a_pasted_code_is_the_exact_bytes_it_was_written_as() {
        for byte in 0..=255u8 {
            let rgb = [byte, 255 - byte, byte / 2];
            let text = format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]);
            assert_eq!(rgbs(&text), vec![rgb], "{text}");
            assert_eq!(rgbs(&text.to_uppercase()), vec![rgb], "{text}");
        }
    }
}
