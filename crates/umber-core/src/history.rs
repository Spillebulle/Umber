//! Undo/redo.
//!
//! Rather than snapshotting the whole layer per stroke (16 MB at 2048², which
//! would blow past a gigabyte in ~60 strokes), we store only the rectangle a
//! stroke actually touched. A typical stroke damages a small fraction of the
//! canvas, so this keeps a deep history in a modest budget.
//!
//! Every entry also carries an [`EditKind`] and the moment it was made, which
//! is what lets the history be *listed* rather than only stepped through. The
//! two stacks together are a timeline — everything applied, then everything
//! undone — and [`History::steps_to`] turns a position in that timeline into
//! the number of single steps needed to reach it, so a click on a row costs the
//! caller the same work as that many presses of undo and no new pixel path.

use std::time::Duration;

use crate::brush::BrushMode;
use crate::geom::PixelRect;
use crate::time::Timestamp;

/// What one recorded edit was, so a list of them can be named.
///
/// Deliberately closed and short. An entry exists only where a patch was
/// captured: adding a layer, deleting one or reordering the stack are not
/// undoable, and deleting one clears the history outright. A variant here would
/// be a promise the engine cannot keep — a row naming an action that clicking
/// it will not restore is worse than an action the list stays quiet about.
///
/// [`EditKind::Transform`] earns its place under that rule rather than being an
/// exception to it. A transform captures one patch spanning the source *and*
/// the destination — see `transform::Transform::damage` — so replaying it puts
/// the pixels back where they came from and takes them out of where they went,
/// in one write. A paste is the same shape with no source to restore, which is
/// why it is not a variant of its own: what the engine holds is a rectangle of
/// pixels either way, and two rows that undo identically should not have two
/// names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Paint,
    Erase,
    /// Pixels moved, scaled or turned — or pasted, which is the same patch with
    /// nothing where they came from.
    Transform,
}

impl EditKind {
    pub const ALL: [EditKind; 3] = [Self::Paint, Self::Erase, Self::Transform];

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
            Self::Transform => "Transform",
        }
    }
}

/// One undoable edit: what it was, when it happened, and the pixels it
/// replaced.
#[derive(Clone, Debug)]
pub struct Edit {
    pub kind: EditKind,
    /// When the edit was made, on the wall clock.
    ///
    /// `None` where that is genuinely not known — an entry read out of a
    /// document written before histories carried times. A list shows nothing
    /// for it rather than a plausible-looking time it made up, because a wrong
    /// timestamp is indistinguishable from a right one.
    pub at: Option<Timestamp>,
    pub patch: PixelPatch,
}

impl Edit {
    /// An edit made *now*.
    pub fn new(kind: EditKind, patch: PixelPatch) -> Self {
        Self {
            kind,
            at: Some(Timestamp::now()),
            patch,
        }
    }

    /// An edit whose time is already settled.
    ///
    /// Two callers, and both of them matter. Undo and redo rebuild an entry as
    /// they move it between the stacks, and it must keep the time it was
    /// painted — recomputing it would make stepping through the history
    /// rewrite the history's own clock, so the gaps in the list would churn
    /// every time the user pressed Ctrl+Z. The reader of a saved document is
    /// the other, and it is where `None` comes from.
    pub fn made_at(kind: EditKind, at: Option<Timestamp>, patch: PixelPatch) -> Self {
        Self { kind, at, patch }
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

    /// The ceiling this history keeps itself under.
    ///
    /// Read by the History module so it can say what the limit *is* once
    /// [`History::dropped`] is non-zero. "Earlier edits discarded" is true and
    /// explains nothing, and the explanation is not guessable: nothing else on
    /// screen says undo has a size at all. Exposed rather than retyped in the
    /// panel so the note cannot come to name a figure this no longer holds.
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
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

    /// The edit at `index` in the timeline, whichever stack is holding it.
    ///
    /// Indexing rather than an iterator so a list can draw row `i` without
    /// holding a borrow across the loop, and so nothing here allocates.
    pub fn entry_at(&self, index: usize) -> Option<&Edit> {
        if let Some(edit) = self.undo.get(index) {
            return Some(edit);
        }
        // The redo stack is newest-first — popping it is the next redo — so
        // reading it as a continuation of the timeline means reading it
        // backwards.
        let from_end = index.checked_sub(self.undo.len())?;
        let index = self.redo.len().checked_sub(from_end + 1)?;
        self.redo.get(index)
    }

    /// What the edit at `index` in the timeline was, for a list to name.
    pub fn kind_at(&self, index: usize) -> Option<EditKind> {
        self.entry_at(index).map(|edit| edit.kind)
    }

    /// When the edit at `index` was made, where that is known.
    pub fn time_at(&self, index: usize) -> Option<Timestamp> {
        self.entry_at(index)?.at
    }

    /// How long passed between the edit at `index` and the one before it.
    ///
    /// The gap, not the age: what the list shows is how long the artist spent
    /// between one mark and the next, which is a property of the pair and does
    /// not change as the afternoon wears on. An age would have every row
    /// counting up, so a still panel would need repainting every second to stay
    /// truthful.
    ///
    /// `None` at index 0 — there is nothing before it — for an entry either
    /// side of which has no recorded time, and for a pair the clock puts in the
    /// wrong order. See [`Timestamp::since`] for that last one.
    pub fn gap_at(&self, index: usize) -> Option<Duration> {
        let previous = self.time_at(index.checked_sub(1)?)?;
        self.time_at(index)?.since(previous)
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

    /// Replace the whole timeline with one read back out of a document.
    ///
    /// `entries` is timeline order — everything applied, oldest first, then
    /// everything undone in the order redoing would put it back, which is
    /// exactly what [`History::entry_at`] walks. `position` is how many of them
    /// are applied, so the document reopens with the cursor where it was left
    /// rather than at one end of its own history.
    ///
    /// `dropped` is carried across rather than reset: a history that did not
    /// reach the beginning of the document when it was saved does not reach it
    /// now either, and the list has to be able to say so.
    ///
    /// The in-memory budget still applies — a file written by a build with a
    /// larger one must not be able to hand this process more than it allows.
    pub fn restore(&mut self, entries: Vec<Edit>, position: usize, dropped: usize) {
        self.clear();
        let mut entries = entries;
        let position = position.min(entries.len());
        let redone = entries.split_off(position);

        self.used_bytes = entries.iter().chain(&redone).map(Edit::byte_len).sum();
        self.undo = entries;
        // Timeline order into stack order: the *next* redo is the entry
        // immediately after the cursor, and that is what pops off the end.
        self.redo = redone.into_iter().rev().collect();
        self.dropped = dropped;
        self.evict_to_budget();
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
            h.push_redo(Edit::made_at(e.kind, e.at, patch(4, 4, 9)));
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
            h.push_redo(Edit::made_at(e.kind, e.at, patch(4, 4, 0)));
        }
        assert_eq!(h.position(), 1);
        assert_eq!(h.steps_to(4), Jump::Redo(3));
        assert_eq!(h.steps_to(1), Jump::Stay);
        assert_eq!(h.steps_to(0), Jump::Undo(1));
    }

    /// A history read back out of a document has to come back as the same
    /// timeline it was written from — the same entries in the same order, the
    /// same cursor within them, and both stacks still usable. Restoring only
    /// the undo half would silently throw away work the artist had undone and
    /// meant to come back to.
    #[test]
    fn a_restored_timeline_reads_exactly_as_the_one_it_came_from() {
        let mut original = History::default();
        original.record(Edit::new(EditKind::Paint, patch(4, 4, 1)));
        original.record(Edit::new(EditKind::Erase, patch(4, 4, 2)));
        original.record(Edit::new(EditKind::Paint, patch(4, 4, 3)));
        // Step back one, so the timeline straddles both stacks.
        let undone = original.take_undo().unwrap();
        original.push_redo(Edit::made_at(undone.kind, undone.at, patch(4, 4, 9)));

        let timeline: Vec<Edit> = (0..original.len())
            .map(|i| original.entry_at(i).unwrap().clone())
            .collect();

        let mut restored = History::default();
        restored.restore(timeline, original.position(), original.dropped());

        assert_eq!(restored.len(), original.len());
        assert_eq!(restored.position(), original.position());
        assert_eq!(restored.used_bytes(), original.used_bytes());
        for i in 0..original.len() {
            let (a, b) = (original.entry_at(i).unwrap(), restored.entry_at(i).unwrap());
            assert_eq!(a.kind, b.kind, "entry {i}");
            assert_eq!(a.at, b.at, "entry {i}");
            assert_eq!(a.patch.rect, b.patch.rect, "entry {i}");
            assert_eq!(a.patch.slot, b.patch.slot, "entry {i}");
            assert_eq!(a.patch.bytes, b.patch.bytes, "entry {i}");
        }

        // And the cursor is where it was: one redo available, two undos.
        assert_eq!(restored.take_redo().unwrap().patch.bytes[0], 9);
        assert_eq!(restored.take_undo().unwrap().patch.bytes[0], 2);
    }

    /// A file written by a build with a larger budget must not be able to hand
    /// this one more than it allows.
    #[test]
    fn restoring_still_answers_to_the_budget() {
        let entries: Vec<Edit> = (0..8u8).map(|i| edit(16, 16, i)).collect();
        let mut h = History::with_budget(2500);
        h.restore(entries, 8, 0);
        assert!(h.used_bytes() <= 2500, "used {}", h.used_bytes());
        assert!(h.dropped() > 0, "the budget dropped nothing");
        assert_eq!(h.dropped() + h.len(), 8);
    }

    /// What the list's time column reads off: the distance from one entry to
    /// the one before it, and nothing at all where that is not a distance.
    #[test]
    fn a_gap_is_measured_against_the_entry_before_it() {
        let at = |secs: i64| Some(Timestamp::from_unix_millis(secs * 1000));
        let mut h = History::default();
        h.record(Edit::made_at(EditKind::Paint, at(100), patch(4, 4, 1)));
        h.record(Edit::made_at(EditKind::Paint, at(101), patch(4, 4, 2)));
        h.record(Edit::made_at(EditKind::Erase, at(191), patch(4, 4, 3)));
        // No time at all — a document written before histories carried one.
        h.record(Edit::made_at(EditKind::Paint, None, patch(4, 4, 4)));
        // And after it, one whose predecessor has none.
        h.record(Edit::made_at(EditKind::Paint, at(400), patch(4, 4, 5)));
        // A clock put back between two strokes.
        h.record(Edit::made_at(EditKind::Paint, at(200), patch(4, 4, 6)));

        assert_eq!(h.gap_at(0), None, "nothing precedes the first entry");
        assert_eq!(h.gap_at(1), Some(Duration::from_secs(1)));
        assert_eq!(h.gap_at(2), Some(Duration::from_secs(90)));
        assert_eq!(h.gap_at(3), None, "the entry itself has no time");
        assert_eq!(h.gap_at(4), None, "the entry before it has no time");
        assert_eq!(h.gap_at(5), None, "the clock ran backwards");
        assert_eq!(h.gap_at(6), None, "off the end");

        assert_eq!(h.time_at(2), at(191));
        assert_eq!(h.time_at(3), None);
    }

    /// Stepping through the history must not rewrite its clock. An entry moved
    /// between the stacks keeps the moment it was painted, or the gaps in the
    /// list would churn every time the user pressed Ctrl+Z.
    #[test]
    fn undoing_does_not_restamp_an_entry() {
        let made = Timestamp::from_unix_millis(1_700_000_000_000);
        let mut h = History::default();
        h.record(Edit::made_at(EditKind::Paint, Some(made), patch(4, 4, 1)));

        let e = h.take_undo().unwrap();
        assert_eq!(e.at, Some(made));
        h.push_redo(Edit::made_at(e.kind, e.at, patch(4, 4, 2)));
        assert_eq!(h.time_at(0), Some(made), "the redo entry lost its time");

        let e = h.take_redo().unwrap();
        h.push_undo(Edit::made_at(e.kind, e.at, patch(4, 4, 3)));
        assert_eq!(h.time_at(0), Some(made), "redoing lost it");
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

    /// A patch is the *rectangle* a stroke covered, so its size follows the
    /// canvas and not the mark. On a 10000² document a stroke drawn across the
    /// picture damages the whole of it — 400 MB — and the default budget then
    /// holds exactly one, which is why the second such stroke ages the first
    /// out and the panel starts saying "Earlier edits discarded".
    ///
    /// Pinned because the History module states that as the reason. An
    /// explanation that had quietly stopped being arithmetically true would be
    /// worse than no explanation at all. Arithmetic rather than two recorded
    /// patches: the point is the ratio, and proving it by allocating 800 MB
    /// would be a test nobody can run on a small machine.
    #[test]
    fn one_broad_stroke_on_a_large_canvas_all_but_fills_the_budget() {
        let budget = History::default().budget_bytes();
        let patch = 10_000usize * 10_000 * 4;
        assert!(patch < budget, "not even one such stroke is held");
        assert!(
            patch * 2 > budget,
            "two now fit, so the panel's note names the wrong reason"
        );
    }
}
