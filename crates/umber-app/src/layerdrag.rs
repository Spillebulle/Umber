//! Dragging a layer to another place in the stack.
//!
//! The model only: what is being carried, which row the pointer is over, and
//! what a release would actually do. Nothing here paints, which is what lets
//! the insertion index, the refusal and the hit testing be tested without a
//! window — the same division [`crate::dock`] keeps against [`crate::panels`]
//! and [`crate::brushdrag`] keeps against [`crate::brushlib`], for the same
//! reason. `panels::layers_body` supplies this frame's pointer and the
//! rectangles the rows landed in, and lights up whichever row this names.
//!
//! Three rules are decided here and none of them is obvious from the drawing
//! side:
//!
//! - **A drop that would not move anything is not a drop.** The position the
//!   layer is already at is refused rather than accepted-and-ignored, so the
//!   "this is where it lands" mark is never drawn over something that will do
//!   nothing. Same rule as [`crate::brushdrag`]'s "the collection it is already
//!   in".
//! - **Past the ends of the list is the end of the list**, not nothing. The
//!   gesture for "put this at the top" is to drag it up there, and a list a
//!   couple of rows long leaves plenty of panel above the first row and below
//!   the last to let go over. So the nearest row wins, and for a pointer past
//!   an end that is the end row.
//! - **Off to the side is nothing at all.** That is the way out of a drag: a
//!   panel is 264 px wide, so leaving it sideways is easy, and something has to
//!   mean "I have changed my mind". Vertical distance is clamped and horizontal
//!   distance is refused precisely because the two directions mean different
//!   things in a vertical list.
//! - **Horizontal position *inside* the list is the nesting.** Once folders
//!   exist a drop has to say two things — where in the order, and inside what —
//!   and the second is the pointer's `x` read against the same indent the rows
//!   are drawn with, one level per step, exactly as every tree outliner spells
//!   it. It is capped by what the row under the pointer can actually hold: one
//!   level *into* a folder, and no deeper than its own level for anything else,
//!   because a layer cannot be nested inside a layer.
//!
//!   This does not fight the rule above it. Sideways-out is decided by
//!   containment first, and the rows span the panel — so the leftmost step of
//!   the list means "top level" and leaving the list entirely still means "no".
//!
//! Whether a drop is *legal* is not decided here. `LayerStack` owns that — a
//! drop that would nest too deep, or put a folder inside itself, is refused by
//! the same code that would carry it out — so [`Drag::aim`] takes the test as a
//! predicate rather than reimplementing it. A mark promising a move the model
//! will then refuse is exactly the lying control the "no drop that does nothing"
//! rule already exists to prevent.
//!
//! The rows are given in whatever order the caller drew them — the layer list
//! draws top-first, which is the stack upside down — so everything here works
//! off geometry and the `index` each row carries, never off the position of a
//! row in the slice.

use egui::{Pos2, Rect};

/// One row of the layer list: the stack position it draws, and where it landed.
///
/// Rectangles rather than an assumed row height, because what a drop hits is a
/// question about geometry, and the panel is the only thing that knows what
/// egui actually laid out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    /// Index into `LayerStack`, counting from the bottom — the number
    /// `LayerStack::reorder` is spoken to in, not the row's place on screen.
    pub index: usize,
    pub rect: Rect,
    /// How deeply the entry is nested. What a drop onto this row means by
    /// default, and the floor a drop *into* it is measured from.
    pub depth: u8,
    /// This row is a folder, so a drop can land one level inside it.
    pub folder: bool,
}

/// Where a release would put the carried entry.
///
/// Two numbers, because with folders in the stack a position no longer says
/// where something goes: the same index at two depths is inside a folder or
/// beside it. They travel together so the mark on the list and the move that
/// happens are one answer — the reason [`Drag::aim`] returned one number
/// before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aim {
    pub index: usize,
    pub depth: u8,
}

/// A layer on its way to another position in the stack.
#[derive(Clone, Debug, PartialEq)]
pub struct Drag {
    /// Where the layer sits now. Positions do move under a drag — the whole
    /// point of it — but only at the drop, so this stays true for the whole
    /// gesture.
    pub from: usize,
    /// Its name, for the label that follows the pointer. Carried rather than
    /// looked up, so the label cannot disagree with what is being moved.
    pub name: String,
    /// Where a release right now would put it, or `None` where a release would
    /// do nothing.
    to: Option<Aim>,
}

impl Drag {
    pub fn new(from: usize, name: impl Into<String>) -> Self {
        Self {
            from,
            name: name.into(),
            to: None,
        }
    }

    /// Work out what a release now would do, and remember it.
    ///
    /// Returns the row to light up, which is exactly the one named by
    /// [`Drag::destination`] — one answer, so the mark on the list and the move
    /// that happens cannot disagree. `None` where a release would do nothing.
    ///
    /// `pointer` is an `Option` because a pointer that has left the window has
    /// no position, and a drag does not end just because it did.
    ///
    /// `indent` is the width of one level of nesting, which the panel owns
    /// because it is what the rows were actually drawn with. `allowed` is
    /// `LayerStack`'s own test — see the module docs for why the legality of a
    /// drop is asked rather than restated.
    pub fn aim(
        &mut self,
        rows: &[Row],
        pointer: Option<Pos2>,
        indent: f32,
        allowed: impl Fn(usize, u8) -> bool,
    ) -> Option<Aim> {
        let landed = pointer
            .and_then(|pos| aim_at(rows, pos, indent))
            .filter(|aim| allowed(aim.index, aim.depth));
        self.to = landed;
        landed
    }

    /// Where a release right now would move the layer.
    pub fn destination(&self) -> Option<Aim> {
        self.to
    }
}

/// The row under `pointer` and the nesting its horizontal position asks for.
///
/// Split out from [`Drag::aim`] so the geometry can be tested without a drag,
/// which is how the depth rule is pinned at all.
pub fn aim_at(rows: &[Row], pointer: Pos2, indent: f32) -> Option<Aim> {
    let index = row_at(rows, pointer)?;
    let row = rows.iter().find(|r| r.index == index)?;
    // One level per step of the indent, from the left edge of the list — the
    // same measurement the rows were drawn by.
    let steps = ((pointer.x - row.rect.left()) / indent.max(1.0)).floor();
    let asked = steps.clamp(0.0, u8::MAX as f32) as u8;
    // A folder can take something one level inside it; nothing else can take
    // anything deeper than itself, because a layer holds no layers.
    let ceiling = if row.folder {
        row.depth.saturating_add(1)
    } else {
        row.depth
    };
    Some(Aim {
        index,
        depth: asked.min(ceiling),
    })
}

/// Which row a pointer at `pointer` is actually on, with nothing clamped.
///
/// What a drag *begins* on, where [`row_at`] is what it ends on. The two are
/// deliberately different: the panel has the blend picker and the opacity
/// slider directly above the list and the same panel's other modules below, all
/// of them within the list's own horizontal span, so a press that [`row_at`]
/// rounded to the nearest row would turn dragging the opacity slider into
/// dragging the bottom layer. A press is a question about where the pointer is;
/// a drop is a question about where the user is aiming.
pub fn row_pressed(rows: &[Row], pointer: Pos2) -> Option<usize> {
    rows.iter()
        .find(|row| row.rect.contains(pointer))
        .map(|row| row.index)
}

/// Which row a pointer at `pointer` belongs to.
///
/// Containment first, which is every ordinary case and makes a pointer exactly
/// on the boundary between two rows belong to whichever the caller listed
/// first. Failing that — in the gap between two rows, or above the first or
/// below the last — the nearest row vertically, but only while the pointer is
/// within the span the rows themselves occupy horizontally. See the module
/// docs: up and down mean "further along the stack" and sideways means "no".
pub fn row_at(rows: &[Row], pointer: Pos2) -> Option<usize> {
    if let Some(index) = row_pressed(rows, pointer) {
        return Some(index);
    }
    let span = rows.iter().fold(None::<Rect>, |acc, row| match acc {
        Some(rect) => Some(rect.union(row.rect)),
        None => Some(row.rect),
    })?;
    if pointer.x < span.left() || pointer.x > span.right() {
        return None;
    }
    rows.iter()
        .min_by(|a, b| {
            distance_to(a.rect, pointer.y)
                .partial_cmp(&distance_to(b.rect, pointer.y))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|row| row.index)
}

/// How far `y` is outside a row's own vertical extent. Zero inside it.
fn distance_to(rect: Rect, y: f32) -> f32 {
    (rect.top() - y).max(y - rect.bottom()).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    /// One level of nesting, as `theme::metrics::LAYER_INDENT` spells it. Named
    /// here rather than imported so these tests describe a geometry of their
    /// own and do not move when the design does.
    const INDENT: f32 = 12.0;

    /// Four layers as the panel draws them: top of the stack first, so the row
    /// order and the index order are opposites. Rows are 30 px with 2 px
    /// between, which is a gap a pointer can genuinely be in.
    fn list() -> Vec<Row> {
        (0..4)
            .map(|drawn| Row {
                index: 3 - drawn,
                rect: Rect::from_min_size(
                    pos2(10.0, 100.0 + drawn as f32 * 32.0),
                    vec2(240.0, 30.0),
                ),
                depth: 0,
                folder: false,
            })
            .collect()
    }

    /// A stack with a folder in it, as the panel would lay it out:
    ///
    /// ```text
    ///   3  Folder          depth 0   <- drawn first, so topmost
    ///   2    Inside        depth 1
    ///   1  Loose           depth 0
    ///   0  Bottom          depth 0
    /// ```
    ///
    /// The folder is *above* its contents, which is the invariant the whole of
    /// `LayerStack`'s tree rests on — see its module docs.
    fn nested() -> Vec<Row> {
        let row = |drawn: usize, index: usize, depth: u8, folder: bool| Row {
            index,
            rect: Rect::from_min_size(pos2(10.0, 100.0 + drawn as f32 * 32.0), vec2(240.0, 30.0)),
            depth,
            folder,
        };
        vec![
            row(0, 3, 0, true),
            row(1, 2, 1, false),
            row(2, 1, 0, false),
            row(3, 0, 0, false),
        ]
    }

    /// Stands in for `LayerStack::can_reorder` over a flat stack: every drop is
    /// legal except the one that would move nothing. The real predicate is the
    /// model's, which is the point — see the module docs.
    fn flat(from: usize) -> impl Fn(usize, u8) -> bool {
        move |to, depth| !(to == from && depth == 0)
    }

    /// Every drop legal, for the tests that are about geometry alone.
    fn anything(_to: usize, _depth: u8) -> bool {
        true
    }

    fn at(index: usize, depth: u8) -> Option<Aim> {
        Some(Aim { index, depth })
    }

    #[test]
    fn a_pointer_finds_the_row_it_is_inside() {
        let rows = list();
        assert_eq!(row_at(&rows, pos2(100.0, 115.0)), Some(3));
        assert_eq!(row_at(&rows, pos2(100.0, 147.0)), Some(2));
        assert_eq!(row_at(&rows, pos2(100.0, 179.0)), Some(1));
        assert_eq!(row_at(&rows, pos2(100.0, 211.0)), Some(0));
    }

    /// The 2 px between two rows is not a hole the drop falls through.
    #[test]
    fn a_pointer_in_the_gap_between_two_rows_takes_the_nearer() {
        let rows = list();
        assert_eq!(row_at(&rows, pos2(100.0, 130.5)), Some(3));
        assert_eq!(row_at(&rows, pos2(100.0, 131.5)), Some(2));
    }

    /// Dragging above the first row is how "put it at the top" is said, and
    /// below the last is how "put it at the bottom" is said.
    #[test]
    fn past_the_ends_lands_on_the_end_row() {
        let rows = list();
        assert_eq!(row_at(&rows, pos2(100.0, 40.0)), Some(3), "above the top");
        assert_eq!(
            row_at(&rows, pos2(100.0, 600.0)),
            Some(0),
            "below the bottom"
        );
    }

    /// Sideways is the way out. Clamping in x as well as y would leave a drag
    /// with nowhere to be abandoned short of the window edge.
    #[test]
    fn a_pointer_beside_the_list_is_over_nothing() {
        let rows = list();
        assert_eq!(row_at(&rows, pos2(400.0, 115.0)), None);
        assert_eq!(row_at(&rows, pos2(-5.0, 115.0)), None);
    }

    #[test]
    fn an_empty_list_is_over_nothing() {
        assert_eq!(row_at(&[], pos2(100.0, 115.0)), None);
        assert_eq!(row_pressed(&[], pos2(100.0, 115.0)), None);
    }

    /// A press clamps to nothing. The opacity slider sits directly above the
    /// list, inside the same horizontal span, and a press on it must not pick
    /// up the layer nearest to it.
    #[test]
    fn a_press_is_only_ever_on_a_row_it_is_inside() {
        let rows = list();
        assert_eq!(row_pressed(&rows, pos2(100.0, 115.0)), Some(3));
        assert_eq!(
            row_pressed(&rows, pos2(100.0, 80.0)),
            None,
            "above the list"
        );
        assert_eq!(
            row_pressed(&rows, pos2(100.0, 600.0)),
            None,
            "below the list"
        );
        assert_eq!(row_pressed(&rows, pos2(100.0, 131.0)), None, "in a gap");
    }

    #[test]
    fn a_drop_on_another_row_names_its_position() {
        let mut drag = Drag::new(3, "Layer 4");
        assert_eq!(
            drag.aim(&list(), Some(pos2(100.0, 179.0)), INDENT, flat(3)),
            at(1, 0)
        );
        assert_eq!(drag.destination(), at(1, 0));
    }

    /// The one target that has to be refused: accepting it would light a row up
    /// to promise a move that then does not happen. Decided by the model now —
    /// this only checks that the answer is carried through.
    #[test]
    fn a_drop_where_the_layer_already_is_does_nothing() {
        let rows = list();
        let mut drag = Drag::new(2, "Layer 3");
        assert_eq!(
            drag.aim(&rows, Some(pos2(100.0, 147.0)), INDENT, flat(2)),
            None
        );
        assert_eq!(drag.destination(), None);
        // Its neighbours are still perfectly good targets.
        assert_eq!(
            drag.aim(&rows, Some(pos2(100.0, 115.0)), INDENT, flat(2)),
            at(3, 0)
        );
        assert_eq!(drag.destination(), at(3, 0));
    }

    /// The top layer dragged above the top is the same refusal: it is already
    /// there. The clamp must not turn "past the end" into a move of its own.
    #[test]
    fn the_top_layer_dragged_further_up_still_does_nothing() {
        let mut drag = Drag::new(3, "Layer 4");
        assert_eq!(
            drag.aim(&list(), Some(pos2(100.0, 20.0)), INDENT, flat(3)),
            None
        );
        assert_eq!(drag.destination(), None);
    }

    #[test]
    fn a_release_away_from_the_list_moves_nothing() {
        let rows = list();
        let mut drag = Drag::new(0, "Layer 1");
        drag.aim(&rows, Some(pos2(100.0, 115.0)), INDENT, flat(0));
        assert_eq!(drag.destination(), at(3, 0));
        // Dragged out sideways, and then off the window altogether. Both have
        // to clear the target, or a release would drop the layer wherever the
        // pointer last happened to pass.
        drag.aim(&rows, Some(pos2(400.0, 115.0)), INDENT, flat(0));
        assert_eq!(drag.destination(), None);
        drag.aim(&rows, Some(pos2(100.0, 115.0)), INDENT, flat(0));
        drag.aim(&rows, None, INDENT, flat(0));
        assert_eq!(drag.destination(), None);
    }

    /// The list is rebuilt every frame and a layer added or deleted under the
    /// drag changes what is in it. Aiming at where a row used to be must answer
    /// with whatever is there now.
    #[test]
    fn a_list_that_has_changed_under_the_drag_still_answers_for_itself() {
        let mut drag = Drag::new(0, "Layer 1");
        drag.aim(&list(), Some(pos2(100.0, 115.0)), INDENT, flat(0));
        assert_eq!(drag.destination(), at(3, 0));

        let shorter = vec![Row {
            index: 1,
            rect: Rect::from_min_size(pos2(10.0, 100.0), vec2(240.0, 30.0)),
            depth: 0,
            folder: false,
        }];
        assert_eq!(
            drag.aim(&shorter, Some(pos2(100.0, 115.0)), INDENT, flat(0)),
            at(1, 0)
        );
        assert_eq!(drag.destination(), at(1, 0));
    }

    /// The horizontal position is the nesting, one level per step of the
    /// indent, measured from the row's own left edge.
    #[test]
    fn how_far_right_the_pointer_is_decides_the_nesting() {
        let rows = nested();
        // On the folder's own row: left of the first step is beside it, past
        // the first step is inside it.
        assert_eq!(aim_at(&rows, pos2(14.0, 115.0), INDENT), at(3, 0));
        assert_eq!(aim_at(&rows, pos2(30.0, 115.0), INDENT), at(3, 1));
        // And no further, however far right the pointer goes: a folder takes
        // one level, not as many as the panel is wide.
        assert_eq!(aim_at(&rows, pos2(240.0, 115.0), INDENT), at(3, 1));
    }

    /// A layer holds no layers, so nothing can be dropped *into* one however
    /// far right the pointer is.
    #[test]
    fn a_layer_cannot_be_nested_inside_a_layer() {
        let rows = nested();
        // The row at depth 0 that is not a folder.
        assert_eq!(aim_at(&rows, pos2(240.0, 179.0), INDENT), at(1, 0));
        // And the one already inside the folder keeps its own level rather
        // than gaining another.
        assert_eq!(aim_at(&rows, pos2(240.0, 147.0), INDENT), at(2, 1));
    }

    /// Dragging to the far left of the list is how something comes *out* of a
    /// folder — the row under the pointer is inside one, and the pointer is
    /// not.
    #[test]
    fn the_left_edge_of_the_list_is_the_top_level() {
        let rows = nested();
        assert_eq!(
            aim_at(&rows, pos2(12.0, 147.0), INDENT),
            at(2, 0),
            "over a nested row but hard left: beside the folder, not in it"
        );
    }

    /// The predicate is the model's answer, and a refusal has to clear the
    /// mark rather than leave the last legal one standing.
    #[test]
    fn a_drop_the_model_refuses_lights_nothing_up() {
        let rows = nested();
        let mut drag = Drag::new(0, "Bottom");
        assert_eq!(
            drag.aim(&rows, Some(pos2(30.0, 115.0)), INDENT, anything),
            at(3, 1)
        );
        // The same gesture, with the model saying no — a folder cannot go
        // inside itself, a stack cannot nest that deep, and this stands in for
        // all of them.
        //
        // **Refused outright rather than nudged to a depth that would be
        // allowed.** A fallback would move the layer somewhere the pointer did
        // not ask for, and the way to say "beside the folder instead" already
        // exists and is one the painter is holding: move left. Nothing lights
        // up, which is the honest answer to "this drop cannot happen".
        assert_eq!(
            drag.aim(&rows, Some(pos2(30.0, 115.0)), INDENT, |_, depth| depth
                == 0),
            None
        );
        assert_eq!(drag.destination(), None);
        // And the shallower drop the pointer *can* ask for is still offered.
        assert_eq!(
            drag.aim(&rows, Some(pos2(14.0, 115.0)), INDENT, |_, depth| depth
                == 0),
            at(3, 0)
        );
    }
}
