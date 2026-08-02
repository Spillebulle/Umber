//! Dragging a brush out of one collection and into another.
//!
//! The model only: what is being carried, what the pointer is over, and what a
//! release would actually do. Nothing here paints, which is what lets the drop
//! rules be tested without a window — the same division [`crate::dock`] keeps
//! against [`crate::panels`], and for the same reason. [`crate::brushlib`]
//! supplies this frame's pointer and the rectangles the rail's rows landed in,
//! and lights up whichever row this names.
//!
//! Two rules are worth stating up front, because both are decided here and
//! neither is obvious from the drawing side:
//!
//! - **A brush is carried by id, never by its position** in `Editor::presets`.
//!   A move rewrites the grouping and the merged list is rebuilt around it, so
//!   an index taken when the drag began names a different brush by the time it
//!   ends. That is the same rule `brushlib::resync` re-finds the selection by.
//! - **A drop that would not move anything is not a drop.** The collection the
//!   brush is already in is refused rather than accepted-and-ignored, so the
//!   "this is where it lands" mark is never drawn over something that will do
//!   nothing.

use egui::{Pos2, Rect};

/// One row of the collection rail: what it is called, and where it landed.
///
/// Rectangles rather than indices, because what a drop hits is a question about
/// geometry. Owned names because the rail is rebuilt from the index every
/// frame and borrowing from it would tie a drag that outlives the frame to a
/// list that does not.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub rect: Rect,
}

/// A brush on its way from one collection to another.
#[derive(Clone, Debug, PartialEq)]
pub struct Drag {
    /// The brush's stable id. See the module docs: never its position.
    pub id: String,
    /// Its name, for the label that follows the pointer. Carried rather than
    /// looked up, so the label survives the list being rebuilt underneath it.
    pub name: String,
    /// The collection it is filed under now, which is the one drop target that
    /// has to be refused.
    pub from: String,
    /// The collection a release right now would move it to, or `None` where a
    /// release would do nothing.
    over: Option<String>,
}

impl Drag {
    pub fn new(id: impl Into<String>, name: impl Into<String>, from: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            from: from.into(),
            over: None,
        }
    }

    /// Work out what a release now would do, and remember it.
    ///
    /// Returns the index of the row to light up, which is exactly the row named
    /// by [`Drag::destination`] — one answer, so the mark on the rail and the
    /// move that happens cannot disagree. `None` where a release would do
    /// nothing: the pointer is off the rail, or over the collection the brush
    /// is already in.
    ///
    /// `pointer` is an `Option` because a pointer that has left the window has
    /// no position, and a drag does not end just because it did.
    pub fn aim(&mut self, rows: &[Row], pointer: Option<Pos2>) -> Option<usize> {
        let landed = pointer
            .and_then(|pos| row_at(rows, pos))
            .filter(|i| rows[*i].name != self.from);
        // Compared before it is replaced. This runs every frame of a drag, and
        // the answer changes a handful of times in one; cloning the name on
        // every frame instead would be an allocation per frame for nothing.
        if self.over.as_deref() != landed.map(|i| rows[i].name.as_str()) {
            self.over = landed.map(|i| rows[i].name.clone());
        }
        landed
    }

    /// The collection a release right now would move the brush to.
    pub fn destination(&self) -> Option<&str> {
        self.over.as_deref()
    }
}

/// Which row a pointer at `pointer` is over.
///
/// The first that contains it. The rail's rows do not overlap, so the only case
/// this decides is a pointer exactly on the boundary between two — and taking
/// the first makes that the upper one every time, rather than whichever way
/// round the rail happened to be built.
pub fn row_at(rows: &[Row], pointer: Pos2) -> Option<usize> {
    rows.iter().position(|row| row.rect.contains(pointer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    /// A rail of three collections, stacked as the browser draws them.
    fn rail() -> Vec<Row> {
        ["Imported", "My brushes", "Pencils & sketching"]
            .iter()
            .enumerate()
            .map(|(i, name)| Row {
                name: (*name).to_owned(),
                rect: Rect::from_min_size(pos2(0.0, i as f32 * 26.0), vec2(200.0, 26.0)),
            })
            .collect()
    }

    #[test]
    fn a_pointer_finds_the_row_it_is_inside() {
        let rows = rail();
        assert_eq!(row_at(&rows, pos2(10.0, 13.0)), Some(0));
        assert_eq!(row_at(&rows, pos2(10.0, 39.0)), Some(1));
        assert_eq!(row_at(&rows, pos2(10.0, 65.0)), Some(2));
        // Off the rail entirely — over the list of brushes, or outside the
        // dialog. Neither is a collection.
        assert_eq!(row_at(&rows, pos2(400.0, 13.0)), None);
        assert_eq!(row_at(&rows, pos2(10.0, 200.0)), None);
    }

    /// Rows meet exactly, so the boundary belongs to one of them by a rule
    /// rather than by luck.
    #[test]
    fn a_pointer_on_a_boundary_lands_in_the_upper_row() {
        assert_eq!(row_at(&rail(), pos2(10.0, 26.0)), Some(0));
    }

    #[test]
    fn a_drop_on_another_collection_names_it() {
        let rows = rail();
        let mut drag = Drag::new("umber/ink", "Ink", "Inks & pens");
        assert_eq!(drag.aim(&rows, Some(pos2(10.0, 39.0))), Some(1));
        assert_eq!(drag.destination(), Some("My brushes"));
    }

    /// The collection the brush is already in is the one target that has to be
    /// refused: accepting it would light a row up to promise a move that then
    /// does not happen.
    #[test]
    fn a_drop_where_the_brush_already_is_does_nothing() {
        let rows = rail();
        let mut drag = Drag::new("user/x", "X", "My brushes");
        assert_eq!(drag.aim(&rows, Some(pos2(10.0, 39.0))), None);
        assert_eq!(drag.destination(), None);
        // And its neighbours are still perfectly good targets.
        assert_eq!(drag.aim(&rows, Some(pos2(10.0, 13.0))), Some(0));
        assert_eq!(drag.destination(), Some("Imported"));
    }

    #[test]
    fn a_release_away_from_the_rail_moves_nothing() {
        let rows = rail();
        let mut drag = Drag::new("umber/ink", "Ink", "Inks & pens");
        drag.aim(&rows, Some(pos2(10.0, 13.0)));
        assert_eq!(drag.destination(), Some("Imported"));
        // Dragged back out over the brush list, and then off the window
        // altogether. Both have to clear the target, or a release would file
        // the brush wherever the pointer last happened to pass.
        drag.aim(&rows, Some(pos2(400.0, 13.0)));
        assert_eq!(drag.destination(), None);
        drag.aim(&rows, Some(pos2(10.0, 13.0)));
        drag.aim(&rows, None);
        assert_eq!(drag.destination(), None);
    }

    /// The rail is rebuilt every frame, and a collection emptied by an earlier
    /// move is simply not in it any more. Aiming at where it used to be must
    /// answer with whatever is there now.
    #[test]
    fn a_rail_that_has_changed_under_the_drag_still_answers_for_itself() {
        let mut drag = Drag::new("umber/ink", "Ink", "Inks & pens");
        drag.aim(&rail(), Some(pos2(10.0, 13.0)));
        assert_eq!(drag.destination(), Some("Imported"));

        let shorter = vec![Row {
            name: "Markers".to_owned(),
            rect: Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 26.0)),
        }];
        assert_eq!(drag.aim(&shorter, Some(pos2(10.0, 13.0))), Some(0));
        assert_eq!(drag.destination(), Some("Markers"));
    }
}
