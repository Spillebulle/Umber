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
//! **The leading count is the number of cells; `rank0:` is not.** They usually
//! agree and are not required to: `wood1b.gih` in rubberduck's pack says four
//! cells and `rank0:6`, and holds four. The count is what the file is walked
//! with, so a reader that sized itself off the rank would run off the end of
//! that file.
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
//! needs the dab pass to hold an array of tips and an index per instance.
//! `docs/brush-pipes.md` is the design and the measurements, and the headline
//! is that the *memory* is not what stands in the way — the widest pipe in
//! every pack Umber fetches is 9 cells of 300², which is 810 kB beside a
//! canvas-sized scratch. What stands in the way is that a tip is bound once
//! per pass and named by a single [`crate::BrushPreset::tip`].
//!
//! # A rule per dimension, not one rule
//!
//! A pipe has `dim` dimensions, each with a `rankN:` and a `selN:`, and the
//! index into the cells is walked separately along each. Every pipe in every
//! pack Umber fetches is `dim:1`, so reading `sel0:` alone is right for all 55
//! of them — and it is not right in general, which matters because the two
//! things a pipe can lose are different sentences. A `dim:2` pipe that turns
//! with the stroke along one axis and shuffles along the other loses both, and
//! naming only the first sends somebody looking for a rotating stamp when half
//! the mark is a shuffle.
//!
//! [`Selection`] is one dimension's rule and [`GihPipe::rules`] is all of them.
//! A dimension the file says nothing about is taken as **walking** its cells,
//! because the two readings fail in opposite directions and only one of them
//! fails safely: naming a loss that may not be there costs a sentence, where
//! assuming a rule that is not stated loses stamps in silence.
//!
//! # Two collapses that are exact, and one that is not
//!
//! A pipe becomes one brush, losing nothing, in exactly two cases, and both are
//! decided from the file rather than from a judgement about what the stamps
//! depict. They are in [`from_gih`] and they are why `animated` is not simply
//! `count > 1`.
//!
//! **Nothing walks.** `constant` names a dimension whose index never leaves
//! where it started, so a pipe whose every dimension is constant paints its
//! first cell for ever and the others are cells GIMP would never reach. Read
//! off the header. `count > 1` on its own got both halves wrong at once for
//! such a file: a five-cell constant pipe arrived as five brushes, four of
//! which GIMP would never paint, and each of the five claimed a sequencing
//! loss that had not happened.
//!
//! **The cells cannot differ.** Where every cell is the same brush, choosing
//! between them — at random, by direction, by anything — makes exactly the mark
//! one of them repeated makes. Compared byte for byte, so there is no threshold
//! to get wrong; and over everything the import reads off a cell rather than
//! over its coverage alone, so a `.gpb` whose colour differs is not "the same".
//! **Two of the 55 pipes in the fetched packs are this**, both tips inside
//! David Revoy's Krita bundle, and they are the only brushes anywhere in the
//! packs that this module's own work moves into the shipped library. A one-cell
//! pipe is the degenerate case of the same rule: a sequence of one has no
//! sequencing, so `ncells:1 sel0:incremental` used to name a loss that had not
//! happened.
//!
//! The third case is [`Selection::Angular`], and it is *not* exact — see below.
//! The difference is the whole reason two of these are built and one is not:
//! these two ask whether the other cells can ever be reached or ever differ,
//! which the file answers, where that one asks whether they are rotations of
//! each other, which only a resampler and a threshold could answer.
//!
//! # What is dropped
//!
//! - **The sequence** — `rank` and `sel` describe how GIMP walks the cells, and
//!   Umber has one cell in hand at a time. Reported per rule in force, so a
//!   pipe that both turns and shuffles says both.
//! - **Colour**, exactly as [`super::gbr`] drops it: a cell may be a 4-byte
//!   RGBA stamp or a `.gpb`, and only its coverage survives.
//!
//! `placement` is **not** in that list, and that is deliberate rather than an
//! omission: it says where a dab lands relative to the stroke, which is
//! [`crate::Brush::scatter`]'s question and not the cell array's. Naming it
//! beside the sequence would be a second sentence about the same loss.
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
//! is written against. Deciding it from the pixels — rotating cell 0 by
//! `i × 360/n` and comparing — needs a resampler this crate deliberately does
//! not have and a similarity threshold nobody could calibrate, because
//! **not one pipe in any pack Umber fetches is angular**: 44 loose files and 11
//! inside the two Krita bundles, every one of them `random` or `incremental`.
//! That is the whole argument in one figure, and `examples/measure-pipes.rs` is
//! what re-checks it. Until an angular file exists to check against, such a
//! pipe arrives as one preset per cell like any other and says precisely which
//! rule was lost, rather than the general "sequence" sentence.

use crate::preset::PresetError;

use super::gbr::{self, GbrBrush};

/// Refuse a pipe claiming more cells than any real brush has.
///
/// The number is read from a text header before any of the cells exist, so it
/// sizes an allocation on a stranger's say-so. GIMP's own animated brushes top
/// out in the low tens; the widest in every pack Umber fetches is **9**, and
/// the widest anywhere it has looked is 24.
const MAX_CELLS: usize = 1024;

/// Refuse a pipe claiming more dimensions than any real brush has.
///
/// Same reasoning as [`MAX_CELLS`] and a much tighter number, because `dim`
/// sizes the rule table off a stranger's word and GIMP's own editor offers
/// four. Every pipe in every fetched pack is `dim:1`.
const MAX_DIMENSIONS: usize = 8;

/// The longest header line this will read, matching GIMP's own limit.
const MAX_LINE: usize = 1024;

/// How a pipe walks the index along one of its dimensions.
///
/// GIMP's eight, and the division that matters is not eight ways: it is
/// [`Constant`](Self::Constant), which never leaves the cell it starts on and
/// is therefore the one rule a pipe can be collapsed under losing nothing;
/// [`Angular`](Self::Angular), which is a *rotating* stamp and is the one
/// Umber's own dab could reproduce; and the other six, which are all "a
/// different cell per dab, chosen by something" and are one loss with one
/// sentence. See the module docs for both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Selection {
    /// The index never moves: the pipe paints its first cell for ever.
    Constant,
    /// The next cell per dab, wrapping.
    Incremental,
    /// A cell per dab, uniformly at random. What every pipe in the fetched
    /// packs uses, bar four that are incremental.
    Random,
    /// The cell chosen by the **direction of the stroke** — one stamp drawn at
    /// `rank` rotations. Named apart everywhere in this module.
    Angular,
    /// By how fast the stroke is moving.
    Velocity,
    /// By pen pressure.
    Pressure,
    /// By pen tilt, on each axis.
    XTilt,
    YTilt,
    /// A word this build has never heard of.
    ///
    /// Taken as walking the cells, like every other unknown here: the reading
    /// that costs a sentence beats the reading that loses stamps in silence.
    Unknown,
}

impl Selection {
    fn parse(word: &str) -> Self {
        match word {
            "constant" => Self::Constant,
            "incremental" => Self::Incremental,
            "random" => Self::Random,
            "angular" => Self::Angular,
            "velocity" => Self::Velocity,
            "pressure" => Self::Pressure,
            "xtilt" => Self::XTilt,
            "ytilt" => Self::YTilt,
            _ => Self::Unknown,
        }
    }

    /// Whether this rule ever leaves the cell it starts on.
    pub fn walks(self) -> bool {
        self != Self::Constant
    }
}

/// A decoded brush pipe.
#[derive(Clone, Debug)]
pub struct GihPipe {
    /// The pipe's own name, from line one. Cells rarely name themselves.
    pub name: String,
    /// Every cell the pipe can actually reach, in file order.
    ///
    /// Usually every cell in the file. It is one cell where the file's own
    /// header or its own pixels say the others can never make a different
    /// mark — see the two collapses in [`from_gih`]. The whole file is walked
    /// and validated first either way: a truncated pipe is an error whichever
    /// rule it states.
    pub cells: Vec<GbrBrush>,
    /// How many cells the file held, before either collapse.
    ///
    /// `written > cells.len()` is the whole of "this pipe was collapsed", and
    /// it is what `examples/measure-pipes.rs` counts. Kept because the two are
    /// different questions: the cell count is what a pipe *is*, and the reach
    /// is what it can paint.
    pub written: usize,
    /// The rule for each of the pipe's `dim` dimensions, in order.
    ///
    /// `None` where the file states no `selN:` for that dimension, which is
    /// **not** the same as constant and must not be read as it — see the module
    /// docs. A dimension with no rule is taken as walking its cells.
    pub rules: Vec<Option<Selection>>,
    /// True when the pipe walks its cells by anything other than "always the
    /// first" — which every pipe in the wild does, and which is the thing
    /// Umber cannot reproduce.
    pub animated: bool,
    /// True when some dimension walks them by the **direction of the stroke**,
    /// which is a rotating stamp rather than a shuffled one. Reported
    /// separately because it is a different loss: see the module docs.
    pub angular: bool,
}

impl GihPipe {
    /// What Umber cannot reproduce about the way this pipe walks its cells.
    ///
    /// One list rather than a rule restated at each of the three places that
    /// reads a pipe — [`dropped_features`], `brushimport::read_file` and
    /// `kpp`'s tip decoder — because a pipe that both turns and shuffles has
    /// **two** losses, and the `if angular … else if animated` each of them
    /// used to spell separately could only ever name one of them.
    ///
    /// Colour is not here: it belongs to the *cell*, and a container reports it
    /// per brush where this is per file. [`dropped_features`] adds it.
    pub fn sequence_losses(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.angular {
            out.push(ANGULAR);
        }
        // Everything that is not the rotating stamp shares the general
        // sentence, because from Umber's side they are one thing: a cell per
        // dab, chosen by something, and no cell array to choose from.
        if self.shuffles() {
            out.push(ANIMATION);
        }
        out
    }

    /// Whether anything other than a rotating stamp walks the cells.
    fn shuffles(&self) -> bool {
        self.animated
            && self
                .rules
                .iter()
                .any(|rule| rule.is_none_or(|r| r.walks() && r != Selection::Angular))
    }
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

    // Everything after the count is `key:value`. A pipe has `dim` dimensions
    // and a `selN:` rule for each, and reading only `sel0:` would under-report
    // a two-dimensional one — see the module docs. `dim` is read before the
    // rules are placed, so a `sel3:` on a `dim:1` pipe is ignored rather than
    // inventing a dimension the file does not have.
    let mut dimensions = 1usize;
    let mut stated: Vec<(usize, Selection)> = Vec::new();
    for word in words {
        if let Some(value) = word.strip_prefix("dim:") {
            if let Ok(n) = value.parse::<usize>() {
                dimensions = n.clamp(1, MAX_DIMENSIONS);
            }
        } else if let Some(rest) = word.strip_prefix("sel")
            && let Some((index, value)) = rest.split_once(':')
            && let Ok(index) = index.parse::<usize>()
        {
            stated.push((index, Selection::parse(value)));
        }
    }
    let mut rules = vec![None; dimensions];
    for (index, rule) in stated {
        if let Some(slot) = rules.get_mut(index) {
            // Last wins, which is what the old single-rule read did.
            *slot = Some(rule);
        }
    }

    // A dimension the file says nothing about is taken as walking its cells,
    // which for a single-cell pipe is a walk of length one and therefore no
    // loss at all. `constant` is the only rule that pins the index, so it is
    // the only one a pipe can be trimmed under — see the module docs for why
    // that has to be *stated* rather than inferred from the cell count.
    let animated = rules
        .iter()
        .any(|rule| rule.is_none_or(Selection::walks) && (rule.is_some() || count > 1));
    let angular = rules.contains(&Some(Selection::Angular));

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

    // Every cell is read before this, whatever the rules say, so a truncated
    // file is still an error naming the cell it stopped at rather than a pipe
    // that quietly claims to hold one.
    //
    // Two collapses, and both are *exact* — which is what separates them from
    // the angular one the module docs decline. Neither has a threshold in it
    // and neither looks at what the stamps depict:
    //
    // - **Nothing walks.** Every dimension states `constant`, so the index sits
    //   where it started and the rest of the file is cells GIMP would never
    //   reach. Read off the header.
    // - **The cells cannot differ.** Every one of them is the same brush, so
    //   choosing between them — by chance, by direction, by anything — makes
    //   exactly the mark one of them repeated makes. Compared byte for byte
    //   over everything the import reads off a cell, so a `.gpb` whose colour
    //   differs is not "the same" and neither is a cell at another spacing.
    //   Two of the 55 pipes in the fetched packs are this, both of them tips
    //   inside David Revoy's Krita bundle. A one-cell pipe is the degenerate
    //   case and falls out of the same line: a sequence of one has no
    //   sequencing, so `sel0:incremental` on it names a loss that did not
    //   happen.
    let written = cells.len();
    let uniform = cells.iter().all(|cell| same_brush(cell, &cells[0]));
    if !animated || uniform {
        cells.truncate(1);
    }
    let collapsed = cells.len() < written || uniform;

    Ok(GihPipe {
        name,
        cells,
        written,
        rules,
        // Nothing was lost where the pipe collapsed, so nothing may be
        // reported. Claiming a loss that did not happen is the failure the
        // whole of `dropped_features` exists to avoid — a list that cries wolf
        // costs the losses that matter.
        animated: animated && !collapsed,
        angular: angular && !collapsed,
    })
}

/// Whether two cells are the same brush in everything the import keeps.
///
/// Not `==` on the whole [`GbrBrush`]: the **name** is a label rather than part
/// of the mark, and cells routinely carry different ones. Everything else is
/// compared, and comparing [`crate::tip::TipMask`] whole rather than its
/// coverage bytes is deliberate — it is what makes this stay right on the day a
/// mask carries more than coverage.
fn same_brush(a: &GbrBrush, b: &GbrBrush) -> bool {
    a.tip == b.tip && a.spacing == b.spacing && a.coloured == b.coloured
}

/// What reading this `.gih` will throw away.
///
/// Best-effort in the same way as [`super::gbr::dropped_features`]: a file that
/// will not parse says nothing here and fails properly in [`from_gih`].
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    let Ok(pipe) = from_gih(bytes) else {
        return Vec::new();
    };
    let mut out = pipe.sequence_losses();
    if pipe.cells.iter().any(|cell| cell.coloured) {
        out.push(gbr::COLOURED);
    }
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
        assert_eq!(
            dropped_features(&file),
            ["animated brush sequences", "coloured stamps"]
        );
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

        // …and so is one cell walked by an incremental rule, which used to be
        // reported as a loss on the reasoning that a pipe is taken at its word.
        // It is not a loss: a sequence of one has no sequencing, GIMP paints
        // that cell every dab and so does Umber. Naming it is the same claim
        // about a file that had not lost anything that `count > 1` made about a
        // constant pipe, and a list that cries wolf costs the losses that
        // matter.
        let walked = gih("Single", "ncells:1 sel0:incremental", &[cell(1)]);
        assert!(!from_gih(&walked).expect("decode").animated);
        assert!(dropped_features(&walked).is_empty());
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

        // A one-cell angular pipe has nothing to turn *through*, so there is
        // nothing to report. Same rule as `incremental` on one cell, and the
        // same reason: the loss is the sequencing, and a sequence of one has
        // none.
        let single = gih("One", "ncells:1 sel0:angular", &[cell(1)]);
        assert!(!from_gih(&single).expect("decode").angular);
        assert!(dropped_features(&single).is_empty());
    }

    /// The second exact collapse, and the only one that moves a real brush:
    /// where every cell is the same brush, choosing between them makes exactly
    /// the mark one of them repeated makes. Two of David Revoy's bundled tips
    /// are this — four copies of one stamp under `sel0:random`.
    #[test]
    fn a_pipe_whose_cells_are_all_the_same_brush_is_one_stamp_and_loses_nothing() {
        let same = gih(
            "Copies",
            "ncells:4 dim:1 rank0:4 sel0:random",
            &[cell(7), cell(7), cell(7), cell(7)],
        );
        let pipe = from_gih(&same).expect("decode");
        assert_eq!(pipe.written, 4, "the file still held four");
        assert_eq!(pipe.cells.len(), 1);
        assert!(!pipe.animated);
        assert!(dropped_features(&same).is_empty());

        // One cell out of four differing is a pipe that genuinely shuffles.
        let nearly = gih(
            "Nearly",
            "ncells:4 sel0:random",
            &[cell(7), cell(7), cell(8), cell(7)],
        );
        let pipe = from_gih(&nearly).expect("decode");
        assert_eq!(pipe.cells.len(), 4);
        assert_eq!(dropped_features(&nearly), [ANIMATION]);

        // Sameness is over everything the import reads off a cell, not over the
        // coverage alone. These two masks are identical and one of them is a
        // `.gpb` carrying a colour the other does not have, so they are not the
        // same stamp — and the colour is still reported, which it could not be
        // if the pipe had been collapsed onto the plain one.
        let mut pixmap = gbr(2, 2, 2, 1, "", &[7, 8, 9, 10]);
        pixmap.extend_from_slice(&pattern(2, 2));
        let mixed = gih("Mixed", "ncells:2 sel0:random", &[cell(7), pixmap]);
        let pipe = from_gih(&mixed).expect("decode");
        assert_eq!(pipe.cells.len(), 2);
        assert_eq!(dropped_features(&mixed), [ANIMATION, gbr::COLOURED]);
    }

    /// The one collapse a pipe admits that loses nothing: `constant` pins the
    /// index, so the cells after the first are ones GIMP would never reach.
    ///
    /// Reading `count > 1` as animation on its own got *both* halves of this
    /// wrong at once — five brushes where GIMP paints one, and a sequencing
    /// loss claimed on each of them that had not happened.
    #[test]
    fn a_pipe_that_never_leaves_its_first_cell_is_one_stamp_and_loses_nothing() {
        let pinned = gih(
            "Pinned",
            "ncells:3 dim:1 rank0:3 sel0:constant",
            &[cell(10), cell(20), cell(30)],
        );
        let pipe = from_gih(&pinned).expect("decode");
        assert!(!pipe.animated);
        assert_eq!(pipe.cells.len(), 1, "the other two are unreachable");
        assert_eq!(pipe.cells[0].tip.coverage(), [10, 11, 12, 13]);
        assert!(dropped_features(&pinned).is_empty());

        // The whole file is still walked before anything is trimmed, so a
        // constant pipe missing a cell is an error rather than a pipe that
        // quietly claims to hold one.
        let short = gih_claiming("Pinned", "ncells:3 sel0:constant", 3, &[cell(1), cell(2)]);
        assert!(from_gih(&short).is_err());

        // And every dimension has to be pinned, not just the first.
        let half = gih(
            "Half",
            "ncells:2 dim:2 rank0:1 rank1:2 sel0:constant sel1:random",
            &[cell(1), cell(2)],
        );
        let pipe = from_gih(&half).expect("decode");
        assert!(pipe.animated);
        assert_eq!(pipe.cells.len(), 2);
    }

    /// A dimension the file states no rule for is taken as **walking** its
    /// cells, because the two readings fail in opposite directions: naming a
    /// loss that may not be there costs a sentence, where assuming a rule
    /// nobody wrote down loses stamps in silence.
    #[test]
    fn a_dimension_with_no_stated_rule_is_taken_as_walking_its_cells() {
        let silent = gih("Silent", "ncells:3", &[cell(1), cell(2), cell(3)]);
        let pipe = from_gih(&silent).expect("decode");
        assert_eq!(pipe.rules, [None]);
        assert!(pipe.animated);
        assert_eq!(pipe.cells.len(), 3);
        assert_eq!(dropped_features(&silent), [ANIMATION]);

        // One cell walked by nothing in particular is a walk of length one,
        // which is a plain `.gbr` wearing a pipe's header.
        let alone = gih("Alone", "ncells:1", &[cell(1)]);
        assert!(!from_gih(&alone).expect("decode").animated);
        assert!(dropped_features(&alone).is_empty());

        // A word this build has never heard of walks, for the same reason.
        let strange = gih("Strange", "ncells:2 sel0:hyperbolic", &[cell(1), cell(2)]);
        let pipe = from_gih(&strange).expect("decode");
        assert_eq!(pipe.rules, [Some(Selection::Unknown)]);
        assert!(pipe.animated);
        assert_eq!(pipe.cells.len(), 2);
    }

    /// A pipe can lose two different things at once, and the `if angular …
    /// else if animated` this replaced could only ever name one of them —
    /// which is the sentence that would send somebody looking for a rotating
    /// stamp while half the mark was a shuffle.
    #[test]
    fn a_pipe_that_turns_and_shuffles_names_both_of_its_losses() {
        let both = gih(
            "Both",
            "ncells:6 dim:2 rank0:3 rank1:2 sel0:angular sel1:random",
            &[cell(1), cell(2), cell(3), cell(4), cell(5), cell(6)],
        );
        let pipe = from_gih(&both).expect("decode");
        assert_eq!(
            pipe.rules,
            [Some(Selection::Angular), Some(Selection::Random)]
        );
        assert_eq!(pipe.sequence_losses(), [ANGULAR, ANIMATION]);
        assert_eq!(dropped_features(&both), [ANGULAR, ANIMATION]);

        // Angular on its own still says only the specific thing.
        let turning = gih("Turn", "ncells:2 dim:1 sel0:angular", &[cell(1), cell(2)]);
        assert_eq!(
            from_gih(&turning).expect("decode").sequence_losses(),
            [ANGULAR]
        );
    }

    /// `dim` is read before the rules are placed, so a `selN:` naming a
    /// dimension the pipe does not have is ignored rather than inventing one —
    /// which would make a pinned pipe look as though something walked it.
    #[test]
    fn a_rule_for_a_dimension_the_pipe_does_not_have_is_ignored() {
        // `sel1:` on a one-dimensional pipe. The `dim:` word comes *after* it,
        // so this also pins that the two passes are ordered rather than
        // interleaved.
        let file = gih(
            "Stray",
            "ncells:2 sel1:random sel0:constant dim:1",
            &[cell(1), cell(2)],
        );
        let pipe = from_gih(&file).expect("decode");
        assert_eq!(pipe.rules, [Some(Selection::Constant)]);
        assert!(!pipe.animated);
        assert_eq!(pipe.cells.len(), 1);

        // A `dim` no allocation should be sized from is clamped, not trusted.
        let wide = gih("Wide", "ncells:1 dim:99999999", &[cell(1)]);
        assert_eq!(from_gih(&wide).expect("decode").rules.len(), MAX_DIMENSIONS);
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
