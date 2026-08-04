//! Undo/redo.
//!
//! Rather than snapshotting the whole layer per stroke (16 MB at 2048², which
//! would blow past a gigabyte in ~60 strokes), we store only the pixels a
//! stroke actually replaced.
//!
//! "Actually replaced" used to mean the stroke's bounding rectangle, and a
//! rectangle describes a diagonal terribly: a thin line corner to corner of a
//! 10000² canvas reserved 381 MB to record a few million pixels, which is a
//! history one step deep. A patch is therefore a set of [`PatchPiece`]s — the
//! cells of a [`crate::damage::TileMask`] the dabs reached, merged into runs
//! and clipped to the bounding box, so the pixels kept are always a subset of
//! what the box held. Measured, that same diagonal costs 6.8 MB, and a piece
//! whose pixels are all identical — blank canvas, a flat fill — costs four
//! bytes. `examples/measure-undo.rs` is where the numbers come from.
//!
//! Every entry also carries an [`EditKind`] and the moment it was made, which
//! is what lets the history be *listed* rather than only stepped through. The
//! two stacks together are a timeline — everything applied, then everything
//! undone — and [`History::steps_to`] turns a position in that timeline into
//! the number of single steps needed to reach it, so a click on a row costs the
//! caller the same work as that many presses of undo and no new pixel path.
//!
//! Not every entry holds pixels. A canvas flip is **its own inverse** and
//! preserves the canvas size, so it is recorded as an [`EditBody::Flip`] —
//! nothing at all — and undone by flipping again. That works only because the
//! timeline is stepped rather than seeked: an older patch is reached with the
//! flip above it already undone, so it applies verbatim at the rectangle it was
//! recorded at. See [`EditKind`].

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::brush::BrushMode;
use crate::geom::{FlipAxis, PixelRect};
use crate::layer::StackShape;
use crate::time::Timestamp;

/// What one document's history is allowed to hold unless it is told otherwise,
/// and what every build before the setting existed held.
///
/// Named rather than written into [`History::default`] so the preference that
/// now governs it has something to default *to* — a settings page that stated
/// 512 MB in its own words would be a second copy of this number.
pub const DEFAULT_BUDGET_BYTES: usize = 512 * 1024 * 1024;

/// The ceiling a [`History`] built with no explicit one takes.
///
/// Published rather than passed, for the reason [`set_default_budget`] gives.
static BUDGET: AtomicUsize = AtomicUsize::new(DEFAULT_BUDGET_BYTES);

/// Set the ceiling every history built from here on takes.
///
/// A published value rather than an argument because a `History` is created in
/// places that have no business knowing preferences exist — a blank document, a
/// new tab, an import — and threading a number through all of them would put
/// the setting into the signature of everything that opens a picture. The same
/// shape the application's shortcut table already uses, and for the same
/// reason: one published value, read where it is needed.
///
/// It does **not** reach a history that already exists. That is
/// [`History::set_budget`]'s job, and it is deliberately separate: lowering the
/// limit has to drop entries from the documents already open *now*, which is a
/// mutation of each of them and not a change of a global.
pub fn set_default_budget(bytes: usize) {
    BUDGET.store(bytes, Ordering::Relaxed);
}

/// The ceiling a history built now would take.
pub fn default_budget() -> usize {
    BUDGET.load(Ordering::Relaxed)
}

/// What one recorded edit was, so a list of them can be named.
///
/// Deliberately closed. An entry exists only for something the engine can
/// genuinely restore — a variant here would be a promise it cannot keep, and a
/// row naming an action that clicking it will not restore is worse than an
/// action the list stays quiet about. Clearing a layer and resizing the canvas
/// are still outside it, and the History module's footnote says so.
///
/// [`EditKind::Transform`] earns its place under that rule rather than being an
/// exception to it. A transform captures one patch spanning the source *and*
/// the destination — see `transform::Transform::damage` — so replaying it puts
/// the pixels back where they came from and takes them out of where they went,
/// in one write. A paste is the same shape with no source to restore, which is
/// why it is not a variant of its own: what the engine holds is a rectangle of
/// pixels either way, and two rows that undo identically should not have two
/// names.
///
/// The two flips earn it a different way, and they are the first entries that
/// carry **no pixels at all** — see [`EditBody::Flip`]. A canvas flip keeps the
/// canvas size, so unlike a resize it does not invalidate a single recorded
/// rectangle; and it is its own inverse, so the engine can undo one without
/// having stored anything. Undo here is strictly sequential — [`History::
/// steps_to`] turns a jump into that many single steps and there is nothing to
/// seek to — so by the time an older patch is reached the flip above it has
/// already been undone and the canvas is back in the orientation that patch was
/// recorded in. That is what makes "no coordinate mapping, no mirrored bytes"
/// true rather than merely convenient.
///
/// The six structural kinds earn it a third way, and they are the ones that
/// need the "two rows that undo identically must not have two names" rule
/// stated precisely, because under [`EditBody::Structure`] *every* structural
/// edit undoes identically — they all restore a shape. Read that way the rule
/// would collapse the lot into one row saying "Layers", which is plainly wrong.
/// The rule is about what the **painter did**, not how the engine stores it: a
/// paste and a transform fail it because both are a rectangle of pixels
/// arriving on a layer, and Add, Delete and Move pass it because somebody
/// scanning the list for "where did my layer go" is looking for exactly that
/// word. Hence also no `Ungroup` — dissolving a folder *is* a delete of the
/// folder or a set of moves out of it, and records as whichever it was — and no
/// separate kind for deleting a folder, which restores identically to deleting
/// a layer and out of the same entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    Paint,
    Erase,
    /// Pixels moved, scaled or turned — or pasted, which is the same patch with
    /// nothing where they came from.
    Transform,
    /// The whole canvas mirrored left to right.
    FlipHorizontal,
    /// The whole canvas mirrored top to bottom.
    FlipVertical,
    /// A layer, or an empty folder, added to the stack.
    AddLayer,
    /// An entry deleted — a layer, or a folder and everything in it.
    DeleteLayer,
    /// An entry moved, or re-nested, in the stack.
    MoveLayer,
    /// Entries gathered into a new folder.
    Group,
    /// A layer given a mask.
    AddMask,
    /// A layer's mask taken off. The one structural edit that changes the
    /// picture — what the mask hid comes back — which is why it is not filed
    /// under `DeleteLayer`.
    RemoveMask,
}

impl EditKind {
    pub const ALL: [EditKind; 11] = [
        Self::Paint,
        Self::Erase,
        Self::Transform,
        Self::FlipHorizontal,
        Self::FlipVertical,
        Self::AddLayer,
        Self::DeleteLayer,
        Self::MoveLayer,
        Self::Group,
        Self::AddMask,
        Self::RemoveMask,
    ];

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

    /// Which kind a flip about `axis` records.
    pub fn for_axis(axis: FlipAxis) -> Self {
        match axis {
            FlipAxis::Horizontal => Self::FlipHorizontal,
            FlipAxis::Vertical => Self::FlipVertical,
        }
    }

    /// The axis this entry mirrors about, for the kinds that mirror.
    ///
    /// The axis is read back off the kind rather than stored beside it, because
    /// there is exactly one kind per axis and a second copy could disagree with
    /// the row the History list draws.
    pub fn flip_axis(self) -> Option<FlipAxis> {
        match self {
            Self::FlipHorizontal => Some(FlipAxis::Horizontal),
            Self::FlipVertical => Some(FlipAxis::Vertical),
            Self::Paint
            | Self::Erase
            | Self::Transform
            | Self::AddLayer
            | Self::DeleteLayer
            | Self::MoveLayer
            | Self::Group
            | Self::AddMask
            | Self::RemoveMask => None,
        }
    }

    /// Is this an edit to the *stack* rather than to pixels?
    ///
    /// Read by the writer, which cannot yet put one in a file — see
    /// `docformat::history::SaveHistory::new`.
    pub fn is_structural(self) -> bool {
        matches!(
            self,
            Self::AddLayer
                | Self::DeleteLayer
                | Self::MoveLayer
                | Self::Group
                | Self::AddMask
                | Self::RemoveMask
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Paint => "Stroke",
            Self::Erase => "Erase",
            Self::Transform => "Transform",
            Self::FlipHorizontal => "Flip horizontally",
            Self::FlipVertical => "Flip vertically",
            Self::AddLayer => "Add layer",
            Self::DeleteLayer => "Delete layer",
            Self::MoveLayer => "Move layer",
            Self::Group => "Group",
            Self::AddMask => "Add mask",
            Self::RemoveMask => "Remove mask",
        }
    }
}

/// What an entry holds in order to be undone.
///
/// Three shapes, because there are three ways an edit can be reversible and
/// only one of them costs memory:
///
/// * [`EditBody::Pixels`] is everything that paints. The engine cannot work out
///   what was under a stroke, so it keeps it.
/// * [`EditBody::Structure`] is an edit to the layer *stack*. It stores no
///   pixels at all: what it holds is the shape the stack had, and the layers
///   the edit removed — which own their texture slices, so the pixels never
///   move and nothing is copied. See [`StackShape`].
/// * [`EditBody::Flip`] is an edit that is **its own inverse**, so there is
///   nothing to keep. Undoing it is doing it again.
///
/// **No entry mixes them.** A structural entry never carries a patch, for the
/// reason the timeline is stepped rather than seeked: a delete and the paint
/// before it are reached in order, so the paint never has to be carried inside
/// the delete. A body that meant two different things depending on the kind is
/// exactly what that buys freedom from.
///
/// Deliberately not a fourth arm for "some other self-inverse thing later": the
/// axis is in the [`EditKind`], and a body that carried its own copy of it
/// would be a second place for the row's icon and the pixels to disagree.
#[derive(Clone, Debug)]
pub enum EditBody {
    /// The pixels the edit replaced.
    Pixels(PixelPatch),
    /// The stack as it was. Boxed because it is much the largest arm and every
    /// painting entry would otherwise carry its footprint.
    Structure(Box<StackShape>),
    /// Nothing. See [`EditKind::flip_axis`] for which way.
    Flip,
}

impl From<PixelPatch> for EditBody {
    fn from(patch: PixelPatch) -> Self {
        Self::Pixels(patch)
    }
}

impl From<StackShape> for EditBody {
    fn from(shape: StackShape) -> Self {
        Self::Structure(Box::new(shape))
    }
}

impl EditBody {
    /// What this costs in memory, which is what the budget counts. A flip is
    /// free, and the list is allowed to hold as many of them as somebody has
    /// the patience to press.
    ///
    /// A structural entry is nearly free — tens of bytes an entry — so what
    /// bounds *those* is not the budget but the slice ceiling: a parked layer
    /// holds a slot claim, and there are [`crate::layer::LayerStack::MAX_SLOTS`]
    /// of those. [`History::free_until`] is the release valve.
    fn byte_len(&self) -> usize {
        match self {
            Self::Pixels(patch) => patch.byte_len(),
            Self::Structure(shape) => shape.byte_len(),
            Self::Flip => 0,
        }
    }
}

/// One undoable edit: what it was, when it happened, and whatever it takes to
/// put it back.
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
    pub body: EditBody,
}

impl Edit {
    /// An edit made *now*.
    ///
    /// `impl Into<EditBody>` so the overwhelmingly common caller — something
    /// that has just captured a patch — writes what it means and nothing else.
    pub fn new(kind: EditKind, body: impl Into<EditBody>) -> Self {
        Self {
            kind,
            at: Some(Timestamp::now()),
            body: body.into(),
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
    pub fn made_at(kind: EditKind, at: Option<Timestamp>, body: impl Into<EditBody>) -> Self {
        Self {
            kind,
            at,
            body: body.into(),
        }
    }

    /// The pixels this entry replaced, for the entries that hold any.
    pub fn patch(&self) -> Option<&PixelPatch> {
        match &self.body {
            EditBody::Pixels(patch) => Some(patch),
            EditBody::Structure(_) | EditBody::Flip => None,
        }
    }

    /// The same, as a slice.
    ///
    /// The form the multi-layer transform wants — one gesture moving a linked
    /// set records several patches in one entry, or an undo would step through
    /// it a layer at a time and leave the document in states it was never in.
    /// Nothing writes more than one yet, so this is `patch()` with the
    /// signature that will not have to change at the call sites when something
    /// does.
    pub fn patches(&self) -> &[PixelPatch] {
        match &self.body {
            EditBody::Pixels(patch) => std::slice::from_ref(patch),
            EditBody::Structure(_) | EditBody::Flip => &[],
        }
    }

    fn byte_len(&self) -> usize {
        self.body.byte_len()
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

/// The RGBA8 contents of one rectangle of a patch, tightly packed at
/// `width * 4` per row.
///
/// A patch is made of these rather than of one rectangle because a stroke's
/// bounding box is a very poor description of a stroke — see
/// [`crate::damage`], which is where the rectangles come from.
#[derive(Clone, Debug)]
pub struct PatchPiece {
    pub rect: PixelRect,
    bytes: PieceBytes,
}

/// How one piece's pixels are held.
#[derive(Clone, Debug, PartialEq, Eq)]
enum PieceBytes {
    /// Every pixel the same one — blank canvas, or a flat fill.
    ///
    /// Most of a layer is usually untouched, and a stroke laid on an empty one
    /// captures nothing but zeroes; storing four bytes for it is what makes an
    /// early session's history effectively free. The scan that finds this stops
    /// at the first pixel that differs, so a piece of busy painting pays four
    /// comparisons to be told it is not flat.
    Flat([u8; 4]),
    Raw(Vec<u8>),
}

impl PatchPiece {
    /// A piece of `rect`, from tightly packed RGBA8.
    pub fn new(rect: PixelRect, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len() as u64,
            rect.area() * 4,
            "piece byte count must match rect area"
        );
        let bytes = match flat_pixel(&bytes) {
            Some(pixel) => PieceBytes::Flat(pixel),
            None => PieceBytes::Raw(bytes),
        };
        Self { rect, bytes }
    }

    /// The pixels, tightly packed — expanded on the spot where the piece is a
    /// flat one.
    ///
    /// Borrowed in the ordinary case, so the write back to the GPU an undo
    /// performs copies nothing it does not have to.
    pub fn bytes(&self) -> Cow<'_, [u8]> {
        match &self.bytes {
            PieceBytes::Raw(bytes) => Cow::Borrowed(bytes),
            PieceBytes::Flat(pixel) => Cow::Owned(pixel.repeat(self.rect.area() as usize)),
        }
    }

    /// What this piece costs in memory, which is what the budget counts.
    pub fn byte_len(&self) -> usize {
        match &self.bytes {
            PieceBytes::Raw(bytes) => bytes.len(),
            PieceBytes::Flat(_) => 4,
        }
    }
}

/// The single pixel a tightly packed RGBA8 buffer is made of, if it is.
fn flat_pixel(bytes: &[u8]) -> Option<[u8; 4]> {
    let head: [u8; 4] = bytes.get(..4)?.try_into().ok()?;
    bytes.chunks_exact(4).all(|p| p == head).then_some(head)
}

/// The pixels a stroke replaced, as the rectangles it actually touched.
#[derive(Clone, Debug)]
pub struct PixelPatch {
    /// The whole region the stroke damaged — what the commit pass spans and
    /// what a list of the history would call the edit's extent. The pieces are
    /// inside it and generally do not fill it.
    pub rect: PixelRect,
    /// Texture-array slot this patch belongs to.
    ///
    /// Slots are recycled, so a patch replayed into a layer that merely
    /// inherited one would corrupt it. What keeps this valid is that a deleted
    /// layer travels into the [`EditBody::Structure`] entry that could put it
    /// back, **holding its slice** — see `crate::layer::SlotClaim` — so no other
    /// layer can be given the number while any entry names it.
    pub slot: u32,
    pieces: Vec<PatchPiece>,
}

impl PixelPatch {
    /// A patch covering the whole of `rect` in one piece.
    ///
    /// What a caller with no damage information has: a test, and a history read
    /// out of a document written before patches were made of pieces.
    pub fn new(rect: PixelRect, slot: u32, bytes: Vec<u8>) -> Self {
        Self {
            rect,
            slot,
            pieces: vec![PatchPiece::new(rect, bytes)],
        }
    }

    /// A patch of the pieces a stroke's damage mask named.
    ///
    /// `rect` is the stroke's whole damaged region; the pieces are the parts of
    /// it the dabs reached. Nothing checks that they are inside it — the same
    /// list goes to the commit pass, so if they were not, the pixels that were
    /// committed and the pixels that were recorded would be the same wrong set.
    pub fn from_pieces(rect: PixelRect, slot: u32, pieces: Vec<PatchPiece>) -> Self {
        Self { rect, slot, pieces }
    }

    pub fn pieces(&self) -> &[PatchPiece] {
        &self.pieces
    }

    /// What the patch costs in memory.
    ///
    /// The per-piece bookkeeping is counted too. A patch is normally a handful
    /// of megabytes and this is a few hundred bytes of it, but a session of
    /// thousands of tiny strokes is the case where the pixels stop being the
    /// bulk of what is held, and a budget that could not see that would be
    /// counting the wrong thing.
    pub fn byte_len(&self) -> usize {
        self.pieces.iter().map(PatchPiece::byte_len).sum::<usize>()
            + self.pieces.len() * std::mem::size_of::<PatchPiece>()
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
        Self::with_budget(default_budget())
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

    /// Change the ceiling of a history that already exists, and answer to it at
    /// once.
    ///
    /// The eviction is the whole point. Lowering the limit has to give the
    /// memory back now rather than at the next stroke — somebody who has just
    /// been told a session is using too much gets no relief from a promise about
    /// the next pointer-up. Raising it resurrects nothing: an entry the budget
    /// has already aged out is gone, and [`History::dropped`] still counts it,
    /// so the list goes on admitting it does not reach the beginning.
    ///
    /// Separate from [`set_default_budget`] because the two answer different
    /// questions — what a *new* document holds, and what the ones already open
    /// hold. The setting drives both.
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget_bytes = bytes;
        self.evict_to_budget();
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
    ///
    /// An entry that costs nothing — a flip — frees nothing when it goes, and
    /// that is correct rather than a hole in the loop: eviction is in timeline
    /// order, so a free entry older than the patch that is actually over the
    /// budget still has to go first. The loop advances regardless, because it
    /// removes an entry every pass and stops at one.
    fn evict_to_budget(&mut self) {
        self.evict_while(1, |h| h.used_bytes > h.budget_bytes);
    }

    /// Give the oldest entries up until the document has a texture slice to
    /// hand out again, and say whether it now has.
    ///
    /// **A structural entry holds a slice, so a history now competes with the
    /// live stack for them.** A deleted layer's slot is not returned while the
    /// entry that could put the layer back still names it, so a session of
    /// adding and deleting can walk the pool empty. When that happens the
    /// history gives a slot back — it does not refuse the layer. Entries are
    /// dropped oldest first, exactly as the budget drops them, and
    /// [`History::dropped`] counts them so the panel's existing "Earlier edits
    /// discarded" note already covers the case.
    ///
    /// Only when the history is empty **and** the live stack holds every slice
    /// is an operation genuinely refused, which is precisely the condition that
    /// refused it before any of this existed.
    ///
    /// `has_room` rather than a `&LayerStack`, because the history and the
    /// stack are separate fields of the editor and this is called while the
    /// history is being mutated. `crate::layer::SlotRoom` is what to pass.
    pub fn free_until(&mut self, has_room: impl Fn() -> bool) -> bool {
        // Oldest first, and down to nothing: unlike the budget, which keeps the
        // newest entry because there is always *some* memory for it, there is
        // no partial answer here — either a slice comes free or the operation
        // is refused.
        self.evict_while(0, |_| !has_room());
        if !has_room() {
            // Then the entries ahead of the cursor, **whole**. They are a run
            // in which each redo restores what the next expects, so one dropped
            // out of the middle is not a shorter run but a wrong one; and they
            // are not counted in `dropped`, which says how far short of the
            // document's *beginning* the list stops.
            for e in self.redo.drain(..) {
                self.used_bytes -= e.byte_len();
            }
        }
        has_room()
    }

    /// Drop the oldest undo entry while `over` says the history is too large,
    /// keeping at least `floor` of them.
    ///
    /// One loop with two stopping conditions rather than two loops: the budget
    /// and the slice ceiling age entries out the same way and in the same
    /// order, and a second copy would eventually disagree about `dropped`.
    fn evict_while(&mut self, floor: usize, over: impl Fn(&Self) -> bool) {
        while self.undo.len() > floor && over(self) {
            let e = self.undo.remove(0);
            self.used_bytes -= e.byte_len();
            self.dropped += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::{Layer, LayerStack};

    fn patch(w: u32, h: u32, fill: u8) -> PixelPatch {
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut bytes = vec![fill; (w * h * 4) as usize];
        // Not *every* byte the same. A patch whose pixels are all identical is
        // held as one pixel — see `PieceBytes::Flat` — so a budget test built
        // out of those would be measuring fifty bytes an entry and would pass
        // whatever the budget did.
        if let Some(last) = bytes.last_mut() {
            *last = fill.wrapping_add(1);
        }
        PixelPatch::new(rect, 0, bytes)
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
        assert_eq!(undone.patch().unwrap().pieces()[0].bytes()[0], 1);
        assert!(h.can_redo());

        let redone = h.take_redo().unwrap();
        assert_eq!(redone.patch().unwrap().pieces()[0].bytes()[0], 2);
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
        assert_eq!(
            h.take_undo().unwrap().patch().unwrap().pieces()[0].bytes()[0],
            7
        );
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
            assert_eq!(
                a.patch().unwrap().rect,
                b.patch().unwrap().rect,
                "entry {i}"
            );
            assert_eq!(
                a.patch().unwrap().slot,
                b.patch().unwrap().slot,
                "entry {i}"
            );
            assert_eq!(
                a.patch().unwrap().pieces()[0].bytes(),
                b.patch().unwrap().pieces()[0].bytes(),
                "entry {i}"
            );
        }

        // And the cursor is where it was: one redo available, two undos.
        assert_eq!(
            restored.take_redo().unwrap().patch().unwrap().pieces()[0].bytes()[0],
            9
        );
        assert_eq!(
            restored.take_undo().unwrap().patch().unwrap().pieces()[0].bytes()[0],
            2
        );
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

    /// The setting has to reach the documents already open, and the two
    /// directions fail differently. Lowering must give the memory back at once
    /// — the whole reason somebody turns it down — while raising must not
    /// resurrect an entry the old limit already discarded, because those pixels
    /// are not held anywhere any more and the count of them is what lets the
    /// list say it does not reach the document's beginning.
    #[test]
    fn changing_the_budget_evicts_at_once_and_never_resurrects() {
        let mut h = History::with_budget(100_000);
        for i in 0..8u8 {
            h.record(edit(16, 16, i));
        }
        assert_eq!(h.len(), 8, "nothing should have been dropped yet");
        assert_eq!(h.dropped(), 0);
        let full = h.used_bytes();

        // Down: the oldest go immediately, without waiting for another stroke.
        h.set_budget(2500);
        assert_eq!(h.budget_bytes(), 2500);
        assert!(h.used_bytes() <= 2500, "used {}", h.used_bytes());
        let kept = h.len();
        let lost = h.dropped();
        assert!(lost > 0, "lowering the budget dropped nothing");
        assert_eq!(kept + lost, 8);
        // Aged out from the bottom, so the newest is still the next undo.
        assert_eq!(
            h.entry_at(kept - 1).unwrap().patch().unwrap().pieces()[0].bytes()[0],
            7
        );

        // Up: the ceiling moves and nothing else does.
        h.set_budget(full * 4);
        assert_eq!(h.budget_bytes(), full * 4);
        assert_eq!(h.len(), kept, "raising the budget brought an entry back");
        assert_eq!(h.dropped(), lost, "the count of what was lost moved");
        assert!(h.used_bytes() < full);

        // And a raise is not a licence to exceed the new limit either: recording
        // still evicts against whatever was last set.
        h.set_budget(2500);
        h.record(edit(16, 16, 9));
        assert!(h.used_bytes() <= 2500, "used {}", h.used_bytes());
    }

    /// A document opened *after* the setting was changed has to take it too,
    /// which is the half `History::set_budget` cannot do — a blank document, a
    /// new tab and an import all build their own history and none of them can
    /// see the preferences.
    ///
    /// Raises the published value rather than lowering it, and puts it back:
    /// every other test in this process builds `History::default()`, and a
    /// smaller ceiling arriving underneath one of them would evict.
    #[test]
    fn a_history_built_after_the_setting_moved_takes_the_new_ceiling() {
        assert_eq!(History::default().budget_bytes(), default_budget());

        let raised = DEFAULT_BUDGET_BYTES * 2;
        set_default_budget(raised);
        assert_eq!(default_budget(), raised);
        assert_eq!(History::default().budget_bytes(), raised);
        // An explicit budget is still an explicit budget.
        assert_eq!(History::with_budget(64).budget_bytes(), 64);

        set_default_budget(DEFAULT_BUDGET_BYTES);
        assert_eq!(History::default().budget_bytes(), DEFAULT_BUDGET_BYTES);
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
        // The shipped default rather than `History::default()`'s, which a
        // preference can now move and which another test in this process may
        // therefore have moved.
        let budget = DEFAULT_BUDGET_BYTES;
        let patch = 10_000usize * 10_000 * 4;
        assert!(patch < budget, "not even one such stroke is held");
        assert!(
            patch * 2 > budget,
            "two now fit, so the panel's note names the wrong reason"
        );
    }

    // --- canvas flips ------------------------------------------------------

    /// A canvas small enough to write out, so a test can say exactly which byte
    /// went where.
    ///
    /// This exists because the claim a flip entry rests on is not about the
    /// history's bookkeeping at all — it is that stepping back over a flip and
    /// then over an older patch lands on the pixels that patch was recorded
    /// against. That is a statement about an ordering, and an ordering is
    /// testable without a GPU.
    #[derive(Clone, PartialEq, Eq, Debug)]
    struct Model {
        w: u32,
        h: u32,
        px: Vec<u8>,
    }

    impl Model {
        fn new(w: u32, h: u32) -> Self {
            Self {
                w,
                h,
                px: (0..w * h * 4).map(|i| i as u8).collect(),
            }
        }

        fn at(&self, x: u32, y: u32) -> [u8; 4] {
            let i = ((y * self.w + x) * 4) as usize;
            self.px[i..i + 4].try_into().unwrap()
        }

        /// Exactly what `flip.wgsl` does: a permutation of whole pixels.
        fn flip(&mut self, axis: FlipAxis) {
            let mut out = vec![0u8; self.px.len()];
            for y in 0..self.h {
                for x in 0..self.w {
                    let (sx, sy) = match axis {
                        FlipAxis::Horizontal => (self.w - 1 - x, y),
                        FlipAxis::Vertical => (x, self.h - 1 - y),
                    };
                    let d = ((y * self.w + x) * 4) as usize;
                    out[d..d + 4].copy_from_slice(&self.at(sx, sy));
                }
            }
            self.px = out;
        }

        /// Read a patch's rectangle back, which is what a commit captures.
        fn read(&self, rect: PixelRect) -> Vec<u8> {
            let mut out = Vec::with_capacity((rect.area() * 4) as usize);
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    out.extend_from_slice(&self.at(x, y));
                }
            }
            out
        }

        /// Write one back, which is what an undo does.
        fn write(&mut self, rect: PixelRect, bytes: &[u8]) {
            let mut i = 0;
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let d = ((y * self.w + x) * 4) as usize;
                    self.px[d..d + 4].copy_from_slice(&bytes[i..i + 4]);
                    i += 4;
                }
            }
        }

        /// Paint `rect` a flat colour, recording what was there — a stroke's
        /// commit, in miniature.
        fn paint(&mut self, rect: PixelRect, fill: u8) -> PixelPatch {
            let before = self.read(rect);
            self.write(rect, &vec![fill; before.len()]);
            PixelPatch::new(rect, 0, before)
        }
    }

    /// Step one entry backwards the way `app.rs` does, and hand back what
    /// putting it forward again would be.
    fn reverse(model: &mut Model, stack: &mut LayerStack, edit: Edit) -> Edit {
        let body = match edit.body {
            EditBody::Pixels(patch) => {
                let now = model.read(patch.rect);
                model.write(patch.rect, &patch.pieces()[0].bytes());
                EditBody::Pixels(PixelPatch::new(patch.rect, patch.slot, now))
            }
            // No pixels at all: the shape goes back and the shape that was
            // there comes out, holding whatever left the stack.
            EditBody::Structure(shape) => {
                EditBody::Structure(Box::new(stack.restore_shape(*shape)))
            }
            // The whole of it: a flip is undone by flipping.
            EditBody::Flip => {
                model.flip(edit.kind.flip_axis().expect("a flip entry names an axis"));
                EditBody::Flip
            }
        };
        Edit::made_at(edit.kind, edit.at, body)
    }

    /// The claim the flip design rests on, end to end.
    ///
    /// A flip records **no pixels**, so undoing one can only be flipping again
    /// — and that is sound only because the timeline is stepped rather than
    /// seeked. Stepping back over the flip puts the canvas into the orientation
    /// the older patch was recorded in, so that patch applies verbatim at the
    /// rectangle it names: no coordinate mapping, no mirrored bytes. Anything
    /// less than a byte-for-byte return would show up here, because the
    /// document is compared against the copy it started as.
    #[test]
    fn stepping_back_over_a_flip_puts_older_patches_where_they_were_recorded() {
        let start = Model::new(8, 6);
        let mut model = start.clone();
        let mut stack = LayerStack::new();
        let mut h = History::default();

        // Paint in a corner, so a flip plainly moves it.
        let mark = PixelRect {
            x: 0,
            y: 0,
            width: 3,
            height: 2,
        };
        h.record(Edit::new(EditKind::Paint, model.paint(mark, 200)));
        let after_paint = model.clone();

        model.flip(FlipAxis::Horizontal);
        h.record(Edit::new(EditKind::FlipHorizontal, EditBody::Flip));
        let after_flip = model.clone();
        assert_ne!(after_flip, after_paint, "the flip did nothing");

        // And a second mark, recorded in the *flipped* orientation.
        let second = PixelRect {
            x: 5,
            y: 4,
            width: 2,
            height: 2,
        };
        h.record(Edit::new(EditKind::Erase, model.paint(second, 9)));

        assert_eq!(h.position(), 3);
        assert_eq!(
            (h.kind_at(0), h.kind_at(1), h.kind_at(2)),
            (
                Some(EditKind::Paint),
                Some(EditKind::FlipHorizontal),
                Some(EditKind::Erase)
            )
        );

        // Back one: the second mark goes, the picture stays flipped.
        let e = h.take_undo().unwrap();
        h.push_redo(reverse(&mut model, &mut stack, e));
        assert_eq!(model, after_flip);

        // Back another: the flip is undone by flipping.
        let e = h.take_undo().unwrap();
        h.push_redo(reverse(&mut model, &mut stack, e));
        assert_eq!(model, after_paint, "undoing the flip did not put it back");

        // And back to the beginning, through a patch recorded before the flip
        // ever happened. This is the assertion the design exists for.
        let e = h.take_undo().unwrap();
        h.push_redo(reverse(&mut model, &mut stack, e));
        assert_eq!(model, start, "the older patch landed in the wrong pixels");

        // Forward again, by the same route, to exactly where it was.
        while let Some(e) = h.take_redo() {
            let back = reverse(&mut model, &mut stack, e);
            h.push_undo(back);
        }
        assert_eq!(h.position(), 3);
        let mut expected = after_flip.clone();
        expected.paint(second, 9);
        assert_eq!(model, expected, "redoing did not arrive where undoing left");
    }

    /// The timeline has to read the same through a flip as through anything
    /// else — the kinds in order, the cursor within them, and the entry keeping
    /// its own moment as it crosses between the stacks.
    #[test]
    fn a_flip_is_an_entry_like_any_other_in_the_timeline() {
        let at = |secs: i64| Some(Timestamp::from_unix_millis(secs * 1000));
        let mut h = History::default();
        h.record(Edit::made_at(EditKind::Paint, at(10), patch(4, 4, 1)));
        h.record(Edit::made_at(
            EditKind::FlipVertical,
            at(12),
            EditBody::Flip,
        ));
        h.record(Edit::made_at(EditKind::Paint, at(20), patch(4, 4, 2)));

        let before: Vec<_> = (0..h.len()).map(|i| h.kind_at(i)).collect();
        assert_eq!(h.gap_at(1), Some(Duration::from_secs(2)));
        assert_eq!(h.gap_at(2), Some(Duration::from_secs(8)));
        assert_eq!(h.steps_to(1), Jump::Undo(2));

        for _ in 0..2 {
            let e = h.take_undo().unwrap();
            h.push_redo(Edit::made_at(e.kind, e.at, e.body));
        }
        assert_eq!(h.position(), 1);
        assert_eq!(h.len(), 3, "undoing discarded something");
        assert_eq!(
            before,
            (0..h.len()).map(|i| h.kind_at(i)).collect::<Vec<_>>()
        );
        assert_eq!(h.time_at(1), at(12), "the flip lost the moment it was made");
        assert_eq!(h.steps_to(3), Jump::Redo(2));
        assert_eq!(
            EditKind::for_axis(FlipAxis::Vertical),
            EditKind::FlipVertical
        );
        assert_eq!(EditKind::FlipVertical.flip_axis(), Some(FlipAxis::Vertical));
        assert_eq!(EditKind::Paint.flip_axis(), None);
    }

    /// A flip stores nothing, so it costs nothing and the budget cannot be made
    /// to age one out on its own account. The list may hold as many as somebody
    /// has the patience to press.
    #[test]
    fn a_flip_entry_costs_the_budget_nothing() {
        let mut h = History::with_budget(2500);
        for _ in 0..1000 {
            h.record(Edit::new(EditKind::FlipHorizontal, EditBody::Flip));
        }
        assert_eq!(h.used_bytes(), 0);
        assert_eq!(h.len(), 1000, "a free entry was evicted");
        assert_eq!(h.dropped(), 0);

        // But it is still an entry, so eviction driven by the patches around it
        // takes it in timeline order like anything else — oldest first, and the
        // accounting stays balanced across it.
        for i in 0..8u8 {
            h.record(edit(16, 16, i));
        }
        assert!(h.used_bytes() <= 2500, "used {}", h.used_bytes());
        assert!(h.dropped() >= 1000, "the free entries outlived their turn");
        while h.take_undo().is_some() {}
        assert_eq!(h.used_bytes(), 0);
    }

    // --- structural entries -------------------------------------------------

    /// A CPU stand-in for a document: the layer stack, and one small canvas per
    /// texture slice.
    ///
    /// The claim structural undo rests on is not about the history's
    /// bookkeeping — it is that a slice parked in an undo entry is never handed
    /// to another layer, so a patch recorded against it goes on meaning the
    /// pixels it was captured from. That is a statement about *who holds which
    /// number*, and no GPU is needed to check it.
    struct Doc {
        stack: LayerStack,
        /// Indexed by slot. Four bytes a pixel, like the real thing.
        slices: Vec<Vec<u8>>,
        w: u32,
        h: u32,
    }

    impl Doc {
        fn new(w: u32, h: u32) -> Self {
            Self {
                stack: LayerStack::new(),
                slices: Vec::new(),
                w,
                h,
            }
        }

        fn slice(&mut self, slot: u32) -> &mut Vec<u8> {
            let blank = vec![0u8; (self.w * self.h * 4) as usize];
            while self.slices.len() <= slot as usize {
                self.slices.push(blank.clone());
            }
            &mut self.slices[slot as usize]
        }

        fn read(&mut self, slot: u32, rect: PixelRect) -> Vec<u8> {
            let w = self.w;
            let px = self.slice(slot).clone();
            let mut out = Vec::with_capacity((rect.area() * 4) as usize);
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let i = ((y * w + x) * 4) as usize;
                    out.extend_from_slice(&px[i..i + 4]);
                }
            }
            out
        }

        fn write(&mut self, slot: u32, rect: PixelRect, bytes: &[u8]) {
            let w = self.w;
            let px = self.slice(slot);
            let mut i = 0;
            for y in rect.y..rect.y + rect.height {
                for x in rect.x..rect.x + rect.width {
                    let d = ((y * w + x) * 4) as usize;
                    px[d..d + 4].copy_from_slice(&bytes[i..i + 4]);
                    i += 4;
                }
            }
        }

        /// Paint a rectangle of one slice flat, recording what was there — a
        /// stroke's commit, in miniature.
        fn paint(&mut self, slot: u32, rect: PixelRect, fill: u8) -> PixelPatch {
            let before = self.read(slot, rect);
            self.write(slot, rect, &vec![fill; before.len()]);
            PixelPatch::new(rect, slot, before)
        }

        /// Step one entry backwards the way `App::reverse` does.
        fn reverse(&mut self, edit: Edit) -> Edit {
            let body = match edit.body {
                EditBody::Pixels(patch) => {
                    let now = self.read(patch.slot, patch.rect);
                    let was = patch.pieces()[0].bytes().to_vec();
                    self.write(patch.slot, patch.rect, &was);
                    EditBody::Pixels(PixelPatch::new(patch.rect, patch.slot, now))
                }
                EditBody::Structure(shape) => {
                    EditBody::Structure(Box::new(self.stack.restore_shape(*shape)))
                }
                EditBody::Flip => EditBody::Flip,
            };
            Edit::made_at(edit.kind, edit.at, body)
        }
    }

    fn rect(x: u32, y: u32, w: u32, h: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    /// The claim the whole design rests on, end to end.
    ///
    /// A structural entry stores **no pixels**: the deleted layer travels
    /// inside it, holding its texture slice, and the slice goes on holding
    /// exactly the picture an undo would want to put back. That is only sound
    /// if two things hold, and both are asserted here — a layer added *after*
    /// the delete must not be given the parked number, and stepping back over
    /// the delete must put every older patch in the layer it was recorded in.
    ///
    /// The shape of `stepping_back_over_a_flip_puts_older_patches_where_they_
    /// were_recorded`, and for the identical reason: the stepped-not-seeked
    /// guarantee, one level up.
    #[test]
    fn stepping_back_over_a_delete_puts_older_patches_in_the_layer_they_were_recorded_in() {
        let mut doc = Doc::new(8, 6);
        let mut h = History::default();

        let bottom = doc.stack.active_slot().expect("a fresh stack has a layer");
        let victim = doc.stack.add().expect("room for a second layer");
        assert_ne!(bottom, victim);

        // Paint on both, recording each — the first is the patch that must
        // survive the delete above it.
        let mark = rect(0, 0, 3, 2);
        h.record(Edit::new(EditKind::Paint, doc.paint(bottom, mark, 200)));
        let bottom_after_paint = doc.read(bottom, mark);
        doc.paint(victim, mark, 111);
        let victim_pixels = doc.read(victim, mark);

        // Delete the top layer, holding it in the entry.
        let before = doc.stack.shape();
        let gone = doc.stack.remove_many(&[1]).expect("the top layer can go");
        h.record(Edit::new(EditKind::DeleteLayer, before.with_removed(gone)));
        assert_eq!(doc.stack.len(), 1);

        // **The parked slice is not handed out.** A new layer takes a fresh
        // number, so the deleted layer's pixels are still there to come back.
        let fresh = doc.stack.add().expect("room for another layer");
        assert_ne!(fresh, victim, "a parked slice was handed to a new layer");
        h.record(Edit::new(EditKind::AddLayer, doc.stack.shape()));
        doc.paint(fresh, mark, 42);
        h.record(Edit::new(EditKind::Paint, doc.paint(fresh, mark, 7)));

        assert_eq!(
            (0..h.len())
                .map(|i| h.kind_at(i).unwrap())
                .collect::<Vec<_>>(),
            vec![
                EditKind::Paint,
                EditKind::DeleteLayer,
                EditKind::AddLayer,
                EditKind::Paint,
            ]
        );

        // All the way back.
        while let Some(e) = h.take_undo() {
            let back = doc.reverse(e);
            h.push_redo(back);
        }

        assert_eq!(doc.stack.len(), 2, "the deleted layer did not come back");
        assert_eq!(
            doc.stack.get(1).and_then(Layer::slot),
            Some(victim),
            "it came back holding a different slice"
        );
        assert_eq!(
            doc.read(victim, mark),
            victim_pixels,
            "the parked slice lost its picture"
        );
        assert_eq!(
            doc.read(bottom, mark),
            vec![0; bottom_after_paint.len()],
            "the patch recorded before the delete landed in the wrong pixels"
        );

        // And forward again, to exactly where it was.
        while let Some(e) = h.take_redo() {
            let back = doc.reverse(e);
            h.push_undo(back);
        }
        assert_eq!(h.position(), 4);
        assert_eq!(doc.stack.len(), 2);
        assert_eq!(doc.read(bottom, mark), bottom_after_paint);
        assert_eq!(doc.read(fresh, mark), doc.read(fresh, mark));
    }

    /// The rule that would otherwise ship broken, and be found by a painter.
    ///
    /// A `Kept` row carries an id and a depth and **nothing else**, so undoing
    /// a move cannot revert a property changed after it. A snapshot of the whole
    /// `Vec` — the tempting shape, and one inverse rule for every operation
    /// there will ever be — would make an undo damage something it was never
    /// asked about.
    #[test]
    fn undoing_a_reorder_does_not_put_back_an_opacity_changed_since() {
        let mut doc = Doc::new(4, 4);
        doc.stack.add();
        doc.stack.add();
        let names: Vec<String> = doc.stack.layers().iter().map(|l| l.name.clone()).collect();

        let before = doc.stack.shape();
        assert!(doc.stack.reorder(0, 2), "bottom to top");
        let entry = Edit::new(EditKind::MoveLayer, before);

        // Afterwards, the artist drags an opacity and renames a layer.
        doc.stack.get_mut(1).unwrap().opacity = 0.25;
        doc.stack.get_mut(1).unwrap().name = "Shading".into();

        doc.reverse(entry);

        // Layer 3 was renamed while it sat in the middle, and the undo puts it
        // back on top — carrying the name and the opacity it has *now*.
        assert_eq!(
            doc.stack
                .layers()
                .iter()
                .map(|l| l.name.clone())
                .collect::<Vec<_>>(),
            vec![names[0].clone(), names[1].clone(), "Shading".to_string()],
            "the order did not come back, or a name travelled to the wrong row"
        );
        assert_eq!(
            doc.stack.get(2).unwrap().opacity,
            0.25,
            "undoing a move reverted an opacity set afterwards"
        );
    }

    /// A whole folder undoes in **one** entry and one step, with the stack
    /// never in a state it was not in.
    ///
    /// A per-operation inverse would have to delete and re-insert position by
    /// position, which is where `remove_many`'s reverse loop once deleted a
    /// layer nobody ticked. A recorded shape describes the whole stack, so
    /// there is no index arithmetic to get wrong.
    #[test]
    fn a_folder_deletion_undoes_in_one_step() {
        let mut doc = Doc::new(4, 4);
        doc.stack.add();
        doc.stack.add();
        let folder = doc.stack.group(&[1, 2]).expect("the top two");
        let shape = |s: &LayerStack| -> Vec<(String, u8, bool)> {
            s.layers()
                .iter()
                .map(|l| (l.name.clone(), l.depth, l.is_folder()))
                .collect()
        };
        let inside: Vec<u32> = doc.stack.layers()[1..3]
            .iter()
            .filter_map(Layer::slot)
            .collect();
        assert_eq!(inside.len(), 2);
        let was = shape(&doc.stack);

        let before = doc.stack.shape();
        let gone = doc.stack.remove_many(&[folder]).expect("the group can go");
        assert_eq!(gone.len(), 3, "the folder and both layers");
        let entry = Edit::new(EditKind::DeleteLayer, before.with_removed(gone));
        assert_eq!(doc.stack.len(), 1);

        // Both slices are still claimed, so nothing else can be given them.
        let fresh = doc.stack.add().expect("room");
        assert!(!inside.contains(&fresh), "a parked slice was reissued");
        doc.stack.remove_many(&[doc.stack.active_index()]);

        doc.reverse(entry);
        assert_eq!(
            shape(&doc.stack),
            was,
            "one step did not put the group back"
        );
        assert_eq!(
            doc.stack.layers()[1..3]
                .iter()
                .filter_map(Layer::slot)
                .collect::<Vec<_>>(),
            inside,
            "the layers came back holding different slices"
        );
    }

    /// A parked slice is given up when the entry holding it goes, and not
    /// before — which is what makes the ceiling shorten the history rather than
    /// refuse a layer.
    #[test]
    fn the_slot_ceiling_shortens_the_history_rather_than_refusing_a_layer() {
        let mut stack = LayerStack::new();
        let mut h = History::default();
        let room = stack.room();
        assert!(room.has_room());

        // Fill the pool by adding and deleting, parking every slice as we go.
        // Each pass claims one more slice than it gives back, because the
        // deleted layer travels into the entry.
        let mut parked = Vec::new();
        for _ in 0..LayerStack::MAX_SLOTS {
            if !room.has_room() {
                break;
            }
            let Some(slot) = stack.add() else { break };
            parked.push(slot);
            let before = stack.shape();
            let gone = stack
                .remove_many(&[stack.active_index()])
                .expect("the bottom layer stays");
            h.record(Edit::new(EditKind::DeleteLayer, before.with_removed(gone)));
        }
        assert!(!room.has_room(), "the pool never filled");
        assert!(stack.add().is_none(), "a full pool handed out a slice");
        let held = h.len();
        assert!(held > 1);

        // The history gives one back rather than the layer being refused, and
        // says how many it dropped so the panel can admit the list no longer
        // reaches the document's beginning.
        assert!(h.free_until(|| room.has_room()), "no slice came free");
        assert!(h.len() < held);
        assert!(h.dropped() > 0);
        assert!(stack.add().is_some(), "the released slice was not usable");
    }

    /// A slice is freed exactly once, when the last holder lets go — so an
    /// entry cloned for inspection cannot hand the number back twice, and a
    /// mask parked in an entry outlives the layer's own copy of the claim.
    #[test]
    fn a_slot_returns_to_the_pool_only_when_the_last_holder_lets_go() {
        let mut stack = LayerStack::new();
        let mask = stack
            .add_mask(0)
            .expect("a layer with no mask can gain one");
        let capacity = stack.slot_capacity_needed();

        // The shape clones the claim; the layer's own copy is then taken away.
        let parked = stack.shape_with_mask(0);
        assert!(stack.remove_mask(0).is_some());
        assert_eq!(
            stack.slot_capacity_needed(),
            capacity,
            "the mask's slice came free while an entry still named it"
        );
        assert_ne!(
            stack.add().expect("room"),
            mask,
            "a parked slice was reissued"
        );

        // A clone of the entry is not a second claim.
        let copy = parked.clone();
        drop(parked);
        assert_ne!(
            stack.add_mask(stack.active_index()).expect("room"),
            mask,
            "dropping one of two holders freed the slice"
        );
        drop(copy);
        // Now nothing names it, so the next taker gets it back.
        let mut freed = Vec::new();
        while let Some(slot) = stack.add() {
            freed.push(slot);
            if freed.len() > LayerStack::MAX_SLOTS as usize {
                break;
            }
        }
        assert!(
            freed.contains(&mask),
            "the last holder let go and nothing did"
        );
    }
}
