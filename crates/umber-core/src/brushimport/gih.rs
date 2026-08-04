//! GIMP `.gih` animated brushes — "brush pipes".
//!
//! A pipe is a *sequence* of `.gbr` stamps plus a rule for choosing between
//! them, and it is the format a large part of the free stamp collections is
//! actually distributed in: 43 of the 60 brushes in rubberduck's OpenGameArt
//! pack are `.gih`, not `.gbr`.
//!
//! The file is a painfully simple text header followed by whole `.gbr` files
//! back to back:
//!
//! ```text
//! Bark\n
//! 5 ncells:5 cellwidth:200 cellheight:500 step:100 dim:1 cols:5 rows:1 \
//!   placement:constant rank0:5 sel0:random\n
//! <gbr><gbr><gbr><gbr><gbr>
//! ```
//!
//! Line one is the pipe's name. Line two starts with the number of cells and
//! continues with `key:value` parameters. The cells carry their own dimensions
//! and spacing in their `.gbr` headers, so `cellwidth`, `cellheight` and `step`
//! are a summary rather than the truth — a pipe whose cells differ in size is
//! legal, and `rocky1.gih` is one.
//!
//! # One tip per stroke, so one preset per cell
//!
//! Umber binds a tip **per pass**, not per dab: a stroke has one brush, so one
//! tip covers the whole dab pass and a thousand tipped dabs stay a single draw
//! call (`docs/brushes.md`). A pipe therefore cannot arrive as one brush that
//! rotates through five stamps.
//!
//! Of the three ways out — one preset per cell, a cell chosen per dab, or one
//! representative cell and a note — this takes the first. It is the only one
//! that loses no *pixels*: every stamp the artist drew arrives and can be
//! painted with. What it loses is the **sequencing**, which
//! [`dropped_features`] says out loud, because a pipe that picks a random cell
//! per dab makes a visibly different mark from any one of its cells repeated.
//!
//! Choosing per dab is the better answer and is not this module's to give: it
//! needs the dab pass to hold an array of tips and an index per instance. The
//! shape of that change is written up in `docs/brushes.md`.
//!
//! # What is dropped
//!
//! - **The sequence**, as above — `placement`, `rank` and `sel` all describe
//!   how GIMP walks the cells, and Umber has one cell in hand at a time.
//! - **Colour**, exactly as [`super::gbr`] drops it: a cell may be a 4-byte
//!   RGBA stamp or a `.gpb`, and only its coverage survives.
//!
//! # `sel0:angular` is the stroke-following stamp, and it is named separately
//!
//! GIMP's selection modes are `constant`, `incremental`, `random`, `angular`,
//! `velocity`, `pressure`, `xtilt` and `ytilt`, and one of them is a different
//! kind of thing from the rest. **`angular` picks the cell by the direction of
//! the stroke**: the cells are one stamp drawn at `ncells` rotations, and
//! painting a curve turns the mark through them. That is a brush whose tip
//! follows the stroke, and Umber's dab does exactly that natively —
//! [`Brush::dab_angle_follows_stroke`](crate::Brush::dab_angle_follows_stroke)
//! turns the quad, tip and all, so *one* cell plus that flag would reproduce
//! the whole pipe rather than approximating it.
//!
//! It is deliberately not done, and the reason is that it cannot be checked.
//! Collapsing a pipe to its first cell is only right if the other cells really
//! are that cell rotated, which is what `angular` *means* but not what the file
//! *says*; a pipe whose cells are unrelated pictures walked angularly would
//! lose every stamp but one, silently, which is the failure this whole module
//! is written against. **No `.gih` in any pack Umber fetches is angular** — all
//! 43 in rubberduck's are `sel0:random` — so there is nothing to verify such a
//! collapse against. Until there is, an angular pipe arrives as one preset per
//! cell like any other and says precisely which rule was lost, rather than the
//! general "sequence" sentence.

use crate::preset::PresetError;

use super::gbr::{self, GbrBrush};

/// Refuse a pipe claiming more cells than any real brush has.
///
/// The number is read from a text header before any of the cells exist, so it
/// sizes an allocation on a stranger's say-so. GIMP's own animated brushes top
/// out in the low tens; the largest in the packs Umber has looked at is 24.
const MAX_CELLS: usize = 1024;

/// The longest header line this will read, matching GIMP's own limit.
const MAX_LINE: usize = 1024;

/// A decoded brush pipe.
#[derive(Clone, Debug)]
pub struct GihPipe {
    /// The pipe's own name, from line one. Cells rarely name themselves.
    pub name: String,
    /// Every cell, in file order.
    pub cells: Vec<GbrBrush>,
    /// True when the pipe walks its cells by anything other than "always the
    /// first" — which every pipe in the wild does, and which is the thing
    /// Umber cannot reproduce.
    pub animated: bool,
    /// True when it walks them by the **direction of the stroke**, which is a
    /// rotating stamp rather than a shuffled one. Reported separately because
    /// it is a different loss: see the module docs.
    pub angular: bool,
}

/// Decode a GIMP `.gih` file.
pub fn from_gih(bytes: &[u8]) -> Result<GihPipe, PresetError> {
    let (name, rest) = line(bytes, 0)?;
    let (parameters, mut at) = line(bytes, rest)?;

    let name = String::from_utf8_lossy(name).trim().to_string();
    let parameters = String::from_utf8_lossy(parameters);

    // The count is the first word; everything after it is `key:value` pairs.
    let mut words = parameters.split_ascii_whitespace();
    let count: usize = words
        .next()
        .and_then(|w| w.parse().ok())
        .ok_or_else(|| malformed("its second line does not begin with a cell count"))?;
    if count == 0 || count > MAX_CELLS {
        return Err(PresetError::Malformed(
            None,
            format!("a brush pipe of {count} cells is not plausible"),
        ));
    }

    // The rule for the first dimension. Written `sel0:` because a pipe may have
    // several — `dim:2` with `sel0:` and `sel1:` — and no pack in the wild uses
    // more than one.
    let selection = words.filter_map(|w| w.strip_prefix("sel0:")).next_back();

    // `sel0:constant` on a single-cell pipe is a plain `.gbr` wearing a pipe's
    // header, and nothing is lost converting it. Anything else walks the cells.
    let animated = count > 1 || selection.is_some_and(|mode| mode != "constant");
    // Read off the same word, and reported on the same terms as `incremental`
    // is: a pipe that says it walks its cells by direction is taken at its
    // word, whether or not it happens to have only one to walk.
    let angular = selection == Some("angular");

    let mut cells = Vec::with_capacity(count.min(64));
    for index in 0..count {
        let tail = bytes
            .get(at..)
            .ok_or_else(|| malformed("the file ends before its cells do"))?;
        let (cell, consumed) = gbr::read_one(tail).map_err(|e| match e {
            // The cell's own message names a byte offset inside the cell, which
            // is meaningless from outside. Say which cell instead.
            PresetError::Malformed(_, detail) => PresetError::Malformed(
                None,
                format!(
                    "cell {} of the brush pipe could not be read: {detail}",
                    index + 1
                ),
            ),
            other => other,
        })?;
        at += consumed;
        cells.push(cell);
    }

    Ok(GihPipe {
        name,
        cells,
        animated,
        angular,
    })
}

/// What reading this `.gih` will throw away.
///
/// Best-effort in the same way as [`super::gbr::dropped_features`]: a file that
/// will not parse says nothing here and fails properly in [`from_gih`].
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    let Ok(pipe) = from_gih(bytes) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if pipe.angular {
        out.push(ANGULAR);
    } else if pipe.animated {
        out.push(ANIMATION);
    }
    // A coloured cell used to be reported here. It is not a loss any more —
    // `gbr` carries the colour across — so the sequence is all a pipe drops.
    out
}

/// Named once so the pipe reader and the bundle reader report it identically.
pub(crate) const ANIMATION: &str = "animated brush sequences";

/// The one selection rule that is a rotating stamp rather than a shuffled one.
/// See the module docs for why it is named apart and why the pipe is not
/// collapsed into a single stroke-following brush.
pub(crate) const ANGULAR: &str = "stamps chosen by the direction of the stroke";

/// One line of the text header, and the offset just past its newline.
fn line(bytes: &[u8], from: usize) -> Result<(&[u8], usize), PresetError> {
    let rest = bytes
        .get(from..)
        .ok_or_else(|| malformed("the file ends inside its header"))?;
    let end = rest
        .iter()
        .take(MAX_LINE)
        .position(|&b| b == b'\n')
        .ok_or_else(|| malformed("its header has no line break in the first kilobyte"))?;
    Ok((&rest[..end], from + end + 1))
}

fn malformed(message: &str) -> PresetError {
    PresetError::Malformed(None, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brushimport::gbr::tests::{gbr, pattern};

    /// Build a `.gih` by hand out of `.gbr` fixtures, the same discipline the
    /// `.gbr` tests use: no vendored binary, so the byte layout is pinned by
    /// the test rather than by whatever happened to be on disk.
    fn gih(name: &str, parameters: &str, cells: &[Vec<u8>]) -> Vec<u8> {
        gih_claiming(name, parameters, cells.len(), cells)
    }

    /// The same, with a cell count that need not match what follows — which is
    /// what a truncated file looks like.
    fn gih_claiming(name: &str, parameters: &str, count: usize, cells: &[Vec<u8>]) -> Vec<u8> {
        let mut out = format!("{name}\n{count} {parameters}\n").into_bytes();
        for cell in cells {
            out.extend_from_slice(cell);
        }
        out
    }

    fn cell(seed: u8) -> Vec<u8> {
        gbr(2, 2, 2, 1, "", &[seed, seed + 1, seed + 2, seed + 3])
    }

    #[test]
    fn a_pipe_yields_every_cell_it_holds() {
        // The point of the whole module. Reading only the first cell would
        // throw away four fifths of rubberduck's pack.
        let file = gih(
            "Bark",
            "ncells:3 cellwidth:2 cellheight:2 step:100 dim:1 cols:3 rows:1 \
             placement:constant rank0:3 sel0:random",
            &[cell(10), cell(20), cell(30)],
        );
        let pipe = from_gih(&file).expect("decode");

        assert_eq!(pipe.name, "Bark");
        assert_eq!(pipe.cells.len(), 3);
        assert_eq!(pipe.cells[0].tip.coverage(), [10, 11, 12, 13]);
        assert_eq!(pipe.cells[1].tip.coverage(), [20, 21, 22, 23]);
        assert_eq!(pipe.cells[2].tip.coverage(), [30, 31, 32, 33]);
        // Each cell keeps the spacing its own `.gbr` header states.
        assert_eq!(pipe.cells[0].spacing, Some(0.25));
    }

    /// Cells are found by walking, so a cell whose length is misjudged makes
    /// rubbish of every one after it. A pipe of differently sized cells is the
    /// case that catches an assumed stride — and `rocky1.gih` is one.
    #[test]
    fn cells_of_different_sizes_are_all_found() {
        let file = gih(
            "Mixed",
            "ncells:3 sel0:random",
            &[
                gbr(2, 4, 1, 1, "", &[1, 2, 3, 4]),
                gbr(2, 2, 3, 1, "Named", &[5; 6]),
                gbr(1, 1, 1, 1, "", &[9]),
            ],
        );
        let pipe = from_gih(&file).expect("decode");
        assert_eq!(pipe.cells.len(), 3);
        assert_eq!(
            (pipe.cells[0].tip.width(), pipe.cells[0].tip.height()),
            (4, 1)
        );
        assert_eq!(
            (pipe.cells[1].tip.width(), pipe.cells[1].tip.height()),
            (2, 3)
        );
        assert_eq!(pipe.cells[1].name, "Named");
        // A version 1 cell, which has a shorter header and no spacing.
        assert_eq!(pipe.cells[2].tip.coverage(), [9]);
        assert_eq!(pipe.cells[2].spacing, None);
    }

    /// A pipe of `.gpb` cells is where the length arithmetic earns its keep:
    /// each cell carries a whole colour pattern behind its mask.
    #[test]
    fn cells_that_are_pixmap_brushes_do_not_swallow_the_next_cell() {
        let pixmap = |seed: u8| {
            let mut out = gbr(2, 2, 2, 1, "", &[seed, seed + 1, seed + 2, seed + 3]);
            out.extend_from_slice(&pattern(2, 2));
            out
        };
        let file = gih("Pixmaps", "ncells:2 sel0:random", &[pixmap(40), pixmap(50)]);
        let pipe = from_gih(&file).expect("decode");
        assert_eq!(pipe.cells.len(), 2);
        assert_eq!(pipe.cells[1].tip.coverage(), [50, 51, 52, 53]);
        // Each cell keeps its pattern's colour, so the sequence is the only
        // thing left for the pipe to report.
        assert!(pipe.cells[1].tip.is_coloured());
        assert_eq!(dropped_features(&file), ["animated brush sequences"]);
    }

    /// The sequence is the thing Umber cannot reproduce, and the import has to
    /// say so — a pipe that picks a cell at random per dab makes a visibly
    /// different mark from any one of its cells repeated.
    #[test]
    fn an_animated_pipe_says_its_sequence_was_dropped() {
        let file = gih("Bark", "ncells:2 sel0:random", &[cell(1), cell(2)]);
        assert_eq!(dropped_features(&file), ["animated brush sequences"]);

        // A one-cell pipe that always picks that cell is a `.gbr` in a pipe's
        // clothing, and nothing is lost converting it.
        let plain = gih("Single", "ncells:1 sel0:constant", &[cell(1)]);
        assert!(!from_gih(&plain).expect("decode").animated);
        assert!(dropped_features(&plain).is_empty());

        // …but one cell walked by an incremental rule still is not.
        let walked = gih("Single", "ncells:1 sel0:incremental", &[cell(1)]);
        assert!(from_gih(&walked).expect("decode").animated);
    }

    /// `angular` is the one selection rule that describes a *rotating* stamp
    /// rather than a shuffled one, and Umber's dab turns its tip natively — so
    /// what is lost here is a specific thing and the import says which. The
    /// general sentence would send somebody looking for a randomiser.
    #[test]
    fn a_pipe_that_turns_with_the_stroke_names_that_rather_than_the_sequence() {
        let turning = gih(
            "Rake",
            "ncells:4 dim:1 rank0:4 placement:constant sel0:angular",
            &[cell(1), cell(2), cell(3), cell(4)],
        );
        let pipe = from_gih(&turning).expect("decode");
        assert!(pipe.angular);
        assert!(pipe.animated, "it still walks its cells");
        assert_eq!(
            dropped_features(&turning),
            ["stamps chosen by the direction of the stroke"]
        );

        // A shuffled pipe is the ordinary case and keeps the ordinary sentence.
        let shuffled = gih("Bark", "ncells:2 sel0:random", &[cell(1), cell(2)]);
        assert!(!from_gih(&shuffled).expect("decode").angular);
        assert_eq!(dropped_features(&shuffled), ["animated brush sequences"]);

        // A pipe is taken at its word, on the same terms `incremental` is: a
        // one-cell angular pipe reports the rule it states rather than the
        // rule its cell count happens to make redundant.
        let single = gih("One", "ncells:1 sel0:angular", &[cell(1)]);
        assert!(from_gih(&single).expect("decode").angular);
    }

    #[test]
    fn a_truncated_pipe_is_an_error_that_names_the_cell() {
        let file = gih_claiming("Short", "ncells:3 sel0:random", 3, &[cell(1), cell(2)]);
        let err = from_gih(&file).expect_err("three cells were promised");
        assert!(err.to_string().contains("cell 3"), "{err}");
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_panic() {
        assert!(from_gih(b"").is_err());
        assert!(from_gih(b"no newline at all").is_err());
        assert!(from_gih(b"Name\nnot a number\n").is_err());
        assert!(from_gih(b"Name\n0 ncells:0\n").is_err());
        // A cell count no allocation should be sized from.
        assert!(from_gih(b"Name\n99999999 ncells:99999999\n").is_err());
        assert!(dropped_features(b"rubbish").is_empty());

        let full = gih("Bark", "ncells:2 sel0:random", &[cell(1), cell(2)]);
        for cut in 0..full.len() {
            // Any of these may fail; none may panic.
            let _ = from_gih(&full[..cut]);
        }
    }
}
