//! Undo/redo.
//!
//! Rather than snapshotting the whole layer per stroke (16 MB at 2048², which
//! would blow past a gigabyte in ~60 strokes), we store only the rectangle a
//! stroke actually touched. A typical stroke damages a small fraction of the
//! canvas, so this keeps a deep history in a modest budget.
//!
//! Every entry also carries an [`EditKind`], which is what lets the history be
//! *listed* rather than only stepped through. The two stacks together are a
//! timeline — everything applied, then everything undone — and
//! [`History::steps_to`] turns a position in that timeline into the number of
//! single steps needed to reach it, so a click on a row costs the caller the
//! same work as that many presses of undo and no new pixel path.

use crate::brush::BrushMode;
use crate::geom::PixelRect;

/// What one recorded edit was, so a list of them can be named.
///
/// Deliberately closed and short. An entry exists only where a patch was
/// captured, and today that is a stroke and nothing else: adding a layer,
/// deleting one or reordering the stack are not undoable, and deleting one
/// clears the history outright. A variant here would be a promise the engine
/// cannot keep — a row naming an action that clicking it will not restore is
/// worse than an action the list stays quiet about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Paint,
    Erase,
}

impl EditKind {
    /// Which kind a stroke in `mode` records.
    ///
    /// Here rather than at the call site because the answer belongs with the
    /// list that shows it, and because it is the one place the two enums meet.
    pub fn for_mode(mode: BrushMode) -> Self {
        match mode {
            BrushMode::Paint => Self::Paint,
            BrushMode::Erase => Self::Erase,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Paint => "Stroke",
            Self::Erase => "Erase",
        }
    }
}

/// One undoable edit: what it was, and the pixels it replaced.
#[derive(Clone, Debug)]
pub struct Edit {
    pub kind: EditKind,
    pub patch: PixelPatch,
}

impl Edit {
    pub fn new(kind: EditKind, patch: PixelPatch) -> Self {
        Self { kind, patch }
    }

    fn byte_len(&self) -> usize {
        self.patch.byte_len()
    }
}

/// How to reach a given position in the timeline from where the history is now.
///
/// A count rather than a target index, because carrying it out is the caller's
/// — every step is a GPU read and write — and a count is the one form that
/// cannot ask for more steps than there are entries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jump {
    Stay,
    Undo(usize),
    Redo(usize),
}

/// The RGBA8 contents of a rectangle, tightly packed at `width * 4` per row.
#[derive(Clone, Debug)]
pub struct PixelPatch {
    pub rect: PixelRect,
    /// Texture-array slot this patch belongs to. Slots are recycled on layer
    /// deletion, which is why [`History::clear`] must be called then — replaying
    /// this patch into a layer that merely inherited the slot would corrupt it.
    pub slot: u32,
    pub bytes: Vec<u8>,
}

impl PixelPatch {
    pub fn new(rect: PixelRect, slot: u32, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len() as u64,
            rect.area() * 4,
            "patch byte count must match rect area"
        );
        Self { rect, slot, bytes }
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }
}

/// A bounded two-stack undo history.
#[derive(Debug)]
pub struct History {
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    used_bytes: usize,
    budget_bytes: usize,
    /// Entries the budget has aged out of the bottom of the undo stack.
    ///
    /// Counted rather than forgotten so a list of the history can say that it
    /// does not reach all the way back. Silently drawing the oldest surviving
    /// entry as "the document as it opened" would be a lie about a state the
    /// user can no longer return to.
    dropped: usize,
}

impl Default for History {
    fn default() -> Self {
        Self::with_budget(512 * 1024 * 1024)
    }
}

impl History {
    pub fn with_budget(budget_bytes: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            used_bytes: 0,
            budget_bytes,
            dropped: 0,
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    // --- the timeline ------------------------------------------------------
    //
    // The two stacks read as one list: everything applied, oldest first, then
    // everything undone, in the order redoing would put it back. A *position*
    // in that list is a count of applied edits, so position 0 is the document
    // with none of them and `len()` is the document with all of them.

    /// How many edits are held, applied or undone.
    pub fn len(&self) -> usize {
        self.undo.len() + self.redo.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many of them are currently applied.
    pub fn position(&self) -> usize {
        self.undo.len()
    }

    /// What the edit at `index` in the timeline was, for a list to name.
    ///
    /// Indexing rather than an iterator so a list can draw row `i` without
    /// holding a borrow across the loop, and so nothing here allocates.
    pub fn kind_at(&self, index: usize) -> Option<EditKind> {
        if let Some(edit) = self.undo.get(index) {
            return Some(edit.kind);
        }
        // The redo stack is newest-first — popping it is the next redo — so
        // reading it as a continuation of the timeline means reading it
        // backwards.
        let from_end = index.checked_sub(self.undo.len())?;
        let index = self.redo.len().checked_sub(from_end + 1)?;
        self.redo.get(index).map(|edit| edit.kind)
    }

    /// How many entries the budget has discarded, which is how far short of
    /// the document's beginning the list stops.
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    /// The steps that reach `position` from where the history is now.
    ///
    /// `position` is clamped to what is held, so a click on a row of a list
    /// drawn a frame ago cannot ask for steps that do not exist.
    pub fn steps_to(&self, position: usize) -> Jump {
        let position = position.min(self.len());
        match position.cmp(&self.position()) {
            std::cmp::Ordering::Equal => Jump::Stay,
            std::cmp::Ordering::Less => Jump::Undo(self.position() - position),
            std::cmp::Ordering::Greater => Jump::Redo(position - self.position()),
        }
    }

    // --- mutation ----------------------------------------------------------

    /// Record the pre-stroke contents of the damaged region. Invalidates redo,
    /// as any new edit does.
    pub fn record(&mut self, edit: Edit) {
        self.used_bytes += edit.byte_len();
        for e in self.redo.drain(..) {
            self.used_bytes -= e.byte_len();
        }
        self.undo.push(edit);
        self.evict_to_budget();
    }

    /// Pop the state to restore. The caller must capture the *current* contents
    /// of the same rect first and hand it to [`History::push_redo`].
    pub fn take_undo(&mut self) -> Option<Edit> {
        let e = self.undo.pop()?;
        self.used_bytes -= e.byte_len();
        Some(e)
    }

    pub fn take_redo(&mut self) -> Option<Edit> {
        let e = self.redo.pop()?;
        self.used_bytes -= e.byte_len();
        Some(e)
    }

    pub fn push_redo(&mut self, edit: Edit) {
        self.used_bytes += edit.byte_len();
        self.redo.push(edit);
    }

    /// Re-push onto the undo stack without discarding redo — used when redoing.
    pub fn push_undo(&mut self, edit: Edit) {
        self.used_bytes += edit.byte_len();
        self.undo.push(edit);
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.used_bytes = 0;
        self.dropped = 0;
    }

    /// Drop the oldest undo entries until we fit. The most recent history is
    /// what users actually reach for, so age out from the bottom.
    fn evict_to_budget(&mut self) {
        while self.used_bytes > self.budget_bytes && self.undo.len() > 1 {
            let e = self.undo.remove(0);
            self.used_bytes -= e.byte_len();
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(w: u32, h: u32, fill: u8) -> PixelPatch {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        PixelPatch::new(rect, 0, vec![fill; (w * h * 4) as usize])
    }

    fn edit(w: u32, h: u32, fill: u8) -> Edit {
        Edit::new(EditKind::Paint, patch(w, h, fill))
    }

    #[test]
    fn undo_redo_round_trips() {
        let mut h = History::default();
        h.record(edit(4, 4, 1));
        assert!(h.can_undo());

        let undone = h.take_undo().unwrap();
        h.push_redo(edit(4, 4, 2));
        assert_eq!(undone.patch.bytes[0], 1);
        assert!(h.can_redo());

        let redone = h.take_redo().unwrap();
        assert_eq!(redone.patch.bytes[0], 2);
    }

    #[test]
    fn recording_clears_redo() {
        let mut h = History::default();
        h.record(edit(4, 4, 1));
        h.take_undo();
        h.push_redo(edit(4, 4, 2));
        assert!(h.can_redo());

        h.record(edit(4, 4, 3));
        assert!(!h.can_redo(), "a new edit must invalidate redo");
    }

    #[test]
    fn budget_evicts_oldest_first() {
        // Budget fits about two 16x16 patches (1024 bytes each).
        let mut h = History::with_budget(2500);
        for i in 0..8u8 {
            h.record(edit(16, 16, i));
        }
        assert!(h.used_bytes() <= 2500, "used {}", h.used_bytes());
        // The newest entry must survive eviction.
        assert_eq!(h.take_undo().unwrap().patch.bytes[0], 7);
    }

    #[test]
    fn accounting_stays_balanced() {
        let mut h = History::default();
        h.record(edit(8, 8, 1));
        h.record(edit(8, 8, 2));
        while h.take_undo().is_some() {}
        assert_eq!(h.used_bytes(), 0);
    }

    /// A list of the history has to name what each entry was, and an eraser
    /// stroke is not a paint stroke however identical the patch looks.
    #[test]
    fn every_entry_says_what_it_was() {
        let mut h = History::default();
        h.record(Edit::new(EditKind::Paint, patch(4, 4, 1)));
        h.record(Edit::new(EditKind::Erase, patch(4, 4, 2)));

        assert_eq!(h.len(), 2);
        assert_eq!(h.kind_at(0), Some(EditKind::Paint));
        assert_eq!(h.kind_at(1), Some(EditKind::Erase));
        assert_eq!(h.kind_at(2), None);
        assert_eq!(EditKind::for_mode(BrushMode::Erase), EditKind::Erase);
        assert_eq!(EditKind::Erase.label(), "Erase");
    }

    /// Undoing must not reorder the timeline. The entry that was second stays
    /// second whichever stack is holding it, or a list would shuffle every time
    /// the user pressed Ctrl+Z.
    #[test]
    fn the_timeline_reads_the_same_either_side_of_the_cursor() {
        let mut h = History::default();
        h.record(Edit::new(EditKind::Paint, patch(4, 4, 1)));
        h.record(Edit::new(EditKind::Erase, patch(4, 4, 2)));
        h.record(Edit::new(EditKind::Paint, patch(4, 4, 3)));

        let before: Vec<_> = (0..h.len()).map(|i| h.kind_at(i)).collect();
        assert_eq!(h.position(), 3);

        // Undo two, moving them onto the redo stack.
        for _ in 0..2 {
            let e = h.take_undo().unwrap();
            h.push_redo(Edit::new(e.kind, patch(4, 4, 9)));
        }
        assert_eq!(h.position(), 1);
        assert_eq!(h.len(), 3, "undoing does not discard anything");

        let after: Vec<_> = (0..h.len()).map(|i| h.kind_at(i)).collect();
        assert_eq!(before, after);
    }

    /// Clicking a row is a jump of however many steps it takes to get there,
    /// in whichever direction, and a row that is no longer in the list resolves
    /// to the nearest one that is rather than to a step count off the end.
    #[test]
    fn a_jump_counts_the_steps_to_a_position() {
        let mut h = History::default();
        for i in 0..4u8 {
            h.record(edit(4, 4, i));
        }
        assert_eq!(h.steps_to(4), Jump::Stay);
        assert_eq!(h.steps_to(1), Jump::Undo(3));
        assert_eq!(h.steps_to(0), Jump::Undo(4), "back to before anything");
        assert_eq!(h.steps_to(99), Jump::Stay, "clamped to what is held");

        for _ in 0..3 {
            let e = h.take_undo().unwrap();
            h.push_redo(Edit::new(e.kind, patch(4, 4, 0)));
        }
        assert_eq!(h.position(), 1);
        assert_eq!(h.steps_to(4), Jump::Redo(3));
        assert_eq!(h.steps_to(1), Jump::Stay);
        assert_eq!(h.steps_to(0), Jump::Undo(1));
    }

    /// The list must be able to say that it does not reach the beginning.
    #[test]
    fn eviction_is_counted_so_a_list_can_admit_to_it() {
        let mut h = History::with_budget(2500);
        assert_eq!(h.dropped(), 0);
        for i in 0..8u8 {
            h.record(edit(16, 16, i));
        }
        assert!(h.dropped() > 0, "the budget dropped nothing");
        assert_eq!(h.dropped() + h.len(), 8);
        h.clear();
        assert_eq!(h.dropped(), 0, "a cleared history reaches its beginning");
    }
}
