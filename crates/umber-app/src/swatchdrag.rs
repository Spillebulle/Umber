//! Dragging a colour to another place in a palette.
//!
//! The model only: what is being carried, which cell the pointer is over, and
//! what a release would actually do. Nothing here paints, which is what lets
//! the hit testing and the refusal be tested without a window — the same
//! division [`crate::dock`] keeps against [`crate::panels`],
//! [`crate::layerdrag`] against the layer list and [`crate::brushdrag`] against
//! the collection rail, and for the same reason. [`crate::palettelib`] supplies
//! this frame's pointer and the rectangles the cells landed in, and rings
//! whichever cell this names.
//!
//! It is [`crate::layerdrag`]'s shape with one axis more, and the differences
//! are the whole of what is worth reading here.
//!
//! # Two hit tests, for the reason the layer list has two
//!
//! A press must land **strictly inside** a cell where a drop rounds to the
//! nearest one. That rule exists in the layer list because the opacity slider
//! sits directly above the list in the same column, so a press that rounded
//! would turn dragging the slider into dragging the bottom layer.
//!
//! **The same hazard is here**, and it was worth checking rather than copying:
//! the Palette panel draws the palette picker — a full-width dropdown — eight
//! points above the grid, in the same column, and the naming field appears
//! directly below it. A press on either that rounded to the nearest cell would
//! pick up the first or the last colour, and the dropdown is a control somebody
//! uses far more often than the grid. So [`cell_pressed`] clamps to nothing and
//! [`cell_at`] clamps to the nearest.
//!
//! # A drop reaches one gap and no further, and a grid needs no "past the end"
//!
//! The layer list treats past-either-end as the end row, because a list has one
//! axis and up means "further along"; it clamps in `y` and refuses in `x`. A
//! grid has two axes and its order wraps, so there is no direction that means
//! "further along" on its own, and neither *clamp everywhere* nor *the
//! bounding box* is right:
//!
//! - Clamping everywhere would leave a drag with nowhere to be abandoned.
//! - The bounding box looks right and is not, which is worth writing down
//!   because it was written first and the test caught it. Eleven colours four
//!   across leave an empty cell at the end of the last row, *inside* the box —
//!   and that empty cell is exactly as far from the last colour as it is from
//!   the one directly above it in the previous row. Nearest-cell answered with
//!   whichever of the two the list happened to hold first, so "drop it at the
//!   end" put the colour three places from the end, silently, on a tie.
//!
//! So a drop reaches **one gap and no further**: the pointer must be inside a
//! colour, or within `reach` of one, and `reach` is the gap the grid was drawn
//! with — supplied by the caller for the reason [`crate::layerdrag`] takes its
//! indent from the panel, because it is what the cells were actually laid out
//! by. The rule is the same in all four directions, so there is one way to
//! abandon a drag rather than an edge that behaves differently from the others,
//! and the crossing where two gaps meet is comfortably inside it while the
//! empty tail of a short row is comfortably outside.
//!
//! Nothing becomes unreachable. Every position `0..len` is a cell's, and
//! `Palette::move_swatch` puts the colour at the index it names — so dropping
//! on the last colour *is* "put it at the end", and the ring round that colour
//! is what says so.
//!
//! # A drag carries the palette it came from
//!
//! A drag lives in egui's memory across frames and the palette in front can
//! change under it. An index is a position in one palette; the same number in
//! the next palette is a different colour, and a move applied to it would
//! rearrange a palette nobody was dragging. Carrying the id and refusing a
//! mismatch is the same rule [`crate::brushdrag`] carries a brush by id for.
//!
//! Whether a move is *legal* is not decided here. `Palette::can_move_swatch`
//! owns that and [`Drag::aim`] takes it as a predicate rather than restating
//! it — which is also what refuses the one drop that would do nothing, the
//! position the colour is already at.

use egui::{Pos2, Rect};

/// One colour of the grid: the position it draws, and where it landed.
///
/// Rectangles rather than an assumed cell size, because what a drop hits is a
/// question about geometry and the panel is the only thing that knows what was
/// actually laid out — including which cells were culled for being scrolled out
/// of view, which are neither drawn nor targets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Index into `Palette::swatches`.
    pub index: usize,
    pub rect: Rect,
}

/// A colour on its way to another position in its palette.
#[derive(Clone, Debug, PartialEq)]
pub struct Drag {
    /// The palette it belongs to, by id. See the module docs: an index means
    /// nothing without it.
    pub palette: String,
    /// Where the colour sits now. Positions do move under a drag — the whole
    /// point of it — but only at the drop, so this stays true for the gesture.
    pub from: usize,
    to: Option<usize>,
}

impl Drag {
    pub fn new(palette: impl Into<String>, from: usize) -> Self {
        Self {
            palette: palette.into(),
            from,
            to: None,
        }
    }

    /// Work out what a release now would do, and remember it.
    ///
    /// Returns the cell to ring, which is exactly the one named by
    /// [`Drag::destination`] — one answer, so the mark on the grid and the move
    /// that happens cannot disagree. `None` where a release would do nothing.
    ///
    /// `pointer` is an `Option` because a pointer that has left the window has
    /// no position, and a drag does not end just because it did.
    ///
    /// `reach` is how far outside a colour a drop still counts — the gap the
    /// grid was drawn with, which the panel owns. `allowed` is
    /// `Palette::can_move_swatch` with the source already bound; see the module
    /// docs for why the legality is asked rather than restated.
    pub fn aim(
        &mut self,
        cells: &[Cell],
        pointer: Option<Pos2>,
        reach: f32,
        allowed: impl Fn(usize) -> bool,
    ) -> Option<usize> {
        let landed = pointer
            .and_then(|pos| cell_at(cells, pos, reach))
            .filter(|to| allowed(*to));
        self.to = landed;
        landed
    }

    /// Where a release right now would move the colour.
    pub fn destination(&self) -> Option<usize> {
        self.to
    }
}

/// Which cell a pointer at `pointer` is actually on, with nothing clamped.
///
/// What a drag *begins* on, where [`cell_at`] is what it ends on. See the
/// module docs: the palette picker sits directly above the grid and a press
/// that rounded would turn using it into dragging a colour.
pub fn cell_pressed(cells: &[Cell], pointer: Pos2) -> Option<usize> {
    cells
        .iter()
        .find(|cell| cell.rect.contains(pointer))
        .map(|cell| cell.index)
}

/// Which cell a pointer at `pointer` is aiming at, or `None` for nowhere.
///
/// Containment first, which is every ordinary case and makes a pointer exactly
/// on a shared boundary belong to whichever cell the caller listed first.
/// Failing that, the nearest cell — but only while it is within `reach`. See
/// the module docs for why that is a distance rather than the grid's bounding
/// box.
///
/// The distance is the true two-dimensional one rather than either axis alone:
/// in the cross where a row gap meets a column gap, all four neighbours are
/// candidates and the diagonal is what tells them apart. Ties go to the lowest
/// index, because `min_by` keeps the first and the caller builds the list in
/// index order — deterministic rather than dependent on the arithmetic coming
/// out level.
pub fn cell_at(cells: &[Cell], pointer: Pos2, reach: f32) -> Option<usize> {
    if let Some(index) = cell_pressed(cells, pointer) {
        return Some(index);
    }
    cells
        .iter()
        .map(|cell| (cell.index, distance_to(cell.rect, pointer)))
        .filter(|(_, away)| *away <= reach.max(0.0))
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(index, _)| index)
}

/// How far a point is outside a rectangle. Zero inside it.
fn distance_to(rect: Rect, p: Pos2) -> f32 {
    let dx = (rect.left() - p.x).max(p.x - rect.right()).max(0.0);
    let dy = (rect.top() - p.y).max(p.y - rect.bottom()).max(0.0);
    dx.hypot(dy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    const SIZE: f32 = 26.0;
    const GAP: f32 = 4.0;
    const STEP: f32 = SIZE + GAP;
    const ORIGIN: Pos2 = pos2(10.0, 20.0);

    /// `count` colours in a grid `columns` wide, laid out exactly as
    /// `palettelib::swatch_rect` lays them out. The last row is deliberately
    /// short in most of these, because that is the case a grid has and a list
    /// does not.
    fn grid(count: usize, columns: usize) -> Vec<Cell> {
        (0..count)
            .map(|index| Cell {
                index,
                rect: Rect::from_min_size(
                    pos2(
                        ORIGIN.x + (index % columns) as f32 * STEP,
                        ORIGIN.y + (index / columns) as f32 * STEP,
                    ),
                    vec2(SIZE, SIZE),
                ),
            })
            .collect()
    }

    /// The middle of cell `index`.
    fn centre(index: usize, columns: usize) -> Pos2 {
        pos2(
            ORIGIN.x + (index % columns) as f32 * STEP + SIZE * 0.5,
            ORIGIN.y + (index / columns) as f32 * STEP + SIZE * 0.5,
        )
    }

    /// `Palette::can_move_swatch` with the source bound, over a palette of
    /// `len`. The real predicate is the model's, which is the point.
    fn moving(from: usize, len: usize) -> impl Fn(usize) -> bool {
        move |to| to < len && to != from
    }

    #[test]
    fn a_pointer_finds_the_cell_it_is_inside() {
        let cells = grid(11, 4);
        for index in 0..11 {
            assert_eq!(cell_at(&cells, centre(index, 4), GAP), Some(index), "{index}");
            assert_eq!(
                cell_pressed(&cells, centre(index, 4)),
                Some(index),
                "{index}"
            );
        }
    }

    /// The four pixels between two colours are not a hole the drop falls
    /// through, on either axis.
    #[test]
    fn a_pointer_in_a_gap_takes_the_nearer_cell() {
        let cells = grid(8, 4);
        // Horizontally between 0 and 1: a pixel either side of the midline.
        let mid_x = ORIGIN.x + SIZE + GAP * 0.5;
        let row_y = ORIGIN.y + SIZE * 0.5;
        assert_eq!(cell_at(&cells, pos2(mid_x - 1.0, row_y), GAP), Some(0));
        assert_eq!(cell_at(&cells, pos2(mid_x + 1.0, row_y), GAP), Some(1));
        // Vertically between 0 and 4.
        let mid_y = ORIGIN.y + SIZE + GAP * 0.5;
        let col_x = ORIGIN.x + SIZE * 0.5;
        assert_eq!(cell_at(&cells, pos2(col_x, mid_y - 1.0), GAP), Some(0));
        assert_eq!(cell_at(&cells, pos2(col_x, mid_y + 1.0), GAP), Some(4));
    }

    /// The cross where a row gap meets a column gap has four candidates at
    /// once. Either axis read alone would answer with whichever of the four the
    /// list happened to hold first; the diagonal distance is what makes the
    /// answer the colour the pointer is actually nearest to.
    ///
    /// The far corner of that cross is `GAP / sqrt(2)` from each of the four,
    /// which is inside one gap's reach — so the whole crossing is live and
    /// there is no pinhole in the middle of the grid where a drop does nothing.
    #[test]
    fn the_crossing_of_two_gaps_answers_with_the_nearest_of_the_four() {
        let cells = grid(8, 4);
        let cross = pos2(ORIGIN.x + SIZE + GAP * 0.5, ORIGIN.y + SIZE + GAP * 0.5);
        // Nudged towards each of the four corners meeting there: 0, 1, 4, 5.
        assert_eq!(cell_at(&cells, cross + vec2(-1.0, -1.0), GAP), Some(0));
        assert_eq!(cell_at(&cells, cross + vec2(1.0, -1.0), GAP), Some(1));
        assert_eq!(cell_at(&cells, cross + vec2(-1.0, 1.0), GAP), Some(4));
        assert_eq!(cell_at(&cells, cross + vec2(1.0, 1.0), GAP), Some(5));
        // Exactly on the crossing every distance is equal, and the answer is
        // the lowest index rather than whichever way the floats fell.
        assert_eq!(cell_at(&cells, cross, GAP), Some(0));
    }

    /// The case the bounding box got wrong, and the reason `reach` exists.
    ///
    /// Eleven colours four across leave an empty cell at the end of the last
    /// row. It is inside the grid's bounding box and it is *exactly* as far
    /// from the last colour as from the colour above it in the previous row —
    /// so nearest-cell answered on a tie, and "drop it at the end" put the
    /// colour three places from the end. It is now outside the reach of either,
    /// nothing lights up, and the way to say "at the end" is to drop on the
    /// last colour, which is the same position.
    #[test]
    fn the_empty_tail_of_a_short_last_row_is_over_nothing() {
        let cells = grid(11, 4);
        let tail = pos2(
            ORIGIN.x + 3.0 * STEP + SIZE * 0.5,
            ORIGIN.y + 2.0 * STEP + SIZE * 0.5,
        );
        assert_eq!(cell_at(&cells, tail, GAP), None);
        // Just beside the last colour, still within one gap of it, is the last
        // colour — which is what a hand aiming past the end actually does.
        let beside = pos2(
            ORIGIN.x + 2.0 * STEP + SIZE + GAP * 0.5,
            ORIGIN.y + 2.0 * STEP + SIZE * 0.5,
        );
        assert_eq!(cell_at(&cells, beside, GAP), Some(10));
        // And a full last row has no such run at all.
        let full = grid(12, 4);
        assert_eq!(cell_at(&full, centre(11, 4), GAP), Some(11));
    }

    /// Leaving the grid is how a drag is abandoned, and a grid has four ways
    /// out rather than the list's two. All of them have to mean the same thing,
    /// or one edge would drop the colour somewhere the pointer had left.
    #[test]
    fn a_pointer_outside_the_grid_is_over_nothing() {
        let cells = grid(11, 4);
        for away in [
            pos2(ORIGIN.x - GAP - 1.0, ORIGIN.y + 10.0),
            pos2(ORIGIN.x + 400.0, ORIGIN.y + 10.0),
            pos2(ORIGIN.x + 10.0, ORIGIN.y - GAP - 1.0),
            pos2(ORIGIN.x + 10.0, ORIGIN.y + 400.0),
        ] {
            assert_eq!(cell_at(&cells, away, GAP), None, "{away:?}");
        }
        // And a reach of nothing still finds a colour the pointer is inside, so
        // a caller that hands over a zero cannot make the grid dead.
        assert_eq!(cell_at(&cells, centre(3, 4), 0.0), Some(3));
        assert_eq!(cell_at(&cells, centre(3, 4), -5.0), Some(3));
    }

    #[test]
    fn an_empty_grid_is_over_nothing() {
        assert_eq!(cell_at(&[], pos2(10.0, 10.0), GAP), None);
        assert_eq!(cell_pressed(&[], pos2(10.0, 10.0)), None);
    }

    /// A press clamps to nothing. The palette picker sits directly above the
    /// grid, in the same column and at the same width, and a press on it must
    /// not pick up the colour nearest to it.
    #[test]
    fn a_press_is_only_ever_on_a_cell_it_is_inside() {
        let cells = grid(11, 4);
        assert_eq!(cell_pressed(&cells, centre(5, 4)), Some(5));
        assert_eq!(
            cell_pressed(&cells, pos2(ORIGIN.x + 10.0, ORIGIN.y - 8.0)),
            None,
            "the picker's own line, above the grid"
        );
        assert_eq!(
            cell_pressed(&cells, pos2(ORIGIN.x + 10.0, ORIGIN.y + 3.0 * STEP)),
            None,
            "the naming field's line, below the grid"
        );
        assert_eq!(
            cell_pressed(&cells, pos2(ORIGIN.x + SIZE + GAP * 0.5, ORIGIN.y + 10.0)),
            None,
            "in a gap"
        );
    }

    #[test]
    fn a_drop_on_another_cell_names_its_position() {
        let cells = grid(11, 4);
        let mut drag = Drag::new("ochres", 2);
        assert_eq!(
            drag.aim(&cells, Some(centre(7, 4)), GAP, moving(2, 11)),
            Some(7)
        );
        assert_eq!(drag.destination(), Some(7));
    }

    /// The one target that has to be refused: accepting it would ring a cell to
    /// promise a move that then does not happen. Decided by the model — this
    /// checks the answer is carried through and that a refusal *clears* the
    /// mark rather than leaving the last legal one standing.
    #[test]
    fn a_drop_where_the_colour_already_is_does_nothing() {
        let cells = grid(11, 4);
        let mut drag = Drag::new("ochres", 5);
        assert_eq!(
            drag.aim(&cells, Some(centre(5, 4)), GAP, moving(5, 11)),
            None
        );
        assert_eq!(drag.destination(), None);
        // Its neighbours are still perfectly good targets.
        assert_eq!(
            drag.aim(&cells, Some(centre(4, 4)), GAP, moving(5, 11)),
            Some(4)
        );
        assert_eq!(drag.destination(), Some(4));
        // And a model that refuses everything lights nothing up.
        assert_eq!(drag.aim(&cells, Some(centre(4, 4)), GAP, |_| false), None);
        assert_eq!(drag.destination(), None);
    }

    #[test]
    fn a_release_away_from_the_grid_moves_nothing() {
        let cells = grid(11, 4);
        let mut drag = Drag::new("ochres", 0);
        drag.aim(&cells, Some(centre(3, 4)), GAP, moving(0, 11));
        assert_eq!(drag.destination(), Some(3));
        // Out of the grid, and then off the window altogether. Both have to
        // clear the target, or a release would drop the colour wherever the
        // pointer last happened to pass.
        drag.aim(
            &cells,
            Some(pos2(ORIGIN.x + 400.0, ORIGIN.y)),
            GAP,
            moving(0, 11),
        );
        assert_eq!(drag.destination(), None);
        drag.aim(&cells, Some(centre(3, 4)), GAP, moving(0, 11));
        drag.aim(&cells, None, GAP, moving(0, 11));
        assert_eq!(drag.destination(), None);
    }

    /// The grid is rebuilt every frame, and a colour removed under the drag
    /// changes what is in it — as does scrolling, which culls the cells out of
    /// view. Aiming at where a cell used to be must answer with whatever is
    /// there now, and never with an index the palette no longer holds.
    #[test]
    fn a_grid_that_has_changed_under_the_drag_still_answers_for_itself() {
        let mut drag = Drag::new("ochres", 0);
        drag.aim(&grid(11, 4), Some(centre(9, 4)), GAP, moving(0, 11));
        assert_eq!(drag.destination(), Some(9));

        // Two colours removed: the same point is now past the end of the last
        // row, and out of reach of anything.
        let shorter = grid(9, 4);
        assert_eq!(
            drag.aim(&shorter, Some(centre(9, 4)), GAP, moving(0, 9)),
            None
        );
        assert_eq!(drag.destination(), None);
        // And a point still on a colour answers for the shorter grid.
        assert_eq!(
            drag.aim(&shorter, Some(centre(8, 4)), GAP, moving(0, 9)),
            Some(8)
        );
    }
}
