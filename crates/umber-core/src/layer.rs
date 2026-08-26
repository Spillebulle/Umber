//! The layer stack.
//!
//! # Slots
//!
//! A layer's pixels live in a fixed slice ("slot") of a GPU texture array,
//! assigned when the layer is created and never changed. The stack order is
//! just the order of this `Vec`, so reordering layers is a pointer shuffle
//! rather than 16 MB of texture copies per move.
//!
//! Slots are recycled when a layer is deleted — but not while anything still
//! *holds* one. That is [`SlotClaim`], and it is the whole of how deleting a
//! layer stopped clearing the undo history: the deleted [`Layer`] moves into
//! the history entry, the claim moves with it, and the slice it names is left
//! holding exactly the picture an undo would want to put back. See
//! [`StackShape`] and `docs/structural-undo.md`.
//!
//! # A layer has two identities, doing two different jobs
//!
//! The **slot claim** answers "which slice do these pixels live in". It is what
//! `PixelPatch` names, and holding one is what keeps a patch valid.
//!
//! [`Layer::id`] answers "which entry is this". A structural undo entry has to
//! be able to say "the entry that was at position 3 goes back to position 3",
//! and a slot cannot say it, because **a folder holds no slot**. Neither
//! identity can do the other's job.
//!
//! # A mask is a slot too
//!
//! [`Layer::mask`] is another slice of the **same** texture array, and that is
//! a deliberate choice rather than an economy. A dedicated single-channel array
//! would store a mask in a quarter of the memory, and would then need its own
//! banded readback, its own resize, its own flip, its own autosave capture, its
//! own undo patch width and its own history file revision — six paths to keep
//! in step with the six that already exist, for a saving on a texture most
//! documents never allocate at all. Sharing the array means a mask *is* a
//! layer as far as every one of those is concerned: `PixelPatch` needs no new
//! field, `read_layer_pieces` needs no new width, and a stroke on a mask
//! commits through the pipeline a stroke on a layer commits through.
//!
//! What it costs is four bytes per pixel where one would do — a masked layer is
//! two slices instead of one, which is exactly the arithmetic a painter already
//! does when they think of a mask as "another layer's worth of memory".
//!
//! The consequences are the same ones slots always had: a mask's slice is
//! claimed exactly as a layer's is, so taking a mask off parks the slice in the
//! history entry rather than putting it straight back on the free list; and a
//! mask slice is ordinary RGBA, of which the composite reads one channel.
//!
//! # A text layer is a layer that remembers what painted it
//!
//! [`Layer::text`] is a [`TextObject`] — a string, a face, the figures and the
//! placement — and a layer holding one is a **text layer**. It is an `Option` on
//! an ordinary layer rather than a third [`Layer`] kind beside a folder, and that
//! shape is the whole reason nothing else here changed: a text layer holds a
//! slot, composites through the same single pass, takes a mask, clips, links,
//! reorders, is deleted and comes back out of a structural undo entry exactly as
//! any other layer does, and [`LayerStack::MAX`] means what it always meant. The
//! record travels *inside* the [`Layer`], so a folder deleted with text in it
//! parks the text along with the slice and an undo brings both back, with
//! nothing written to make that happen.
//!
//! What it does need is one refusal. A text layer cannot also hold brush
//! strokes — the record would then describe pixels that are half somebody's
//! painting, and re-rendering it would destroy their work — so painting on one
//! is refused at [`LayerStack::refusal_at`], which is the **one gate** the lock
//! and the folder are already refused at. See [`EditRefusal`], and
//! `docs/text-tool.md` §3 for the argument.
//!
//! **Three of the four rules below are the model's half only, and nothing in
//! `umber-app` calls them yet.** The gate, the flip's mirror and the resize's
//! drop are each one call in one place, and each is named at its own method with
//! what goes wrong until it is made. That is the honest state of a wave-one
//! change: `docformat` and `docimport` are wired, so a record survives a save and
//! an open, and the three edits that would have to keep it in step do not yet ask.
//!
//! Two things follow that are easy to get backwards:
//!
//! * **A stroke on a text layer's *mask* is allowed.** A mask bounds the alpha
//!   the composite reads and changes not one of the layer's own pixels, so it
//!   cannot put the record out of step with them. That is why the gate takes an
//!   [`EditTarget`] rather than answering about a layer.
//! * **A canvas flip need not cost a text layer its record**, because
//!   [`crate::textobj::Placement::flipped`] mirrors the placement exactly.
//!   Dropping it would destroy something no undo could put back — undoing a flip
//!   is another flip — which is the failure `Selection::flipped` exists to avoid.
//!   A *resize* does drop it, for the reason a resize clears the undo history:
//!   the placement is a rectangle of a canvas that no longer exists, and the
//!   pixels may have been cropped.
//!
//! # Folders
//!
//! The stack is still one `Vec` and every existing caller still indexes it by
//! position. A folder is an *entry* in that `Vec` carrying no slot, and its
//! contents are the contiguous run of entries **immediately below it** whose
//! [`Layer::depth`] is greater than its own. Two shapes were considered and a
//! real tree (`enum Node { Layer(..), Folder(.., Vec<Node>) }`) reads better and
//! breaks everything: `get`, `active_index`, `reorder`, `remove`, the app's
//! `layer_draws`, the autosave's snapshot, `SaveHistory`'s position mapping and
//! `layerdrag`'s rows all take a flat index today. A depth on a flat list keeps
//! every one of them.
//!
//! **A folder sits above its own contents, not below them**, which is the one
//! thing here that is easy to get backwards. It is what a layers panel draws —
//! the group's row is above the layers in it — it is the order ORA's nested
//! `<stack>` writes, since the first element of a stack is the uppermost; and it
//! makes the folder entry the natural *end* of its own group as the composite
//! walks bottom to top. So a folder's subtree ends at the folder and begins at
//! the lowest entry of the run beneath it, and
//! [`LayerStack::subtree`] is the one place that is computed.
//!
//! Every folder in this build is **pass-through**: a container, whose visibility
//! and lock reach its contents and whose opacity and blend mode do not exist.
//! That is not a simplification of the feature so much as the whole of the
//! cheap half of it — a pass-through folder is *exactly* its contents
//! composited in place, which is why `composite.wgsl` was not touched, why
//! `umber-version` did not move, and why an older Umber (or GIMP, or MyPaint)
//! flattening the nesting away shows the identical picture. See
//! `docs/layer-folders.md`. A folder with an opacity of its own is group
//! compositing and needs all three of those; the controls for it are
//! deliberately not drawn until it does.
//!
//! ## Well-formedness
//!
//! Not every sequence of depths describes a tree, and the one that does not is
//! a layer nested inside no folder.
//!
//! The two mutations that could produce one — [`LayerStack::reorder_to`] and
//! [`LayerStack::group`] — build the depth sequence they *would* produce and
//! run [`well_formed`] over it **before** committing, so a refusal changes
//! nothing at all. The stack is at most [`LayerStack::MAX`] entries, so that
//! costs nothing. Each has a `can_` beside it sharing the same plan, which is
//! what lets a button or a drag ask whether an operation will happen rather
//! than offering it and being refused.
//!
//! The rest — `add`, `remove_many`, `flatten_ill_formed` — cannot produce a
//! malformed stack by construction: one inserts at a depth it derives from a
//! neighbour, one removes whole subtrees, and one exists to straighten a
//! sequence. They carry a `debug_assert` rather than a gate, which is a
//! statement that the argument is expected to hold and not a check that runs
//! in a release build.

use std::sync::{Arc, Mutex};

use glam::UVec2;
use serde::{Deserialize, Serialize};

use crate::color::Color;
use crate::effect::{self, Effect, EffectKind};
use crate::geom::FlipAxis;
use crate::textobj::TextObject;

/// The slices of one document's layer array: which are in use, and which
/// number the next one would take.
///
/// Private, and reached only through [`SlotClaim`] and [`SlotRoom`], because
/// the one thing that must never happen is a number handed to two layers.
#[derive(Debug, Default)]
struct SlotPool {
    /// Numbers given back and not yet handed out again, **ascending** — see
    /// [`SlotPool::give_back`], which relies on the order to compact the tail.
    free: Vec<u32>,
    /// One past the highest number currently claimed.
    next: u32,
}

impl SlotPool {
    /// Could this pool hand a slice out?
    fn has_room(&self) -> bool {
        !self.free.is_empty() || self.has_headroom()
    }

    /// Is there a slice number *above everything claimed*?
    ///
    /// A different question from [`SlotPool::has_room`], and the two genuinely
    /// diverge: a pool holding a gap in the middle can hand a slice out and
    /// still have nothing above the top. That is exactly what a floating
    /// transform needs, because `CanvasRenderer::begin_float` reserves its
    /// preview at [`LayerStack::slot_capacity_needed`] — above every claim by
    /// construction, which is what stops a float rendering into a deleted
    /// layer's parked pixels.
    fn has_headroom(&self) -> bool {
        self.next < LayerStack::MAX_SLOTS
    }

    /// Hand out the **lowest** free number, or the next one up.
    ///
    /// Lowest rather than highest, which is what a bare `pop` off an ascending
    /// list would give: taking from the top re-claims the end of the range and
    /// keeps low numbers on the free list, so `next` can never fall again.
    /// Taking from the bottom leaves the high end free for [`SlotPool::
    /// give_back`] to compact away, and the linear removal is over a list of at
    /// most [`LayerStack::MAX_SLOTS`] numbers on a path that runs once per new
    /// layer.
    fn take(&mut self) -> Option<u32> {
        if !self.free.is_empty() {
            return Some(self.free.remove(0));
        }
        if !self.has_headroom() {
            return None;
        }
        let n = self.next;
        self.next += 1;
        Some(n)
    }

    /// Reach the pool, **recovering from a poisoned lock rather than failing**.
    ///
    /// Poisoning means some thread panicked while holding it. The operations
    /// inside are a push, a sort and a pop of a `Vec<u32>` with nothing in them
    /// that can panic, so there is no half-written state to protect anybody
    /// from — and every alternative is worse in a different direction: failing
    /// closed loses a slice for ever, and failing open once had
    /// `slot_capacity_needed` answering `MAX_SLOTS`, which would ask the
    /// renderer for 256 canvas-sized slices.
    fn locked(pool: &Mutex<SlotPool>) -> std::sync::MutexGuard<'_, SlotPool> {
        pool.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Take a number back, and **compact the tail** so `next` is one past the
    /// highest slice still claimed.
    ///
    /// The compaction is not tidiness. `next` is what
    /// [`LayerStack::slot_capacity_needed`] reports, which is what the renderer
    /// allocates to and what `CanvasRenderer::begin_float` reserves its preview
    /// slice at; and `ensure_slots` **never shrinks**. So a `next` left high is
    /// a texture array left large, for the rest of the session. (It doubles
    /// only while the array is cheap — see `grown_capacity` — but that bounds
    /// the *overshoot*, not this.)
    ///
    /// It is not on its own enough, and the two things beside it are worth
    /// naming here because each looks redundant next to this one:
    ///
    /// * Compaction only fires when the **highest** claim is released, so a
    ///   parked slice below a live one holds the number up regardless.
    ///   [`StackShape::byte_len`] charging the undo budget for a parked slice
    ///   is what actually bounds how many there are.
    /// * A pool holding a gap in the middle can hand a slice out and still have
    ///   nothing above the top, which is what [`SlotPool::has_headroom`] is for.
    ///
    /// The array never shrinking is also why a capacity that falls and rises
    /// again costs nothing: `ensure_slots` returns at once.
    fn give_back(&mut self, n: u32) {
        self.free.push(n);
        self.free.sort_unstable();
        while self.next > 0 && self.free.last() == Some(&(self.next - 1)) {
            self.free.pop();
            self.next -= 1;
        }
    }
}

/// A slice of the layer array, held for as long as anything names it.
///
/// `Drop` is what returns the number to the pool, which is why this is the only
/// way to hold one: a `free_slots.push` beside each of the places a layer can
/// leave the stack is the "forgotten at the sixth" failure written out in
/// advance, and the failure it produces — one slice handed to two layers — is
/// silent corruption of somebody's painting.
///
/// **Cloning shares the claim** rather than duplicating it, which is the
/// semantics a snapshot wants and the only safe one: two independent claims on
/// one number would give it back twice. The slice is freed when the last holder
/// lets go.
#[derive(Clone, Debug)]
pub struct SlotClaim(Arc<Claim>);

/// The claim itself. Separate from [`SlotClaim`] so that `Drop` runs once, when
/// the last clone goes, rather than once per clone.
#[derive(Debug)]
struct Claim {
    number: u32,
    pool: Arc<Mutex<SlotPool>>,
}

impl SlotClaim {
    /// The slice this claim names.
    ///
    /// No lock: the number is fixed for the claim's life, so every existing
    /// caller and the whole drawing path read it as cheaply as they read a
    /// `u32` field. Only `Drop` takes the lock.
    pub fn number(&self) -> u32 {
        self.0.number
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // Never `unwrap`: panicking in a `Drop` while something else is
        // already unwinding is an abort. `SlotPool::locked` recovers from a
        // poisoned lock rather than declining, so a slice is not leaked either.
        SlotPool::locked(&self.pool).give_back(self.number);
    }
}

/// "Has this document a slice to hand out?", answerable without borrowing the
/// stack.
///
/// It exists for one caller: the undo history has to be able to give an old
/// entry's parked slice back when a new layer needs one, and it cannot be
/// handed a `&LayerStack` because the two are separate fields of the editor and
/// the history is being mutated at the time. Read-only by construction, so it
/// cannot become a second way to take a slot.
#[derive(Clone, Debug)]
pub struct SlotRoom(Arc<Mutex<SlotPool>>);

impl SlotRoom {
    /// Could the document hand a slice out — to a layer, or to a mask?
    pub fn has_room(&self) -> bool {
        SlotPool::locked(&self.0).has_room()
    }

    /// Is there a slice above everything claimed, which is what a floating
    /// transform's preview takes?
    ///
    /// **Not the same question as [`SlotRoom::has_room`]**, and asking the
    /// wrong one is a release valve that never opens: a pool with a gap in the
    /// middle answers yes to that and no to this, so a history giving entries
    /// up "until there is room" would stop at once and leave the transform tool
    /// refusing on a document with a handful of layers.
    pub fn has_headroom(&self) -> bool {
        SlotPool::locked(&self.0).has_headroom()
    }
}

/// What a stroke on the active layer lands in.
///
/// Per *document* rather than per layer: it is a statement about what the
/// painter is doing now, and it follows them from layer to layer the way the
/// brush in their hand does. It reaches the renderer as
/// `StrokeStyle::on_mask`, which preview and commit are both handed — the same
/// arrangement that stops those two disagreeing about anything else.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditTarget {
    #[default]
    Layer,
    Mask,
}

/// Why an edit that would write pixels is refused.
///
/// **One answer rather than three booleans**, because a gate that has to ask
/// three questions is a gate that will one day ask two. There were already two —
/// `begin_stroke` refuses a locked layer and refuses a folder — and text is the
/// third; asking [`LayerStack::refusal_at`] is what makes them one test with one
/// reason, which is also what lets the interface say *which* it was.
///
/// An `enum` and not a `bool`, matched exhaustively wherever it is turned into
/// words, so a further refusal cannot be added without something being said
/// about it. `matches!` over this is exactly what the rule about partial
/// exhaustiveness forbids: it answers **false** for a variant it has never heard
/// of, which here means letting a stroke through a gate that exists to stop it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditRefusal {
    /// A folder holds no pixels, so there is nowhere for the edit to land.
    Folder,
    /// The layer is locked, by its own flag or by a folder it is inside.
    Locked,
    /// The layer holds text that can still be edited, so painting on it would
    /// leave a record describing pixels it did not make.
    Text,
    /// There is no entry at that position.
    ///
    /// Not a state a caller should be able to reach — the selected index is
    /// always valid and a row of the list carries its own — and it is a
    /// **refusal** rather than a permission anyway, because this is a gate.
    /// Answering `None` for an index off the end would make one `Option` carry
    /// both "go ahead" and "no such layer", on the one function whose whole
    /// purpose is to say no.
    Missing,
}

impl EditRefusal {
    /// Every refusal.
    ///
    /// **Guarded by an exhaustive match in a test rather than by iterating
    /// itself**, which is the rule this codebase learned from a hand-written
    /// `[EditKind; 11]` that still compiled at the wrong length: a test that
    /// walks this array can only ever check what is already in it. See
    /// `every_refusal_is_named_in_the_all_array`, which also names the hole that
    /// remedy still leaves.
    pub const ALL: [EditRefusal; 4] = [Self::Folder, Self::Locked, Self::Text, Self::Missing];

    /// What to tell somebody who asked for an edit this refuses.
    ///
    /// A finished sentence written for the user, and deliberately not one that
    /// names a control: this module cannot know what the interface calls the
    /// way out, and a sentence naming a button is one that goes stale silently
    /// when the button is renamed. The Text panel's "Convert to paint" —
    /// [`LayerStack::take_text`]'s caller — is named in `app.rs`'s own wording
    /// for a refused paste and a refused cut, where the button is in view.
    ///
    /// Exhaustive with no catch-all, so a further variant fails the build here
    /// rather than going out as a blank line.
    pub fn reason(self) -> &'static str {
        match self {
            Self::Folder => "A folder holds no pixels, so there is nothing here to paint on.",
            // **Both halves, because a lock reaches down from a folder.**
            // `refusal_at` reads `effective_locked`, so this is shown for a layer
            // whose own padlock is open and whose folder's is shut; naming only
            // the layer would send somebody to unlock the wrong row.
            Self::Locked => "This layer is locked, or it is inside a locked folder.",
            // "Painted on" alone is too narrow: the same gate refuses a paste
            // and a lift, and neither is painting.
            Self::Text => {
                "This layer holds text that can still be edited, so nothing may be \
                 painted or pasted onto it."
            }
            Self::Missing => "That layer is no longer there.",
        }
    }
}

/// How a layer combines with everything beneath it.
///
/// The numeric values are consumed directly by `composite.wgsl`; keep them in
/// step with the `switch` in `blend_rgb`.
///
/// Serialised because a *brush* carries one too — see [`crate::Brush::blend`] —
/// and a brush is what a preset file holds. **An [`Effect`] now carries one as
/// well, into a *document*** rather than into a preset, which is what makes the
/// serde spelling of these five variants an `.ora`'s business and not only
/// `brushes.ron`'s; `effect::tests::the_serialised_names_of_a_blend_mode_are_
/// these_exact_strings` is what pins it, and `docformat::blend_id`'s pin cannot,
/// because that one is the `Debug` spelling and a `#[serde(rename)]` moves only
/// the other. Deliberately the same enum rather
/// than a second one beside it: the arithmetic is one shared WGSL function, so
/// a layer set to Multiply and a brush set to Multiply mean the same thing, and
/// two enums would eventually stop agreeing about which modes exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    // **The first five discriminants may not move.** They are what
    // `composite.wgsl` switches on, and while the *number* is never written to
    // a file, keeping them fixed means a shader and a document written before
    // the set grew still mean the same thing. Everything added since is
    // appended, which is what makes growing the set a safe change.
    #[default]
    Normal = 0,
    Multiply = 1,
    Screen = 2,
    Overlay = 3,
    /// Photoshop and Clip Studio both call this **Linear Dodge (Add)**, and it
    /// is the same operation — which is why `blend::nearest` reads their
    /// `linear-dodge` as this one *exactly* rather than approximately.
    Add = 4,
    Darken = 5,
    Lighten = 6,
    ColorDodge = 7,
    ColorBurn = 8,
    LinearBurn = 9,
    HardLight = 10,
    SoftLight = 11,
    VividLight = 12,
    LinearLight = 13,
    PinLight = 14,
    Difference = 15,
    Exclusion = 16,
    Subtract = 17,
    Divide = 18,
    // The four **non-separable** modes: each channel of the result depends on
    // all three of the inputs, so they need the hue/saturation/luminosity
    // helpers in `blend.wgsl` rather than one line of arithmetic.
    Hue = 19,
    Saturation = 20,
    Color = 21,
    Luminosity = 22,
    /// Clip Studio's **Add (Glow)**, which is not a blend function at all.
    ///
    /// It is Porter-Duff `plus` — a straight addition of premultiplied colour —
    /// so it changes the compositing step rather than supplying a `B(Cb, Cs)`,
    /// and `blend.wgsl` handles it in `composite_over` with no `blend_rgb` arm.
    /// OpenRaster's `svg:plus` names exactly this operator, so it is the one
    /// non-SVG-named mode that nonetheless round-trips through an `.ora`
    /// exactly.
    ///
    /// **It agrees with [`Self::Add`] wherever the backdrop is opaque or
    /// empty**, which is derived in `composite_over`'s own note, so the two
    /// differ only at a soft edge.
    AddGlow = 23,
}

impl BlendMode {
    /// Every mode, in the order the interface offers them.
    ///
    /// **Grouped by what they do to the picture** — darkening, lightening,
    /// contrast, comparison, colour — which is how Photoshop, Clip Studio and
    /// Krita all arrange the same list, and therefore what somebody coming from
    /// one of those is looking for. Deliberately *not* discriminant order: the
    /// numbers are fixed by the shader and by what was here first, and sorting
    /// the menu by them would put Add between Overlay and Darken for no reason
    /// a painter could see.
    pub const ALL: [BlendMode; 24] = [
        Self::Normal,
        // Darken
        Self::Darken,
        Self::Multiply,
        Self::ColorBurn,
        Self::LinearBurn,
        // Lighten
        Self::Lighten,
        Self::Screen,
        Self::ColorDodge,
        Self::Add,
        Self::AddGlow,
        // Contrast
        Self::Overlay,
        Self::SoftLight,
        Self::HardLight,
        Self::VividLight,
        Self::LinearLight,
        Self::PinLight,
        // Comparison
        Self::Difference,
        Self::Exclusion,
        Self::Subtract,
        Self::Divide,
        // Colour
        Self::Hue,
        Self::Saturation,
        Self::Color,
        Self::Luminosity,
    ];

    /// What the interface calls it.
    ///
    /// British spelling, as every user-facing string here is — so the label is
    /// "Colour Dodge" where the variant is `ColorDodge`. The variant keeps the
    /// American spelling because **its name is a file format**: it is what
    /// serde writes into `brushes.ron` and into an effect record, and renaming
    /// it would break every preset that carries one. See the type's own note.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Add => "Add",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorDodge => "Colour Dodge",
            Self::ColorBurn => "Colour Burn",
            Self::LinearBurn => "Linear Burn",
            Self::HardLight => "Hard Light",
            Self::SoftLight => "Soft Light",
            Self::VividLight => "Vivid Light",
            Self::LinearLight => "Linear Light",
            Self::PinLight => "Pin Light",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::Divide => "Divide",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Color => "Colour",
            Self::Luminosity => "Luminosity",
            Self::AddGlow => "Add (Glow)",
        }
    }

    pub fn index(self) -> u32 {
        self as u32
    }
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub visible: bool,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub blend: BlendMode,
    /// Bounded by the alpha of the nearest **unclipped** layer below.
    ///
    /// A run of clipped layers therefore all answer to the same base, which is
    /// what every other application means by the word and what the composite
    /// loop implements — see `composite.wgsl`. A clipped layer with nothing
    /// unclipped beneath it shows nothing: there is no base for it to be bound
    /// by, and inventing one would make the flag mean something different at
    /// the bottom of the stack than it does anywhere else.
    pub clipped: bool,
    /// Refuses every edit until it is unlocked.
    ///
    /// Read through [`LayerStack::locked_at`] at the *one* gate each operation
    /// has, never at the call sites that reach it — see that method.
    pub locked: bool,
    /// Which link group the layer belongs to, if any.
    ///
    /// A *group*, not a flag: several independent sets can exist at once, each
    /// drawn in its own colour on the rows that belong to it, so "these three
    /// move together" and "those two move together" are two different
    /// statements rather than one set of five. Numbered rather than named
    /// because the number is only ever a key — into
    /// [`LayerStack::LINK_GROUPS`]'s worth of palette colours, and into the
    /// file. Written through [`LayerStack::link`] and [`LayerStack::unlink`].
    pub link: Option<u8>,
    /// Ticked in the list, for an operation that is about to be done to
    /// several layers at once.
    ///
    /// **Deliberately not written to the file.** Every other flag here is a
    /// property of the picture; this one is a statement about what the painter
    /// is *about to do*, and reopening a document to find four layers still
    /// ticked from last week would be an instruction nobody gave. It is a field
    /// rather than a set held beside the stack because a set would have to be
    /// keyed by slot and then kept in step with reordering and deletion by
    /// hand — as a field both come free, which is the same argument
    /// [`Layer::link`] makes for itself.
    ///
    /// Read through [`LayerStack::targets`], never off the field: what a bulk
    /// operation reaches is one rule and it has one place.
    pub picked: bool,
    /// How deeply this entry is nested. 0 is the top level.
    ///
    /// Public to read and write freely only because every *structural* change
    /// goes through [`LayerStack`], which validates the whole sequence: see the
    /// module docs. Nothing outside this module should assign it.
    pub depth: u8,
    /// A folder folded shut, so the list draws its row and not its contents.
    ///
    /// Purely a property of the list, and deliberately so: a collapsed folder
    /// composites exactly as an open one does, which is what stops a fold being
    /// something that can change the picture. It is *not* written to the file
    /// for the reason [`Layer::picked`] is not — reopening a document to find
    /// somebody's folders shut the way they left them last week is a state
    /// nobody asked for, and unlike a tick it is one they would have to undo
    /// before they could see their own painting.
    pub collapsed: bool,
    /// This entry is a folder: it holds no pixels, and owns the run of entries
    /// immediately below it whose [`Layer::depth`] is greater than its own.
    folder: bool,
    /// Which entry this is, for as long as the document is open.
    ///
    /// From a per-document counter, never recycled, and **never written to the
    /// file**: it is an identity within one session, which is all a structural
    /// undo entry needs. A folder has one, which is the whole reason it exists
    /// — see the module docs.
    id: u32,
    /// Texture-array slice holding this layer's pixels. Stable for the layer's
    /// lifetime, and `None` for a folder.
    ///
    /// An `Option` rather than a slice a folder holds and nothing writes to,
    /// because the second is a lie that the autosave would find: it reads every
    /// slot back, so a folder holding one would be written to the file as a
    /// blank layer nobody made — and on a large canvas it would cost 400 MB of
    /// texture per folder for the privilege.
    slot: Option<SlotClaim>,
    /// Slice holding this layer's mask, when it has one. Another slot of the
    /// same array — see the module docs.
    mask: Option<SlotClaim>,
    /// What set this layer's pixels, where they were set rather than painted.
    ///
    /// A layer holding one is a **text layer**: its pixels can be produced again
    /// from the record, so the string, the face and the size are editable rather
    /// than gone. Painting on it is refused — see [`LayerStack::refusal_at`] —
    /// because a record describing pixels that are half somebody's brushwork is
    /// a re-render that destroys their work.
    ///
    /// `Box`ed because it holds three `String`s and a [`crate::TextBlock`] and
    /// nearly every layer has none: [`Layer`] is cloned by
    /// [`LayerStack::restore_shape`] and lives in `Vec`s that are shuffled on
    /// every reorder, and one pointer is what that costs a layer without text.
    ///
    /// **Not written to the file from here.** `docformat` writes it as its own
    /// archive entry under `umber/text/`, fingerprinted against the layer's PNG;
    /// see [`crate::textobj`] for why the fingerprint exists and why it is not a
    /// field of the record in memory.
    text: Option<Box<TextObject>>,
    /// Non-destructive marks derived from this layer's own alpha — a stroke, a
    /// drop shadow. See [`crate::effect`] and `docs/layer-effects.md`.
    ///
    /// **Private, and written only through [`LayerStack::set_effect`] and
    /// [`LayerStack::remove_effect`]**, because it carries two invariants no
    /// caller should have to remember: at most one effect per kind, and always
    /// in composite order. A `pub` field is one `push` away from a layer wearing
    /// two drop shadows and a draw list in an order nobody chose — and the cap
    /// in [`effect::MAX_ENABLED`] is a property of the *document*, which a layer
    /// cannot see, so the gate could not live here even if the field were
    /// public.
    ///
    /// Empty for every layer that has none, which is nearly all of them, and an
    /// empty `Vec` allocates nothing.
    effects: Vec<Effect>,
}

impl Layer {
    /// Which entry this is. See the field.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The slice holding this layer's pixels, or `None` for a folder.
    pub fn slot(&self) -> Option<u32> {
        self.slot.as_ref().map(SlotClaim::number)
    }

    /// This entry is a folder and holds no pixels.
    pub fn is_folder(&self) -> bool {
        self.folder
    }

    /// The slice holding this layer's mask, if it has one.
    pub fn mask(&self) -> Option<u32> {
        self.mask.as_ref().map(SlotClaim::number)
    }

    pub fn has_mask(&self) -> bool {
        self.mask.is_some()
    }

    /// What set this layer's pixels, where it was set rather than painted.
    pub fn text(&self) -> Option<&TextObject> {
        self.text.as_deref()
    }

    /// Is this a text layer — one whose pixels can be produced again from a
    /// record?
    pub fn is_text(&self) -> bool {
        self.text.is_some()
    }

    /// Roughly what the record costs, for [`StackShape::byte_len`]. Zero for a
    /// layer with none, which is nearly all of them.
    pub fn text_bytes(&self) -> usize {
        self.text.as_ref().map_or(0, |t| t.byte_len())
    }

    /// This layer's effects, **already in composite order**, bottom to top.
    ///
    /// Read-only: see the field for why there is no `effects_mut`. Enabled and
    /// disabled alike, because the panel draws a row for both.
    pub fn effects(&self) -> &[Effect] {
        &self.effects
    }

    /// This layer's effect of `kind`, if it has one.
    pub fn effect(&self, kind: EffectKind) -> Option<&Effect> {
        self.effects.iter().find(|e| e.kind == kind)
    }

    /// How many of this layer's effects would produce a draw.
    ///
    /// The unit [`effect::MAX_ENABLED`] is counted in, which is why it is the
    /// layer's own arithmetic rather than something the stack open-codes.
    pub fn enabled_effect_count(&self) -> usize {
        self.effects.iter().filter(|e| e.enabled).count()
    }

    /// The enabled effects that composite **under** this layer, bottom to top.
    ///
    /// Nothing reads this yet — stage 0 emits no draws — and it is here because
    /// it is the half of `docs/layer-effects.md` §4 that is a rule rather than a
    /// rendering, so it belongs where it can be tested without a device.
    pub fn effects_below(&self) -> impl Iterator<Item = &Effect> {
        self.effects.iter().filter(|e| e.enabled && e.is_outer())
    }

    /// The enabled effects that composite **over** this layer, bottom to top.
    /// See [`Layer::effects_below`].
    pub fn effects_above(&self) -> impl Iterator<Item = &Effect> {
        self.effects.iter().filter(|e| e.enabled && e.is_inner())
    }

    /// A fresh layer holding `slot`, with every flag at the value a layer
    /// nobody has touched has.
    ///
    /// One constructor rather than two struct literals, so a field added here
    /// cannot be given one default by `LayerStack::new` and another by
    /// `LayerStack::add`.
    fn named(name: &str, id: u32, slot: Option<SlotClaim>) -> Self {
        Self {
            name: name.to_string(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            clipped: false,
            locked: false,
            link: None,
            picked: false,
            depth: 0,
            collapsed: false,
            folder: false,
            id,
            slot,
            mask: None,
            text: None,
            effects: Vec::new(),
        }
    }

    /// A fresh empty folder.
    ///
    /// Built through [`Layer::named`] so that a field added to a layer cannot
    /// be given one default here and another there — the same argument
    /// `named` itself makes. What a folder then overrides is exactly the two
    /// things that make it one, and `opacity` and `blend` are left at the
    /// values a pass-through folder means: no fade and no mode. They are not
    /// drawn, and until group compositing exists nothing reads them.
    fn folder_named(name: &str, id: u32) -> Self {
        Self {
            folder: true,
            ..Self::named(name, id, None)
        }
    }
}

/// Does this sequence of `(depth, is folder)`, bottom first, describe a tree?
///
/// The rule read from the **top** down, which is the direction nesting is
/// declared in: an entry may be one level deeper than the last thing that could
/// enclose it, and only a folder can enclose anything. So a folder at depth `d`
/// permits what follows it to reach `d + 1`, and an ordinary layer at depth `d`
/// permits only `d` — which is precisely "a layer nested inside nothing is not a
/// state", the one malformed shape a `Vec` of depths can otherwise hold.
///
/// Depth is bounded too. [`LayerStack::MAX_DEPTH`] is not a limit the model
/// needs — it is what a bounded group stack in a fragment shader will need when
/// folders gain an opacity of their own, and a document nested deeper than that
/// has to be refused where somebody can be told, not in a shader with nowhere to
/// report it.
pub fn well_formed(entries: &[(u8, bool)]) -> bool {
    let mut allowed = 0u8;
    for (depth, folder) in entries.iter().rev() {
        if *depth > allowed || *depth > LayerStack::MAX_DEPTH {
            return false;
        }
        allowed = if *folder { *depth + 1 } else { *depth };
    }
    true
}

/// What [`LayerStack::add_named`] hands back.
///
/// Two numbers that answer different questions and are easy to confuse, which
/// is why they travel together rather than as a bare `u32`. The **slot** is a
/// slice of the texture array and is what the caller must clear on the GPU; the
/// **id** is the entry's identity and is the only one of the two that a caller
/// may hold across a gesture, because a slot says nothing about where the layer
/// sits and an index stops meaning this layer the moment anything is reordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddedLayer {
    /// The texture slice the layer took. Clear it: a recycled slot still holds
    /// the last layer's pixels.
    pub slot: u32,
    /// [`Layer::id`], for a caller that has to find this entry again later.
    pub id: u32,
}

/// Bottom-to-top stack of layers. Index 0 is the bottom.
#[derive(Debug)]
pub struct LayerStack {
    layers: Vec<Layer>,
    active: usize,
    /// The document's slices. Shared with every [`SlotClaim`] this stack has
    /// handed out, including the ones parked in undo entries.
    pool: Arc<Mutex<SlotPool>>,
    /// Next [`Layer::id`]. Never recycled, so an entry that has left the stack
    /// and comes back is still the same entry.
    next_id: u32,
}

impl Default for LayerStack {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerStack {
    /// How many **stack entries** a document may hold, folders included.
    ///
    /// Mirrored by `MAX_LAYERS` in `umber-render`'s `canvas.rs`. It no longer
    /// sizes anything in `composite.wgsl`: that array is sized by `MAX_DRAWS`,
    /// which is larger, because *a draw is not a stack entry* — a layer
    /// carrying effects composites as several draws, each with its own slot,
    /// opacity and blend mode. See `docs/layer-effects.md` §6.2, and
    /// `the_three_draw_capacities_agree` in `canvas.rs`, which pins the three
    /// numbers against each other.
    ///
    /// It bounds entries rather than the layers that hold pixels. A
    /// pass-through folder reaches the shader as nothing at all — it is
    /// flattened away in the app's `layer_draws` — so counting it here is
    /// stricter than the draw array needs today. It is counted anyway, because
    /// a folder that composites its contents as a group *will* occupy a draw,
    /// and a cap that had to be tightened later would shut documents this build
    /// had already written.
    pub const MAX: usize = 64;

    /// The deepest a folder may be nested: eight levels, 0 through 7.
    ///
    /// Enforced here rather than left to the interface for the reason
    /// [`well_formed`] gives — the eventual group stack in the fragment shader
    /// is a fixed-size array, and a document too deep for it has to be refused
    /// where the refusal can be seen.
    pub const MAX_DEPTH: u8 = 7;

    /// Slices the renderer may have to allocate: one per layer, one per mask,
    /// one spare for a floating transform's preview, and one per effect draw.
    ///
    /// Distinct from [`LayerStack::MAX`], which bounds *stack entries*. A mask
    /// occupies no stack entry, so the two numbers genuinely differ; conflating
    /// them would have capped a document at 32 masked layers. The rest is the
    /// effect-draw budget, because a baked effect is an ordinary slice of the
    /// same array.
    ///
    /// **It is a flat 256 because that is a hardware guarantee, and everything
    /// else is derived from it rather than the other way round.** `Gpu::new`
    /// requests wgpu's `downlevel_defaults`, which leaves
    /// `max_texture_array_layers` at 256; a 257th slice is a `create_texture`
    /// validation error, and a validation error is fatal. So the ceiling is
    /// the input: 64 layers, 64 masks and the float's spare take 129, and the
    /// **127** left over are the effect budget — which is where
    /// `umber-render`'s `MAX_DRAWS` of 191 comes from, since an effect draw
    /// reads an effect slice. `docs/layer-effects.md` §6.3 asked for 257 and
    /// 192 without checking the limit.
    ///
    /// `canvas.rs` carries the whole argument and a `const` assertion against
    /// the limit, because that is where wgpu can be seen. `umber-core` may not
    /// see it, which is exactly why the number is written out here rather than
    /// derived — and why the two are pinned against each other by
    /// `the_slice_ceiling_agrees_with_umber_core`.
    ///
    /// Nothing is allocated by raising it. `CanvasRenderer` starts at
    /// `INITIAL_SLOTS` of four and `ensure_slots` grows towards what the stack
    /// actually claims, so a document with no masks and no effects pays for the
    /// headroom in nothing but this pool's ceiling.
    ///
    /// **"Grows towards" and not "doubles towards", and the difference was a
    /// live defect.** Doubling *overshoots*, and `.min(MAX_SLOTS)` used to trim
    /// the overshoot back because the ceiling was 129 — so raising it here to
    /// 256 made a document needing its 129th slice allocate 256 of them, 4.29 GB
    /// at 2048² against the 2.16 GB it asked for, permanently, from a legal
    /// stack of 64 masked layers. `grown_capacity` in `umber-render` is the
    /// repair and doubles only while the whole array stays inside a byte budget.
    /// This sentence claimed the overshoot could not happen while it was
    /// happening.
    pub const MAX_SLOTS: u32 = 256;

    /// How many independent link groups a document may hold.
    ///
    /// Bounded by the *colours*, not by anything the model needs: a group is
    /// told apart from its neighbours by the colour of the chain on its rows,
    /// and two groups sharing a colour would be a mark that lies about which
    /// layers travel together. `theme::Palette::link_colours` is this long, and
    /// asking for a seventh group is refused with a tooltip saying so rather
    /// than granted with a repeated colour.
    pub const LINK_GROUPS: usize = 6;

    pub fn new() -> Self {
        let mut stack = Self::empty();
        let slot = stack.take_slot();
        debug_assert!(slot.is_some(), "a fresh pool has room for one layer");
        stack.layers.push(Layer::named("Layer 1", 0, slot));
        stack.next_id = 1;
        stack
    }

    /// An empty stack, for [`LayerStack::push_imported`] to fill.
    ///
    /// Only ever a half-built thing, which is why it is not public: a document
    /// with no layer has nowhere to paint, and every other constructor here
    /// guarantees one. `docimport` fills it entry by entry because an import
    /// has folders in it, and a folder is not something [`LayerStack::add`] can
    /// be asked for — it takes no slot, and `add`'s whole contract is to hand
    /// one back.
    pub(crate) fn empty() -> Self {
        Self {
            layers: Vec::new(),
            active: 0,
            pool: Arc::new(Mutex::new(SlotPool::default())),
            next_id: 0,
        }
    }

    /// A handle answering "is there a slice to hand out", for the undo history
    /// to check as it gives parked entries up. See [`SlotRoom`].
    pub fn room(&self) -> SlotRoom {
        SlotRoom(Arc::clone(&self.pool))
    }

    /// Append an entry at the top, for an import.
    ///
    /// Returns the slot a layer took, or `None` for a folder — which is the
    /// same shape [`Layer::slot`] has and for the same reason, so a caller
    /// cannot forget that a folder has no pixels to upload into.
    pub(crate) fn push_imported(&mut self, folder: bool, depth: u8, name: String) -> Option<u32> {
        let depth = depth.min(Self::MAX_DEPTH);
        let id = self.take_id();
        let mut entry = if folder {
            Layer::folder_named(&name, id)
        } else {
            let slot = self.take_slot();
            // An import is bounded at [`LayerStack::MAX`] entries by
            // `ImportedDocument::validate`, so 64 layers and 64 masks is 128
            // slices against [`LayerStack::MAX_SLOTS`]'s 256 and the pool cannot
            // run dry here. Nothing parks a slice during an import either — a
            // freshly opened document has no history. Said out loud because the
            // failure would be a layer with no slice and `folder` false, which
            // is a state nothing downstream expects and every reader of
            // `Layer::slot` would take for a folder.
            debug_assert!(slot.is_some(), "an import outran the slice pool");
            Layer::named(&name, id, slot)
        };
        entry.name = name;
        entry.depth = depth;
        let slot = entry.slot();
        self.layers.push(entry);
        slot
    }

    /// Make the stack describe a tree, whatever the file said.
    ///
    /// An import can name depths that do not nest — a `<stack>` Umber declined
    /// to load a layer out of, a file some other application wrote, or one
    /// simply damaged — and the reader has no business trusting them. Rather
    /// than refuse a picture over its indentation, every depth that cannot be
    /// enclosed is pulled outwards until it can. The pixels are all there
    /// either way; what changes is only how the list groups them.
    pub(crate) fn flatten_ill_formed(&mut self) {
        let mut allowed = 0u8;
        for i in (0..self.layers.len()).rev() {
            let entry = &mut self.layers[i];
            entry.depth = entry.depth.min(allowed).min(Self::MAX_DEPTH);
            allowed = if entry.folder {
                entry.depth + 1
            } else {
                entry.depth
            };
        }
        debug_assert!(well_formed(&self.shape_pairs()));
    }

    pub fn len(&self) -> usize {
        self.layers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.layers.is_empty()
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn get(&self, index: usize) -> Option<&Layer> {
        self.layers.get(index)
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Layer> {
        self.layers.get_mut(index)
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &Layer {
        &self.layers[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Layer {
        &mut self.layers[self.active]
    }

    /// Slot that strokes should be committed into, or `None` when a folder is
    /// selected.
    ///
    /// A folder is selectable — it has to be, or it could not be renamed,
    /// hidden, dragged or deleted — so "there is nowhere to paint" is a state
    /// the caller genuinely has to handle rather than a case that cannot
    /// arise. `Editor::begin_stroke` refuses on it at the same gate a lock is
    /// refused at.
    pub fn active_slot(&self) -> Option<u32> {
        self.layers[self.active].slot()
    }

    /// Is the selected entry a folder, and therefore not somewhere to paint?
    pub fn active_is_folder(&self) -> bool {
        self.layers[self.active].folder
    }

    // --- the tree -----------------------------------------------------------

    /// The entries `index` owns, itself included.
    ///
    /// For an ordinary layer that is just `index..index + 1`. For a folder it
    /// runs back down over the contiguous block beneath it whose depth is
    /// greater — see the module docs for why a folder is *above* its contents
    /// and not below them.
    ///
    /// This is the one place the containment rule is written down. Everything
    /// that has to move, delete, hide, lock, tick or draw a folder's contents
    /// asks here.
    pub fn subtree(&self, index: usize) -> std::ops::Range<usize> {
        let Some(entry) = self.layers.get(index) else {
            return index..index;
        };
        if !entry.folder {
            return index..index + 1;
        }
        let mut start = index;
        while start > 0 && self.layers[start - 1].depth > entry.depth {
            start -= 1;
        }
        start..index + 1
    }

    /// The folders enclosing `index`, innermost first.
    ///
    /// Walking *up* the list: the enclosing folder of an entry at depth `d` is
    /// the first entry above it at depth `d - 1`, which well-formedness
    /// guarantees is a folder.
    ///
    /// **An iterator and not a `Vec`**, because this is on the drawing path:
    /// `effective_visible` asks it for every layer of every frame, from
    /// `Editor::layer_draws`, and the layer list asks it again for every row.
    /// A short-lived allocation per layer per frame is exactly what the rule
    /// about the drawing path exists to keep out, and a tall stack would pay it
    /// a hundred and twenty times a frame for an answer that is at most eight
    /// steps of arithmetic.
    pub fn ancestors_of(&self, index: usize) -> impl Iterator<Item = usize> + '_ {
        let mut want = self.layers.get(index).map_or(0, |l| l.depth);
        let mut i = index + 1;
        std::iter::from_fn(move || {
            while want > 0 && i < self.layers.len() {
                let here = i;
                i += 1;
                if self.layers[here].depth < want {
                    want = self.layers[here].depth;
                    return Some(here);
                }
            }
            None
        })
    }

    /// Does this entry actually contribute to the picture?
    ///
    /// Its own eye and every enclosing folder's. Visibility is the one member
    /// of a folder's state that a *pass-through* folder can carry, and it is
    /// free precisely because it is a boolean: `hidden ∧ anything = hidden`, so
    /// folding it into the children is the same picture rather than an
    /// approximation of one. An opacity is not — a folder at 50% over two
    /// overlapping children is not two children at 50% each — which is why
    /// there is no `effective_opacity` beside this and no control to feed one.
    pub fn effective_visible(&self, index: usize) -> bool {
        self.layers.get(index).is_some_and(|l| l.visible)
            && self.ancestors_of(index).all(|i| self.layers[i].visible)
    }

    /// Is this entry locked, by its own flag or by a folder it is in?
    ///
    /// A lock on a folder reaches its contents for the reason its visibility
    /// does, and it is read at the same one gate per operation everything else
    /// about a lock is read at — see [`LayerStack::locked_at`].
    pub fn effective_locked(&self, index: usize) -> bool {
        self.layers.get(index).is_some_and(|l| l.locked)
            || self.ancestors_of(index).any(|i| self.layers[i].locked)
    }

    /// How many entries hold pixels. A document needs at least one.
    pub fn pixel_count(&self) -> usize {
        self.layers.iter().filter(|l| !l.folder).count()
    }

    /// Would deleting all of `indices` leave somewhere to paint?
    ///
    /// What the delete buttons ask before they draw themselves enabled, and it
    /// is the same arithmetic [`LayerStack::remove`] refuses on — so the
    /// control cannot promise an operation the model will decline. It has to be
    /// asked rather than guessed at from the entry count, because **a folder is
    /// not somewhere to paint**: a stack of one layer inside one folder is two
    /// entries and still cannot give either of them up, and deleting a folder
    /// takes every layer inside it.
    pub fn can_remove(&self, indices: &[usize]) -> bool {
        let mut going: Vec<usize> = indices
            .iter()
            .filter(|i| **i < self.layers.len())
            .flat_map(|i| self.subtree(*i))
            .collect();
        going.sort_unstable();
        going.dedup();
        let lost = going.iter().filter(|i| !self.layers[**i].folder).count();
        !going.is_empty() && self.pixel_count() > lost
    }

    /// Would [`LayerStack::group`] do anything?
    ///
    /// What the Group button draws itself from, for the reason
    /// [`LayerStack::can_reorder`] exists: the refusals here — a full stack, a
    /// set that would nest past [`LayerStack::MAX_DEPTH`], and naming nothing —
    /// are invisible at the call site, and a live button that does nothing and
    /// says nothing is the control the interface rules forbid.
    pub fn can_group(&self, indices: &[usize]) -> bool {
        self.plan_group(indices).is_some()
    }

    /// The members of a grouping and the depth each would take, or `None` where
    /// it is refused.
    ///
    /// Shared by [`LayerStack::group`] and [`LayerStack::can_group`], so the
    /// button and the operation cannot disagree — the same arrangement
    /// `plan_reorder` and `can_reorder` keep.
    ///
    /// The group lands where its **topmost** member was, and the members arrive
    /// in the order they were already in. Gathering entries that were not
    /// adjacent does move them past each other and therefore does change the
    /// picture — that is what grouping is in every application that has it, and
    /// the alternative (refusing a non-contiguous selection) would make the
    /// gesture fail for the commonest reason anybody reaches for it.
    ///
    /// **Frees no slot and reassigns none**, so it does not clear the undo
    /// history. It is a `Vec` shuffle plus one entry that holds no pixels,
    /// which is exactly [`LayerStack::reorder`]'s argument for itself.
    fn plan_group(&self, indices: &[usize]) -> Option<(Vec<usize>, Vec<u8>)> {
        if self.layers.len() >= Self::MAX {
            return None;
        }
        // Whole subtrees, deduplicated: naming a folder and one of its children
        // must not take that child twice, and must not leave it behind either.
        let mut members: Vec<usize> = indices
            .iter()
            .filter(|i| **i < self.layers.len())
            .flat_map(|i| self.subtree(*i))
            .collect();
        members.sort_unstable();
        members.dedup();
        if members.is_empty() {
            return None;
        }
        // The shallowest member is the level the new folder takes; everything
        // moving goes one deeper *than its own root*, not one deeper flat. A
        // member two levels down inside another folder that is also moving has
        // to keep that relationship, or the block arrives describing a layer
        // nested inside nothing.
        let base = members.iter().map(|i| self.layers[*i].depth).min()?;
        let depths: Vec<u8> = members
            .iter()
            .map(|i| {
                // The outermost enclosing folder that is *itself* moving is the
                // root this entry hangs off; failing that it is its own root.
                // The *outermost* enclosing folder that is itself moving —
                // `ancestors_of` runs innermost first, so that is the last one
                // it yields that is a member.
                let root = self
                    .ancestors_of(*i)
                    .filter(|a| members.contains(a))
                    .last()
                    .unwrap_or(*i);
                self.layers[*i].depth + base + 1 - self.layers[root].depth
            })
            .collect();
        if depths.iter().any(|d| *d > Self::MAX_DEPTH) {
            return None;
        }
        Some((members, depths))
    }

    /// Put the entries of `indices` — each expanded to its whole subtree — into
    /// a new folder, and select it. See [`LayerStack::plan_group`] for the
    /// refusals and [`LayerStack::can_group`] for the button's half.
    pub fn group(&mut self, indices: &[usize]) -> Option<usize> {
        let (members, depths) = self.plan_group(indices)?;
        let base = members.iter().map(|i| self.layers[*i].depth).min()?;

        // **Before the members are lifted out.** A folder being grouped is one
        // of them, so asking afterwards would not see it and would hand the new
        // folder the same name — `grouping_keeps_the_nesting_the_entries_
        // already_had` caught exactly that, twice-named "Group 1".
        let name = format!("Group {}", self.next_group_number());

        let mut taken: Vec<Layer> = Vec::with_capacity(members.len());
        for (n, i) in members.iter().enumerate().rev() {
            let mut layer = self.layers.remove(*i);
            layer.depth = depths[n];
            taken.push(layer);
        }
        taken.reverse();

        // Where the block lands: the number of surviving entries below the
        // topmost member, so the folder sits exactly where that member did.
        let top = *members.last()?;
        let at = (0..top).filter(|i| !members.contains(i)).count();
        let mut folder = Layer::folder_named(&name, self.take_id());
        folder.depth = base;
        let n = taken.len();
        for (k, layer) in taken.into_iter().enumerate() {
            self.layers.insert(at + k, layer);
        }
        self.layers.insert(at + n, folder);

        debug_assert!(
            well_formed(&self.shape_pairs()),
            "group left a malformed stack"
        );
        // The new folder is selected, which is what makes it renameable the
        // moment it exists. Nothing was removed, so no slot changed hands and
        // the layer that *was* selected is still in the stack — inside the
        // folder, which is where the painter just put it.
        self.active = at + n;
        Some(at + n)
    }

    /// A name for a new folder that does not collide with an existing one, on
    /// the same argument [`LayerStack::next_name_number`] makes for layers.
    fn next_group_number(&self) -> usize {
        self.layers
            .iter()
            .filter(|l| l.folder)
            .filter_map(|l| l.name.strip_prefix("Group "))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0)
            + 1
    }

    pub fn set_active(&mut self, index: usize) {
        if index < self.layers.len() {
            self.active = index;
        }
    }

    /// How many texture-array slices the renderer must have allocated.
    ///
    /// This is one past the highest slice **currently claimed**, not the layer
    /// count: slots are stable, so a stack of two layers can still be using
    /// slot 5, and a slice parked in an undo entry is claimed even though it is
    /// in no layer.
    ///
    /// **A parked slice must stay below this number, and that is load
    /// bearing.** `CanvasRenderer::begin_float` takes its preview slice at
    /// exactly this value, so a preview can never be rendered into a deleted
    /// layer's pixels — the obvious tidy-up, making this the high-water mark of
    /// the *live stack* alone, creates precisely that bug and it would look
    /// like a transform quietly eating an undone layer.
    pub fn slot_capacity_needed(&self) -> u32 {
        SlotPool::locked(&self.pool).next
    }

    /// What [`LayerStack::slot_capacity_needed`] would answer after one more
    /// slice is claimed, **without claiming one**.
    ///
    /// This exists so a caller can ask the device for the storage *before* the
    /// layer or the mask exists. The renderer's refusal changes nothing at all,
    /// which is only worth anything if the model has not already moved: a stack
    /// holding a slice the texture array does not have is a stack the composite
    /// indexes off the end of, and there is no putting that back from the app.
    ///
    /// **It is not `slot_capacity_needed() + 1`**, and that is the point. A
    /// delete parks its slice in an undo entry and an eviction gives the number
    /// back, so the pool routinely holds a gap; a claim that fills one moves
    /// nothing and needs no storage. Asking for one more than the high-water
    /// mark would grow the array by a slice nobody will use — at 10000² that is
    /// 400 MB, on exactly the canvas where growth is a single slice and the
    /// waste is not lost in a doubling.
    ///
    /// Where nothing can be claimed at all — an empty free list against the
    /// ceiling — the answer is the mark itself, because the add is about to be
    /// refused by [`LayerStack::add`] and no storage will be asked for.
    pub fn slot_capacity_after_one_claim(&self) -> u32 {
        let pool = SlotPool::locked(&self.pool);
        if pool.free.is_empty() && pool.has_headroom() {
            pool.next + 1
        } else {
            pool.next
        }
    }

    /// One past the highest slice the **live stack** claims, ignoring anything
    /// parked in an undo entry.
    ///
    /// Deliberately not what [`LayerStack::slot_capacity_needed`] answers — see
    /// there, and do not swap one for the other. This is for exactly one
    /// question, and the question is not "how much storage": it is *could
    /// releasing every parked slice give a floating transform its preview
    /// slice?* Where this already reaches [`LayerStack::MAX_SLOTS`], the answer
    /// is no however much undo history is given up, because the tail cannot be
    /// compacted past a slice a layer is holding — and a caller that spent the
    /// history finding that out would have destroyed an afternoon for nothing.
    ///
    /// **It is a slot number and not a count**, which is what makes the state
    /// reachable at all: a layer added while most of the range is parked takes
    /// a number near the top and keeps it. This used to say "64 layers each
    /// with a mask is exactly that state", which was never true — that is 128
    /// slices, and the ceiling has never been that low.
    pub fn live_slot_ceiling(&self) -> u32 {
        self.layers
            .iter()
            .flat_map(|l| [l.slot(), l.mask()])
            .flatten()
            .map(|slot| slot + 1)
            .max()
            .unwrap_or(0)
    }

    /// Insert a new empty layer directly above the active one and select it.
    ///
    /// Returns the new layer's slot, which the caller must clear on the GPU —
    /// a recycled slot still holds the deleted layer's pixels.
    /// **Into the selected folder** when one is selected, and beside the
    /// selected layer otherwise.
    ///
    /// A folder's row sits above its contents, so "just below the folder" is
    /// its topmost child — which is where every application puts a new layer
    /// made while a group is in hand, and it is also the only reading that lets
    /// somebody fill a folder they have just made without dragging.
    pub fn add(&mut self) -> Option<u32> {
        let name = format!("Layer {}", self.next_name_number());
        self.add_named(&name).map(|made| made.slot)
    }

    /// The same, under a name the caller chose.
    ///
    /// **This is not a rename**, and that distinction is the whole reason it is
    /// safe to add. [`Layer::name`] is written where a layer is created or
    /// imported and nowhere else — there is no `rename` on this type, because
    /// undoing one needs an [`crate::EditBody`] arm that does not exist yet and
    /// `docs/layer-rename.md` is the standing design for it. A name given at
    /// *creation* is not a value anybody then changed, so it needs no such arm:
    /// the only way back is the structural entry that takes the whole layer out,
    /// and that entry carries the layer itself.
    ///
    /// Its caller is a text placement, which names the layer after the words on
    /// it — see [`crate::textobj::layer_name`]. [`LayerStack::add`] is this with
    /// the "Layer N" it has always used.
    ///
    /// Returns the slot **and the new entry's id**. The id is what a caller
    /// holding on to the layer across a gesture has to keep: a slot is a slice
    /// and an index is a position in a `Vec`, and only the id survives a
    /// reorder.
    pub fn add_named(&mut self, name: &str) -> Option<AddedLayer> {
        if self.layers.len() >= Self::MAX {
            return None;
        }
        let selected = &self.layers[self.active];
        let (at, depth) = if selected.folder {
            if selected.depth + 1 > Self::MAX_DEPTH {
                return None;
            }
            (self.active, selected.depth + 1)
        } else {
            (self.active + 1, selected.depth)
        };
        // Before the insert, so a pool with nothing left changes nothing at
        // all. The caller's remedy is to give a parked slice back — see
        // [`SlotRoom`] — not to be handed a layer with nowhere to paint.
        let slot = self.take_slot()?;
        let number = slot.number();
        let id = self.take_id();
        let mut layer = Layer::named(name, id, Some(slot));
        layer.depth = depth;
        self.layers.insert(at, layer);
        debug_assert!(
            well_formed(&self.shape_pairs()),
            "add left a malformed stack"
        );
        self.active = at;
        Some(AddedLayer { slot: number, id })
    }

    /// Would a layer added right now come out locked?
    ///
    /// The `can_` beside [`LayerStack::add_named`], and it asks about the
    /// *insertion point* rather than about the selected entry — which is the one
    /// thing here that is easy to get backwards. A new layer carries no lock of
    /// its own, so the only thing that can lock it is what encloses it:
    ///
    /// * with a **layer** selected the new one is its sibling, so what reaches
    ///   it is that layer's ancestors and **not** that layer's own flag. A
    ///   locked layer inside an unlocked folder is somewhere a new layer may
    ///   perfectly well go, and reading [`LayerStack::active_is_locked`] here
    ///   would refuse it;
    /// * with a **folder** selected the new one goes inside it, so the folder's
    ///   own flag counts too — which is exactly
    ///   [`LayerStack::effective_locked`] of the folder.
    ///
    /// It exists because a text placement that makes its own layer is not gated
    /// by the *selected* layer's lock at all, where one painting onto the
    /// selected layer is. One predicate answering both would refuse an
    /// operation the model allows, which is the control that lies in its other
    /// direction.
    pub fn new_layer_would_be_locked(&self) -> bool {
        match self.layers.get(self.active) {
            Some(l) if l.folder => self.effective_locked(self.active),
            Some(_) => self.ancestors_of(self.active).any(|i| self.layers[i].locked),
            None => false,
        }
    }

    /// Hand out the next free slice, recycling before growing.
    ///
    /// `None` when every slice is claimed — by the stack, or by a layer parked
    /// in an undo entry. See [`SlotPool::has_room`].
    fn take_slot(&mut self) -> Option<SlotClaim> {
        let number = SlotPool::locked(&self.pool).take()?;
        Some(SlotClaim(Arc::new(Claim {
            number,
            pool: Arc::clone(&self.pool),
        })))
    }

    /// The next entry identity. See [`Layer::id`].
    fn take_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Give the layer at `index` a mask, returning the slice it took.
    ///
    /// The caller must **fill that slice opaque white** on the GPU. White is
    /// "reveal everything", so a mask that has just been added changes nothing
    /// about the picture — which is what makes adding one a safe thing to try.
    /// A recycled slice still holds whatever the last layer or mask left in it,
    /// exactly as [`LayerStack::add`]'s does.
    ///
    /// `None` where the layer already has one, or the index is off the end.
    pub fn add_mask(&mut self, index: usize) -> Option<u32> {
        if self.layers.get(index)?.mask.is_some() {
            return None;
        }
        let slot = self.take_slot()?;
        let number = slot.number();
        self.layers[index].mask = Some(slot);
        Some(number)
    }

    /// Take the mask off the layer at `index`, returning the claim it held.
    ///
    /// **The caller must park that claim in the undo entry it records**, or
    /// dropping it puts the slice straight back on the free list — which is
    /// what used to make this clear the whole history, because a patch recorded
    /// against the slice would then be replayed into whatever inherited it.
    /// [`LayerStack::shape_with_mask`] is where it goes.
    pub fn remove_mask(&mut self, index: usize) -> Option<SlotClaim> {
        self.layers.get_mut(index)?.mask.take()
    }

    /// The mask slice of the layer at `index`, if it has one.
    pub fn mask_at(&self, index: usize) -> Option<u32> {
        self.layers.get(index)?.mask()
    }

    /// The mask slice of the selected layer, if it has one.
    pub fn active_mask(&self) -> Option<u32> {
        self.layers[self.active].mask()
    }

    // --- text ---------------------------------------------------------------

    /// Record what set the layer at `index`, making it a text layer.
    ///
    /// False for an index off the end and **false for a folder**, which holds no
    /// pixels for a record to describe: a folder carrying one would be written to
    /// the file as an entry beside a layer image that does not exist, and every
    /// reader of [`Layer::is_text`] would then have a text layer with nothing in
    /// it. The refusal is here rather than at the call site for the reason every
    /// other refusal in this module is.
    pub fn set_text(&mut self, index: usize, text: TextObject) -> bool {
        match self.layers.get_mut(index) {
            Some(layer) if !layer.folder => {
                layer.text = Some(Box::new(text));
                true
            }
            _ => false,
        }
    }

    /// The record for the layer at `index`.
    pub fn text_at(&self, index: usize) -> Option<&TextObject> {
        self.layers.get(index)?.text()
    }

    /// The record for the selected entry.
    pub fn active_text(&self) -> Option<&TextObject> {
        self.layers[self.active].text()
    }

    /// Take the record off, leaving the pixels exactly where they are.
    ///
    /// **This is what "convert to paint" is**, and it is the whole of it: the
    /// layer keeps every pixel and stops claiming they can be set again, so
    /// [`LayerStack::refusal_at`] lets a brush through from the next stroke on.
    /// There is deliberately nothing to undo here — no pixel changes — which is
    /// also why it is not an [`crate::EditKind`]; see `docs/text-tool.md` §3.
    pub fn take_text(&mut self, index: usize) -> Option<Box<TextObject>> {
        self.layers.get_mut(index)?.text.take()
    }

    /// Take every record off, keeping every pixel.
    ///
    /// What a **resize** should do, for the reason a resize clears the undo
    /// history: a placement is a rectangle of a canvas that no longer exists, and
    /// a canvas that shrank has cropped the pixels the record describes, so
    /// re-rendering would put back a part of the text the artist cut away.
    /// Translating the placement by the anchor's offset would be exact for a
    /// canvas that only *grew*, and two behaviours behind one command is how the
    /// cropping case comes to be the one nobody tested.
    ///
    /// **Called from `Editor::apply_canvas`**, beside the history clear and the
    /// selection being dropped, and only where the size actually changed —
    /// calling it unconditionally would make every caption in the document paint
    /// the moment somebody pressed Apply on a dialog they had not touched.
    ///
    /// Returns how many were dropped, so the caller can say so rather than
    /// leaving somebody to find out that their text is paint now.
    pub fn drop_text_objects(&mut self) -> usize {
        let mut dropped = 0;
        for layer in &mut self.layers {
            dropped += usize::from(layer.text.take().is_some());
        }
        dropped
    }

    /// Mirror every record with the canvas.
    ///
    /// **It belongs beside wherever the layer pixels are flipped, and there is one
    /// such place**: `app.rs`'s `mirror_document` is the single route a flip
    /// takes, shared by the command and by both undo directions. It reaches here
    /// through `Editor::flip_canvas`, on the line after
    /// [`LayerStack::flip_effects`] and for the same reason — both mirror
    /// something that carries a direction or a position. Leaving the record
    /// behind means the next re-render un-mirrors the layer.
    ///
    /// The mirror is exact rather than approximate — see
    /// [`crate::textobj::Placement::flipped`] — which is why a flip does not cost
    /// a text layer its record the way a resize does. A record whose source
    /// rectangle is somehow not inside the canvas cannot be mirrored into it and
    /// is dropped instead, because a record that lies about where its pixels are
    /// is worse than none; the count comes back for the same reason
    /// [`LayerStack::drop_text_objects`]'s does.
    pub fn flip_text(&mut self, axis: FlipAxis, canvas: UVec2) -> usize {
        let mut dropped = 0;
        for layer in &mut self.layers {
            let Some(text) = layer.text.take() else {
                continue;
            };
            match text.flipped(axis, canvas) {
                Some(flipped) => layer.text = Some(Box::new(flipped)),
                None => dropped += 1,
            }
        }
        dropped
    }

    // --- effects ------------------------------------------------------------

    /// Mirror every effect's lighting with the canvas.
    ///
    /// **Beside [`LayerStack::flip_text`] because it is the same job**, and the
    /// two are the whole of what a flip has to do to the model beyond the pixels:
    /// anything holding a *direction* has to be mirrored, or every pixel in the
    /// picture turns over and the lighting does not. That is a picture which is
    /// wrong rather than merely plainer, and wrong in the one direction an artist
    /// notices at once, because a whole document's shadows suddenly disagree with
    /// its forms.
    ///
    /// It went unnoticed on this side and was written down on the other, which is
    /// why a second pair of eyes had to find it: [`LayerStack::flip_text`]'s docs
    /// said in as many words that nothing called it, and an effect's flip said
    /// nothing at all. **If a third thing on a layer ever carries a direction,
    /// say so at its own method whether or not it is wired.**
    ///
    /// Unlike `flip_text` this takes no canvas size, cannot fail and drops
    /// nothing — see [`Effect::flipped`]. Every other field of an effect is a
    /// length or a colour and a mirror preserves those.
    ///
    /// **Called from `Editor::flip_canvas`**, which `app.rs`'s `mirror_document`
    /// is the single route to and which already holds the model's other half of a
    /// flip, the selection's mirror. [`LayerStack::flip_text`] is on the next
    /// line, where this doc said it belonged.
    pub fn flip_effects(&mut self, axis: FlipAxis) {
        for layer in &mut self.layers {
            for effect in &mut layer.effects {
                *effect = effect.flipped(axis);
            }
        }
        // No re-sort: `Effect::rank` reads the kind and the position, and a flip
        // touches neither, so the composite order a layer holds its effects in is
        // unchanged. Sorting anyway would be harmless and would suggest the
        // opposite.
    }

    /// How many effects in this document would produce a draw.
    ///
    /// What the panel reads to say a document is at its effects budget, and what
    /// [`LayerStack::plan_set_effect`] counts against [`effect::MAX_ENABLED`].
    pub fn enabled_effect_count(&self) -> usize {
        self.layers.iter().map(Layer::enabled_effect_count).sum()
    }

    /// The effects the entry at `index` would then hold, in composite order, or
    /// `None` where the change is refused.
    ///
    /// Shared by [`LayerStack::set_effect`] and [`LayerStack::can_set_effect`],
    /// so a control cannot light up promising something the model will decline —
    /// the arrangement `plan_group`/`can_group` and `plan_reorder`/`can_reorder`
    /// already keep. Returning the whole vector rather than a verdict is what
    /// makes "a refusal changes nothing at all" **structural**: the layer is not
    /// touched until there is a plan to install, so there is no half-applied
    /// state for a refusal to leave behind.
    ///
    /// Three refusals:
    ///
    /// * An index off the end.
    /// * **A folder** (`docs/layer-effects.md` §9.5). A folder holds no slot and
    ///   its contents composite in place, so there is no coverage to derive an
    ///   effect from; the honest input is the group's composited result, and
    ///   that does not exist until `docs/group-compositing.md` is built. The
    ///   control is drawn and disabled with a tooltip saying so, which is the
    ///   treatment a folder's blend and opacity already get.
    /// * The document's effect budget, [`effect::MAX_ENABLED`]. Counted with the
    ///   change applied, so replacing an enabled effect with another enabled one
    ///   is free and switching a disabled one on is not.
    ///
    /// Deliberately **not** gated on the layer's lock. An effect's parameters
    /// are a value on a layer with no pixels behind them, exactly as its opacity
    /// is, and a layer's opacity is not lock-gated today either — see the lock's
    /// list of gates in `CLAUDE.md`. Nor is it an undoable edit
    /// (`docs/layer-effects.md` §7): no [`crate::EditKind`] variant, no
    /// `history::VERSION` bump.
    fn plan_set_effect(&self, index: usize, effect: Effect) -> Option<Vec<Effect>> {
        let layer = self.layers.get(index)?;
        if layer.folder {
            return None;
        }

        // At most one per kind: whatever was there of this kind is replaced
        // rather than joined.
        let mut planned: Vec<Effect> = layer
            .effects
            .iter()
            .copied()
            .filter(|e| e.kind != effect.kind)
            .collect();
        planned.push(effect);
        effect::sort_into_composite_order(&mut planned);

        let elsewhere = self.enabled_effect_count() - layer.enabled_effect_count();
        let here = planned.iter().filter(|e| e.enabled).count();
        effect::within_budget(elsewhere + here).then_some(planned)
    }

    /// Would [`LayerStack::set_effect`] do it? See [`LayerStack::
    /// plan_set_effect`] for the refusals.
    pub fn can_set_effect(&self, index: usize, effect: Effect) -> bool {
        self.plan_set_effect(index, effect).is_some()
    }

    /// Give the entry at `index` this effect, replacing whatever it held of the
    /// same kind. `false` where it is refused.
    pub fn set_effect(&mut self, index: usize, effect: Effect) -> bool {
        match self.plan_set_effect(index, effect) {
            Some(planned) => {
                self.layers[index].effects = planned;
                true
            }
            None => false,
        }
    }

    /// The first kind `effects` names twice, if it names one twice.
    ///
    /// **The one statement of "at most one effect per kind", and it is a free
    /// function of a slice so that everything can ask it.** The invariant is
    /// [`Layer::effects`]'s and is maintained by [`LayerStack::
    /// plan_set_effect`], which enforces it by *replacing* — the right answer
    /// for a control setting one effect, and a silent one for anything handing
    /// over a whole set: `set_effect` would install both and answer `true`
    /// twice, leaving the layer holding one where the caller offered two.
    ///
    /// `docimport::openraster` has to ask the same question before there is a
    /// stack to ask it of, because a record naming two drop shadows is a
    /// malformed record and the reader is where a file is judged. Two guards
    /// that each decided for themselves what a duplicate was would be worse
    /// than one; this is the one, and both callers reach it.
    pub fn duplicate_effect_kind(effects: &[Effect]) -> Option<EffectKind> {
        effects
            .iter()
            .enumerate()
            .find(|(i, e)| effects[..*i].iter().any(|p| p.kind == e.kind))
            .map(|(_, e)| e.kind)
    }

    /// The effects the entry at `index` would hold if given `effects` **whole**,
    /// or `None` where the set is refused.
    ///
    /// [`LayerStack::plan_set_effect`]'s refusals, plus the one that only a set
    /// can commit: naming a kind twice. See [`LayerStack::
    /// duplicate_effect_kind`].
    ///
    /// The budget is counted **once, over the whole set**, which is the other
    /// thing installing one at a time cannot do. Feeding a set through
    /// `set_effect` in a loop asks the budget a question per effect, so a set
    /// that fits could still be refused half way through by an intermediate
    /// state that does not — and the refusal would land on whichever effect
    /// happened to be last, leaving the layer holding a prefix of what was
    /// offered. This installs all of it or none of it.
    fn plan_set_effects(&self, index: usize, effects: &[Effect]) -> Option<Vec<Effect>> {
        let layer = self.layers.get(index)?;
        if layer.folder || Self::duplicate_effect_kind(effects).is_some() {
            return None;
        }

        let mut planned = effects.to_vec();
        effect::sort_into_composite_order(&mut planned);

        let elsewhere = self.enabled_effect_count() - layer.enabled_effect_count();
        let here = planned.iter().filter(|e| e.enabled).count();
        effect::within_budget(elsewhere + here).then_some(planned)
    }

    /// Would [`LayerStack::set_effects`] do it? See [`LayerStack::
    /// plan_set_effects`] for the refusals.
    pub fn can_set_effects(&self, index: usize, effects: &[Effect]) -> bool {
        self.plan_set_effects(index, effects).is_some()
    }

    /// Give the entry at `index` exactly this set of effects, **replacing**
    /// whatever it held. `false` where it is refused, and then nothing moved.
    ///
    /// What [`crate::docimport::ImportedDocument::open`] installs a file's
    /// effects with. A loop of [`LayerStack::set_effect`] is the obvious
    /// alternative and is wrong twice over — see [`LayerStack::
    /// plan_set_effects`] — and both failures are *silent*, because each call
    /// in such a loop answers `true` for a duplicate it overwrote and the loop
    /// has no way to notice a budget refusal that arrived half way along.
    pub fn set_effects(&mut self, index: usize, effects: &[Effect]) -> bool {
        match self.plan_set_effects(index, effects) {
            Some(planned) => {
                self.layers[index].effects = planned;
                true
            }
            None => false,
        }
    }

    /// Switch an effect the entry at `index` already holds on or off.
    ///
    /// A read-modify-write through [`LayerStack::set_effect`] rather than beside
    /// it, so switching one **on** meets the budget at the same gate everything
    /// else does. `false` where the layer has no effect of that kind, or where
    /// the gate refuses. Its `can_` twin is
    /// [`LayerStack::can_set_effect_enabled`].
    ///
    /// This is the one thing about an effect that touches the slice pool, and
    /// `docs/layer-effects.md` §7 is where it is decided: switching off frees a
    /// slice and switching on claims one, and **neither parks and neither
    /// clears the undo history** — see [`LayerStack::remove_effect`] for why an
    /// effect slice is the exception to parking. Stage 0 holds no slices, so
    /// nothing here does either.
    pub fn set_effect_enabled(&mut self, index: usize, kind: EffectKind, enabled: bool) -> bool {
        match self.planned_toggle(index, kind, enabled) {
            Some(effect) => self.set_effect(index, effect),
            None => false,
        }
    }

    /// Would [`LayerStack::set_effect_enabled`] do it?
    pub fn can_set_effect_enabled(&self, index: usize, kind: EffectKind, enabled: bool) -> bool {
        match self.planned_toggle(index, kind, enabled) {
            Some(effect) => self.can_set_effect(index, effect),
            None => false,
        }
    }

    /// The effect a toggle would write, shared by the pair above so the button
    /// and the operation read the same one.
    fn planned_toggle(&self, index: usize, kind: EffectKind, enabled: bool) -> Option<Effect> {
        let mut effect = *self.layers.get(index)?.effect(kind)?;
        effect.enabled = enabled;
        Some(effect)
    }

    /// Take an effect off the entry at `index`, handing it back.
    ///
    /// No `can_` beside it and no plan: a removal can only lower the enabled
    /// count, so there is nothing for the budget to refuse, and it cannot leave
    /// the remaining effects out of order because a subsequence of an ordered
    /// list is ordered.
    ///
    /// **It frees no slice, because stage 0 bakes into none.** When one exists,
    /// `docs/layer-effects.md` §4.2 says it goes straight back on the free list
    /// rather than being parked — unlike a deleted layer's or a removed mask's,
    /// because no [`crate::PixelPatch`] can ever name it: effect pixels are
    /// derived, are never read back and are never captured into the undo
    /// history. Recorded here so that stage 1 does not reach for a `SlotClaim`
    /// on the reasoning that everything else in this file holds one.
    pub fn remove_effect(&mut self, index: usize, kind: EffectKind) -> Option<Effect> {
        let effects = &mut self.layers.get_mut(index)?.effects;
        let at = effects.iter().position(|e| e.kind == kind)?;
        Some(effects.remove(at))
    }

    /// Remove a layer, handing it over.
    ///
    /// **The layers come back rather than their slot numbers, and that is the
    /// whole of parking.** A [`Layer`] owns its [`SlotClaim`] and its mask's,
    /// so whoever holds the returned layers holds the slices — nothing else can
    /// be given them, and every recorded patch naming one goes on meaning the
    /// pixels it was captured from. Drop them and the slices go back on the
    /// free list, which is exactly what happened before parking existed.
    ///
    /// Refuses to remove the last layer — a document with no layers has nowhere
    /// to paint.
    /// **A folder takes its contents with it.** Deleting a group and leaving
    /// the layers in it behind would need every one of them re-parented, and
    /// the entry the painter pressed delete on is the one they meant; every
    /// application that has folders does the same.
    pub fn remove(&mut self, index: usize) -> Option<Vec<Layer>> {
        self.remove_many(&[index])
    }

    /// Delete every entry of `indices`, and everything inside any folder among
    /// them, in **one** pass.
    ///
    /// **One pass is the whole point, and deleting them one at a time is a bug
    /// this had.** A folder's subtree runs *below* it, so removing one shifts
    /// every index beneath it — including indices a caller walking the list
    /// backwards has not reached yet. `delete_picked_layers` did exactly that
    /// and deleted a layer nobody ticked, and cleared the undo history on the
    /// way out, so it could not even be taken back. Resolving the whole set
    /// against the stack as it stands now, before anything moves, is the only
    /// arrangement where no index goes stale.
    ///
    /// Returns the entries removed, **bottom first** — see [`LayerStack::
    /// remove`] for why the layers themselves come back and not their slot
    /// numbers. Refuses whole, changing nothing, where it would leave the
    /// document with no layer holding pixels: a folder is not somewhere to
    /// paint. [`LayerStack::can_remove`] is that same question, for the buttons.
    pub fn remove_many(&mut self, indices: &[usize]) -> Option<Vec<Layer>> {
        if !self.can_remove(indices) {
            return None;
        }
        let mut going: Vec<usize> = indices
            .iter()
            .filter(|i| **i < self.layers.len())
            .flat_map(|i| self.subtree(*i))
            .collect();
        going.sort_unstable();
        going.dedup();

        let mut taken = Vec::with_capacity(going.len());
        // Descending, so each removal only moves entries this loop has already
        // dealt with. The set itself was resolved above and does not change.
        for i in going.iter().rev() {
            taken.push(self.layers.remove(*i));
        }
        taken.reverse();

        // The selection follows the stack rather than the number: it shifts
        // down by however many entries below it went, and where it was one of
        // them the clamp catches it.
        let below = going.iter().filter(|i| **i < self.active).count();
        self.active = self
            .active
            .saturating_sub(below)
            .min(self.layers.len().saturating_sub(1));
        debug_assert!(
            well_formed(&self.shape_pairs()),
            "removing a set of subtrees left a malformed stack"
        );
        // Deleting one of a pair would otherwise leave the survivor in a group
        // of one — see `dissolve_lone_groups`.
        self.dissolve_lone_groups();
        Some(taken)
    }

    // --- locking ------------------------------------------------------------
    //
    // A lock is refused at **one gate per operation** — `Editor::begin_stroke`,
    // `App::begin_float`, `App::clear_active_layer`, `App::delete_layer` and
    // `App::mirror_document` — and every one of them asks here. Spreading the
    // test over the call sites that reach those five is how the sixth comes to
    // be forgotten.

    /// Is the layer at `index` locked? An index off the end is not.
    pub fn locked_at(&self, index: usize) -> bool {
        self.layers.get(index).is_some_and(|l| l.locked)
    }

    /// Is the layer a stroke or a transform would land on locked?
    ///
    /// **Through [`LayerStack::effective_locked`]**, so a lock on a folder
    /// reaches every layer inside it. This is the question the one gate per
    /// operation asks — `begin_stroke`, `begin_float`, `clear_active_layer` —
    /// so putting the ancestor walk here is what makes a folder's lock mean
    /// anything at all, without any of those three learning that folders exist.
    ///
    /// [`LayerStack::locked_at`] deliberately stays the layer's *own* flag: it
    /// is what a row's padlock draws and toggles, and a row that showed itself
    /// locked because of a folder would offer an unlock that did nothing.
    pub fn active_is_locked(&self) -> bool {
        self.effective_locked(self.active)
    }

    /// **Why an edit on this entry would be refused**, or `None` where it goes
    /// ahead. The one gate an operation that writes pixels should ask.
    ///
    /// It is the two tests `begin_stroke` and `begin_float` already make — a lock
    /// and a folder — with text as the third, in one call with one answer and one
    /// reason to show. Three separate booleans is how the fourth gate comes to
    /// check only two of them.
    ///
    /// **Four operations ask it**, and they are every route by which a layer's
    /// own pixels are written outside an undo: `Editor::begin_stroke`,
    /// `App::begin_float` for a lift and a paste both, and `App::cut_selection`.
    /// Anything added beside them has to ask it too — the fingerprint does *not*
    /// cover a record that has come adrift in the session, since a save takes
    /// its fingerprint from the pixels it is writing, so the file agrees with
    /// itself and the next open re-renders over whatever is there. See
    /// `docs/text-tool.md` §3 and this module's docs.
    ///
    /// **A lift is the one that is allowed through and it is not an exception
    /// here**: `begin_float` filters this answer for a lift itself, because a
    /// lift moves the caption's *own* pixels and takes the record off with them.
    /// This method's answer is unchanged, which is what keeps the reading in one
    /// place and the policy at the caller.
    ///
    /// `target` matters, and it is the half that is easy to get backwards. A
    /// stroke on a text layer's **mask** is allowed: a mask bounds the alpha the
    /// composite reads and changes none of the layer's own pixels, so it cannot
    /// put the record out of step with them. A lock and a folder refuse both
    /// targets — a lock is a statement about the whole layer, and a folder has
    /// neither slice.
    ///
    /// **"Clear layer" is not one of the operations this refuses**, and that is
    /// deliberate. It genuinely means to replace the pixels, so it must take the
    /// record off instead — [`LayerStack::take_text`] — or the next save would
    /// record text over a blank layer with a fingerprint that agrees, and
    /// reopening would re-render text somebody had cleared.
    ///
    /// The order is the order of the sentences somebody would want: what a folder
    /// cannot do at all, then what a lock forbids until it is unlocked, then what
    /// text forbids until it is converted.
    pub fn refusal_at(&self, index: usize, target: EditTarget) -> Option<EditRefusal> {
        // Fails closed. An index off the end is a caller bug rather than a
        // permission, and this is a gate: see [`EditRefusal::Missing`].
        let Some(layer) = self.layers.get(index) else {
            return Some(EditRefusal::Missing);
        };
        if layer.folder {
            return Some(EditRefusal::Folder);
        }
        if self.effective_locked(index) {
            return Some(EditRefusal::Locked);
        }
        if layer.is_text() {
            // An exhaustive `match` and not `target == EditTarget::Layer`, which
            // is `matches!` wearing an equality: it would answer **permitted**
            // for a target this build has never heard of, on the gate that exists
            // to refuse.
            //
            // `Editor::stroke_target` is the other reader of this enum and is
            // exhaustive too — but it was not when this comment first cited it as
            // the precedent, and the history is the point. It read
            // `match (self.edit_target, self.layers.active_mask())` with one
            // catch-all falling through to the *layer's* slot, so a third variant
            // there would have been a stroke landing silently on the layer's
            // pixels. Both halves are shut now, and what closed the second was
            // somebody reading the claim made about it here. **Two readers of one
            // enum must not disagree about whether it is closed**, and a comment
            // asserting that they agree is worth exactly as much as the last time
            // anybody checked.
            match target {
                EditTarget::Layer => return Some(EditRefusal::Text),
                EditTarget::Mask => {}
            }
        }
        None
    }

    /// [`LayerStack::refusal_at`] for the selected entry.
    pub fn active_refusal(&self, target: EditTarget) -> Option<EditRefusal> {
        self.refusal_at(self.active, target)
    }

    /// Is any layer locked?
    ///
    /// What the canvas flip asks, because a flip mirrors the whole document:
    /// mirroring some layers and not others would leave a picture that was
    /// never on screen, so the flip is refused whole rather than applied in
    /// part.
    pub fn any_locked(&self) -> bool {
        self.layers.iter().any(|l| l.locked)
    }

    // --- ticking several layers ---------------------------------------------

    /// Stack positions of every ticked layer, ascending.
    pub fn picked_indices(&self) -> Vec<usize> {
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.picked)
            .map(|(i, _)| i)
            .collect()
    }

    /// How many layers are ticked.
    pub fn picked_count(&self) -> usize {
        self.layers.iter().filter(|l| l.picked).count()
    }

    /// **What a bulk operation reaches**, and the one place that is decided:
    /// every ticked layer, or the selected one alone when nothing is ticked.
    ///
    /// The fallback makes the rule total, so a caller never has to special-case
    /// an empty tick list. It is not currently reached — the strip that holds
    /// every caller is only drawn once something is ticked — and it is
    /// deliberately not what the *single-layer* controls use: a row's own eye
    /// writes `visible` directly, because that control means "this layer" and
    /// routing it through here would let a tick on another row change what it
    /// does.
    ///
    /// Ascending, so a caller deleting them can walk the list backwards and
    /// have every index still be valid as it goes.
    pub fn targets(&self) -> Vec<usize> {
        let picked = self.picked_indices();
        if picked.is_empty() {
            vec![self.active]
        } else {
            picked
        }
    }

    /// Tick or untick every layer at once.
    pub fn pick_all(&mut self, on: bool) {
        for layer in &mut self.layers {
            layer.picked = on;
        }
    }

    /// Tick or untick one entry, **and everything inside it**.
    ///
    /// Ticking a folder means ticking what is in it — which is the half of
    /// "mark a folder visible to make all its layers visible" that a folder has
    /// to supply, since the other half is [`LayerStack::effective_visible`].
    ///
    /// It is **written into the ticks** rather than derived when they are read,
    /// and the difference matters: a painter who ticks a folder and then unticks
    /// one layer in it means what they did, where a rule that re-derived the set
    /// at read time would put that layer straight back. It also makes "a folder
    /// ticked whose contents are not" impossible to reach by this route, so
    /// there is no third checkbox state for the list to have to draw.
    ///
    /// [`LayerStack::targets`] is untouched by any of this, which is the point:
    /// what a bulk operation reaches is still one rule in one place.
    pub fn pick(&mut self, index: usize, on: bool) {
        for i in self.subtree(index) {
            self.layers[i].picked = on;
        }
    }

    // --- link groups --------------------------------------------------------

    /// Stack positions of every layer in `group`, ascending.
    pub fn group_indices(&self, group: u8) -> Vec<usize> {
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.link == Some(group))
            .map(|(i, _)| i)
            .collect()
    }

    /// The group every one of `indices` is in, where they are all in the same
    /// one — which is what makes the chain button mean "unlink" rather than
    /// "link".
    ///
    /// `None` for an empty list, for a layer in no group, and for a set spread
    /// over two groups. All three are "these are not currently one linked set",
    /// which is the only question the caller asks.
    pub fn shared_group(&self, indices: &[usize]) -> Option<u8> {
        let first = self.layers.get(*indices.first()?)?.link?;
        indices
            .iter()
            .all(|i| self.layers.get(*i).and_then(|l| l.link) == Some(first))
            .then_some(first)
    }

    /// The lowest group number nothing is using, or `None` when all of them
    /// are.
    ///
    /// Lowest rather than next: a group emptied by unlinking gives its number —
    /// and therefore its colour — straight back, so a session of linking and
    /// unlinking does not walk off the end of the palette while most of it
    /// stands unused.
    pub fn free_group(&self) -> Option<u8> {
        (0..Self::LINK_GROUPS as u8).find(|g| self.group_indices(*g).is_empty())
    }

    /// Put `indices` into a link group of their own, returning its number.
    ///
    /// Refused — `None` — for fewer than two layers, because a group of one is
    /// a statement about nothing, and when every group is in use. A layer
    /// already in another group **leaves it**: belonging to two sets that move
    /// independently is not a state the stack can be in.
    pub fn link(&mut self, indices: &[usize]) -> Option<u8> {
        // Counted rather than taken on trust. `link(&[0, 0])` and an index off
        // the end both pass a bare length test and then make the group of one
        // this refuses — and the refusal is the whole reason `unlink` and
        // `remove` have to dissolve one afterwards.
        let mut members: Vec<usize> = indices
            .iter()
            .copied()
            .filter(|i| *i < self.layers.len())
            .collect();
        members.sort_unstable();
        members.dedup();
        let indices = &members[..];
        if indices.len() < 2 {
            return None;
        }
        // Before the writes, so a refusal changes nothing. The layers being
        // linked may be the whole of an existing group, in which case that
        // group is about to be freed and is the one that will be handed
        // back — which is right, and is what makes re-linking the same set
        // keep its colour.
        let group = self
            .shared_group(indices)
            .or_else(|| self.free_group())
            .or_else(|| {
                // Every group is in use, but one of them may consist entirely
                // of layers that are about to leave it.
                (0..Self::LINK_GROUPS as u8)
                    .find(|g| self.group_indices(*g).iter().all(|i| indices.contains(i)))
            })?;
        for index in indices {
            if let Some(layer) = self.layers.get_mut(*index) {
                layer.link = Some(group);
            }
        }
        Some(group)
    }

    /// Take `indices` out of whatever group each is in.
    pub fn unlink(&mut self, indices: &[usize]) {
        for index in indices {
            if let Some(layer) = self.layers.get_mut(*index) {
                layer.link = None;
            }
        }
        self.dissolve_lone_groups();
    }

    /// Take the last member of any group out of it.
    ///
    /// [`LayerStack::link`] refuses to *make* a group of one, so nothing may
    /// leave one behind either — and two things can: unlinking one member of a
    /// pair, and deleting one. A lone member would draw a coloured chain
    /// meaning "moves together with nothing", which is a mark that lies, and it
    /// would hold its number so [`LayerStack::free_group`] could not hand the
    /// colour back — six such strays and the chain button reports every group
    /// in use with no real group anywhere.
    ///
    /// Called from the two places that can shrink a group and from nowhere
    /// else: nothing but a removal reduces one.
    fn dissolve_lone_groups(&mut self) {
        for group in 0..Self::LINK_GROUPS as u8 {
            let members = self.group_indices(group);
            if let [only] = members[..] {
                self.layers[only].link = None;
            }
        }
    }

    /// What a move of the entry at `from` actually carries, ascending.
    ///
    /// **Two ways of saying "these travel together", and one answer.** A link
    /// group carries every other layer of *that group* — not every linked layer
    /// in the document — and a folder carries what is inside it. They compose:
    /// each member is expanded to its whole subtree, so dragging one layer of a
    /// linked pair where the other is a folder brings that folder's contents
    /// too. Written once here rather than as two branches in the move, because
    /// two rules for "what is moving" is how the two come to disagree about a
    /// set that is both.
    fn moving_with(&self, from: usize) -> Vec<usize> {
        let seeds: Vec<usize> = match self.layers.get(from).and_then(|l| l.link) {
            Some(link) => self.group_indices(link),
            None => vec![from],
        };
        let mut members: Vec<usize> = seeds.iter().flat_map(|i| self.subtree(*i)).collect();
        members.sort_unstable();
        members.dedup();
        members
    }

    /// The depth each of `members` would take if the block landed with its
    /// roots at `depth`.
    ///
    /// A *root* is a member no other member encloses. Everything hanging off
    /// one shifts by the same amount as that root, which is what keeps a
    /// folder's shape through a move: its contents stay one level inside it
    /// however far in or out the folder itself travels.
    fn depths_at(&self, members: &[usize], depth: u8) -> Option<Vec<u8>> {
        members
            .iter()
            .map(|i| {
                let root = self
                    .ancestors_of(*i)
                    .filter(|a| members.contains(a))
                    .last()
                    .unwrap_or(*i);
                let root_depth = self.layers[root].depth;
                // Signed, because a folder dragged out to the top level takes
                // its contents *up* by the same delta and `u8` would wrap.
                let shifted = self.layers[*i].depth as i16 + depth as i16 - root_depth as i16;
                (0..=Self::MAX_DEPTH as i16)
                    .contains(&shifted)
                    .then_some(shifted as u8)
            })
            .collect()
    }

    /// Move the entry at `from` so that it sits at position `to`, nested
    /// `depth` levels deep, shifting everything between them along.
    ///
    /// **This is a `Vec` shuffle and nothing else.** A layer's slot is fixed
    /// for its lifetime, so no pixels move and no slot changes hands — which is
    /// exactly why reordering, unlike *deleting*, does not have to clear the
    /// undo history. A `PixelPatch` names a slot; deleting frees one for the
    /// next layer to inherit, and an entry replayed after that would land in
    /// the wrong layer. Nothing here frees or reassigns one, so every patch
    /// still names the pixels it was captured from. **A folder is the same
    /// statement**: it holds no slot at all, so a document gaining, losing or
    /// rearranging folders never invalidates a patch either.
    ///
    /// `depth` is what the moved entry's own nesting becomes — it is how a drag
    /// says "into that folder" rather than "beside it" — and what is inside it
    /// follows. Refused where the result would not describe a tree, where it
    /// would nest past [`LayerStack::MAX_DEPTH`], and where a folder would end
    /// up inside itself; the last needs no test of its own, because a folder's
    /// own subtree is not among the positions left once it has been lifted out.
    ///
    /// Returns `false` where nothing moved — an index off the end, or a drop
    /// that leaves every entry at the position and depth it already had. The
    /// caller wants to know, because a move that did nothing is not a document
    /// modification.
    pub fn reorder_to(&mut self, from: usize, to: usize, depth: u8) -> bool {
        let Some(after) = self.plan_reorder(from, to, depth) else {
            return false;
        };
        // The selection follows the *entry*, not the position it was at. A
        // layer is found again by its slot, which is what it always was; a
        // folder has none, so it is followed by where the rearrangement put it —
        // and `after` names every entry by the index it came from, so one
        // lookup answers for both.
        let was = self.active;
        let mut taken: Vec<Option<Layer>> = self.layers.drain(..).map(Some).collect();
        self.layers = after
            .iter()
            .map(|(i, d)| {
                let mut layer = taken[*i].take().expect("each entry is placed once");
                layer.depth = *d;
                layer
            })
            .collect();
        debug_assert!(
            well_formed(&self.shape_pairs()),
            "reorder left a malformed stack"
        );
        self.active = after
            .iter()
            .position(|(i, _)| *i == was)
            .unwrap_or_else(|| self.active.min(self.layers.len() - 1));
        true
    }

    /// Would [`LayerStack::reorder_to`] do anything?
    ///
    /// What the drag asks before it lights a row up. It is the same decision by
    /// the same code — a mark promising a move the model would then refuse is
    /// the lying control the drop rules exist to prevent — so this is the plan
    /// without the writes, not a second opinion about it.
    pub fn can_reorder(&self, from: usize, to: usize, depth: u8) -> bool {
        self.plan_reorder(from, to, depth).is_some()
    }

    /// What the stack would look like: `(the index each entry came from, the
    /// depth it would take)`, or `None` where the move is refused.
    fn plan_reorder(&self, from: usize, to: usize, depth: u8) -> Option<Vec<(usize, u8)>> {
        if from >= self.layers.len() || to >= self.layers.len() {
            return None;
        }
        let members = self.moving_with(from);
        // A folder cannot be dropped inside itself: the position names one of
        // its own contents, which is about to travel with it.
        //
        // Deliberately the *subtree* and not the whole moving set. A link group
        // carries members that are not inside one another, and dropping one on
        // another of them is an ordinary move to that end of the stack — the
        // insert position is counted among the entries that are staying, so it
        // resolves perfectly well. Testing the whole set here broke exactly
        // that, and `moving_a_linked_layer_carries_the_whole_set` is what said
        // so.
        let span = self.subtree(from);
        if span.contains(&to) && to != from {
            return None;
        }
        let depths = self.depths_at(&members, depth)?;

        // Where the block lands, counted among the entries that are *not*
        // moving: after everything at or before `to` when it is travelling up,
        // before whatever is at `to` when it is travelling down. For a single
        // entry this is exactly `remove(from); insert(to)`.
        //
        // **A drop *into* the folder at `to` is the exception, and it is the
        // gesture the whole depth argument exists for.** A folder sits above
        // its contents, so landing inside it means landing immediately *below*
        // its row — the "insert before" placement, whichever direction the
        // entry travelled from. Without this, dropping onto a folder's own row
        // from below put the entry above the folder at a depth the folder no
        // longer enclosed, `well_formed` refused it, and nesting was reachable
        // only by aiming at a row already inside the group — which left an
        // *empty* folder impossible to fill by dragging at all.
        let into = self
            .layers
            .get(to)
            .is_some_and(|t| t.folder && depth > t.depth);
        let after = to > from && !into;
        let at = (0..self.layers.len())
            .filter(|i| !members.contains(i))
            .filter(|i| if after { *i <= to } else { *i < to })
            .count();

        // The whole result, as (which entry, what depth), judged before a byte
        // is written so that a refusal changes nothing — see the module docs on
        // well-formedness. It is also what answers "did anything move": a drop
        // that reproduces the order and the depths the stack already has is not
        // an edit, however it was expressed.
        let mut after: Vec<(usize, u8)> = (0..self.layers.len())
            .filter(|i| !members.contains(i))
            .map(|i| (i, self.layers[i].depth))
            .collect();
        after.splice(at..at, members.iter().copied().zip(depths.iter().copied()));
        let now: Vec<(usize, u8)> = (0..self.layers.len())
            .map(|i| (i, self.layers[i].depth))
            .collect();
        if after == now {
            return None;
        }
        let shape: Vec<(u8, bool)> = after
            .iter()
            .map(|(i, d)| (*d, self.layers[*i].folder))
            .collect();
        well_formed(&shape).then_some(after)
    }

    /// Move an entry to `to`, keeping the nesting it already has.
    ///
    /// The two-argument form every caller written before folders speaks, and
    /// the one [`LayerStack::move_up`] and [`LayerStack::move_down`] are
    /// written in terms of.
    pub fn reorder(&mut self, from: usize, to: usize) -> bool {
        let Some(depth) = self.layers.get(from).map(|l| l.depth) else {
            return false;
        };
        self.reorder_to(from, to, depth)
    }

    /// Move an entry one step towards the top. Returns its new index.
    ///
    /// A step is over the *neighbour*, and a folder's contents are not a
    /// neighbour — so the step is expressed against the subtree's own bounds
    /// rather than against `index ± 1`, which for a folder would name one of
    /// its own children. `None` where there is nowhere to go, which now
    /// includes the top of a folder: leaving one is a change of nesting, and
    /// this button does not carry a depth. Dragging says it instead.
    pub fn move_up(&mut self, index: usize) -> Option<usize> {
        let span = self.subtree(index);
        if span.end >= self.layers.len() {
            return None;
        }
        // A step is a reorder over one place. Written in terms of it rather
        // than as its own swap so there is one piece of code keeping the
        // selection with its layer, instead of three that have to agree.
        self.reorder(index, span.end).then_some(index + 1)
    }

    /// Move an entry one step towards the bottom. Returns its new index.
    pub fn move_down(&mut self, index: usize) -> Option<usize> {
        let span = self.subtree(index);
        if span.start == 0 || index >= self.layers.len() {
            return None;
        }
        self.reorder(index, span.start - 1).then_some(index - 1)
    }

    /// True when at least one layer would contribute to the composite.
    ///
    /// Through [`LayerStack::effective_visible`], so a stack whose every layer
    /// is inside a hidden folder correctly reports that nothing shows. Folders
    /// themselves are not counted: one holds no pixels, so a document of
    /// nothing but folders shows nothing however many eyes are open.
    pub fn any_visible(&self) -> bool {
        (0..self.layers.len())
            .filter(|i| !self.layers[*i].folder)
            .any(|i| self.effective_visible(i) && self.layers[i].opacity > 0.0)
    }

    // --- structural undo ----------------------------------------------------

    /// The stack as it stands: every entry [`ShapeEntry::Kept`], no layers held
    /// and no mask recorded.
    ///
    /// This is what a structural edit snapshots **before** it runs. What the
    /// edit removes is folded in afterwards by [`StackShape::with_removed`],
    /// because the layers do not exist to be held until the operation has
    /// handed them over.
    ///
    /// `slice_bytes` is what one layer slice costs — `Document::layer_bytes` —
    /// and it is taken here rather than being left out because a shape holding
    /// a deleted layer is holding a **canvas-sized texture slice**, which is
    /// the whole cost of this design and is invisible in the count of entries.
    /// See [`StackShape::byte_len`]. The stack cannot work it out: it holds
    /// slots, and the canvas belongs to the document.
    pub fn shape(&self, slice_bytes: u64) -> StackShape {
        StackShape {
            entries: self
                .layers
                .iter()
                .map(|l| ShapeEntry::Kept {
                    id: l.id,
                    depth: l.depth,
                })
                .collect(),
            active: self.layers.get(self.active).map_or(0, |l| l.id),
            masks: Vec::new(),
            slice_bytes,
        }
    }

    /// The shape the stack had **before** the entry `made` was added, derived
    /// from the stack as it stands now.
    ///
    /// [`LayerStack::shape`]'s twin for one caller: a text placement, which adds
    /// its layer when the artist presses Place and records the entry when the
    /// float is put down, several seconds and a whole drag later. The obvious
    /// spelling is to snapshot `shape` at the add and carry it to the commit,
    /// which is what [`LayerStack::add`]'s own caller does — and it would be
    /// carrying a **stale** description of a stack the gesture had time to
    /// change. Deriving it at the commit cannot be stale, because it is read off
    /// what is there.
    ///
    /// `was_active` is the entry that was selected before the add, by id, so an
    /// undo puts the selection back where the artist left it rather than on
    /// whatever happens to sit where the text layer was. An id no longer in the
    /// stack is harmless: [`LayerStack::restore_shape`] falls back on the first
    /// row, which is what an unknown id has always meant there.
    ///
    /// `None` where `made` is not in the stack. That is not a state any live
    /// path produces — every route that removes an entry commits the float
    /// first — and answering it rather than asserting is what keeps a caller
    /// that reached it recording an ordinary pixel entry instead of a shape
    /// naming an entry that is gone, which [`LayerStack::restore_shape`] would
    /// refuse whole.
    pub fn shape_before_add(
        &self,
        made: u32,
        was_active: u32,
        slice_bytes: u64,
    ) -> Option<StackShape> {
        if !self.layers.iter().any(|l| l.id == made) {
            return None;
        }
        Some(StackShape {
            entries: self
                .layers
                .iter()
                .filter(|l| l.id != made)
                .map(|l| ShapeEntry::Kept {
                    id: l.id,
                    depth: l.depth,
                })
                .collect(),
            active: was_active,
            masks: Vec::new(),
            slice_bytes,
        })
    }

    /// The same, also recording the mask the entry at `index` has now.
    ///
    /// For the two edits that change one — adding a mask and taking one off.
    /// The claim is *cloned*, so the slice stays alive when the layer's own
    /// copy is dropped; that is the whole of how removing a mask stopped
    /// clearing the history.
    pub fn shape_with_mask(&self, index: usize, slice_bytes: u64) -> StackShape {
        let mut shape = self.shape(slice_bytes);
        if let Some(layer) = self.layers.get(index) {
            shape.masks.push((layer.id, layer.mask.clone()));
        }
        shape
    }

    /// Make the stack the shape `target` describes, and hand back the shape it
    /// had — which is exactly what putting this edit back would be.
    ///
    /// One function, called twice, so undo and redo cannot be two
    /// implementations that disagree; the same arrangement `render_float` keeps
    /// for the transform's preview and its commit.
    ///
    /// **It restores shape, not values.** A [`ShapeEntry::Kept`] carries an id
    /// and a depth and nothing else, so undoing a reorder cannot revert an
    /// opacity dragged afterwards — which it would if an entry were a snapshot
    /// of the whole `Vec`. A [`ShapeEntry::Gone`] carries the whole layer,
    /// and may: it has not been in the stack to be changed.
    ///
    /// Entries currently in the stack that `target` does not name are removed
    /// and come back in the returned shape as `Gone`, holding their slices.
    /// That is what parks a layer an undo has just taken out.
    pub fn restore_shape(&mut self, target: StackShape) -> StackShape {
        // **Judged before a byte moves**, the rule the module docs state for
        // every other structural mutation. A `Kept` row naming an entry that is
        // not in the stack cannot happen — the stepped-not-seeked guarantee
        // says an older shape is only ever reached with everything above it
        // already undone — but a *half* rebuilt stack is the one outcome worse
        // than nothing happening, and in a release build a `debug_assert` is
        // not there to stop it. Handing the shape straight back makes the undo
        // a no-op: the entry stays on the other stack and the document is
        // untouched, which is a step that appears to do nothing rather than a
        // layer that silently vanishes or a panic on the undo path.
        let present = |id: u32| self.layers.iter().any(|l| l.id == id);
        let placeable = target.entries.iter().all(|e| match e {
            ShapeEntry::Kept { id, .. } => present(*id),
            ShapeEntry::Gone { .. } => true,
        }) && target.masks.iter().all(|(id, _)| {
            present(*id)
                || target
                    .entries
                    .iter()
                    .any(|e| matches!(e, ShapeEntry::Gone { layer } if layer.id == *id))
        });
        if !placeable {
            debug_assert!(false, "a recorded shape named an entry that is gone");
            return target;
        }

        // What the stack is now, in its own order, before anything moves.
        let was: Vec<(u32, u8)> = self.layers.iter().map(|l| (l.id, l.depth)).collect();
        let was_active = self.layers.get(self.active).map_or(0, |l| l.id);

        let mut held: Vec<Option<Layer>> = self.layers.drain(..).map(Some).collect();
        let mut find = |id: u32| -> Option<Layer> {
            held.iter_mut()
                .find(|l| l.as_ref().is_some_and(|l| l.id == id))
                .and_then(Option::take)
        };

        for entry in target.entries {
            match entry {
                ShapeEntry::Kept { id, depth } => {
                    // Checked above, so this arm is unreachable; a shape naming
                    // one entry twice is the only way in and `shape` cannot
                    // build one, because a stack holds each id once.
                    match find(id) {
                        Some(mut layer) => {
                            layer.depth = depth;
                            self.layers.push(layer);
                        }
                        None => {
                            debug_assert!(false, "a recorded shape named an entry twice")
                        }
                    }
                }
                ShapeEntry::Gone { layer } => self.layers.push(layer),
            }
        }

        // Everything the target did not name leaves the stack and is parked in
        // the shape that undoes this.
        let mut left: Vec<Layer> = held.into_iter().flatten().collect();
        let mut back = StackShape {
            entries: was
                .iter()
                .map(|(id, depth)| ShapeEntry::Kept {
                    id: *id,
                    depth: *depth,
                })
                .collect(),
            active: was_active,
            masks: Vec::new(),
            // Carried across rather than re-derived: the canvas cannot have
            // changed under a history, because a resize clears it.
            slice_bytes: target.slice_bytes,
        };
        back.adopt(&mut left);
        debug_assert!(left.is_empty(), "a removed entry was not in the shape");

        // The masks, swapped the same way round, so redo puts back exactly what
        // undo took off.
        for (id, mask) in target.masks {
            let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else {
                debug_assert!(false, "a recorded mask named an entry that is gone");
                continue;
            };
            let had = std::mem::replace(&mut layer.mask, mask);
            back.masks.push((id, had));
        }

        self.active = self
            .layers
            .iter()
            .position(|l| l.id == target.active)
            .unwrap_or(0)
            .min(self.layers.len().saturating_sub(1));
        debug_assert!(
            well_formed(&self.shape_pairs()),
            "restoring a recorded shape left a malformed stack"
        );
        // A group can have lost a member while it was out of the stack, and can
        // regain one coming back; both are `link`'s refusal to make a group of
        // one, from the other side.
        //
        // **A known defect lives here, and it is not this call's to fix.**
        // Deleting one of a linked pair dissolves the group — `remove_many`
        // does it, correctly, because the survivor would otherwise draw a chain
        // meaning "moves together with nothing" and hold a colour `free_group`
        // could never hand back. Undoing that delete brings the deleted layer
        // back still carrying its own `link`, finds itself alone in the group,
        // and this call clears that too. So an undone delete silently unlinks a
        // pair. Removing the call is not the repair: it would leave the group
        // of one the model refuses to create.
        //
        // The repair is to record what the removal dissolved and swap it back,
        // exactly as `masks` is — a link is changed by the *edit* rather than by
        // the artist afterwards, so it is the mask's case and not the opacity's,
        // and §4's "restores shape, not values" does not cover it. It needs
        // `dissolve_lone_groups` to report what it cleared and `with_removed` to
        // take it, which is a signature change to `remove_many`; putting links
        // on every `Kept` row instead is the tempting shortcut and is the thing
        // §4 and §10 both refuse, because an undone *reorder* would then revert
        // a link somebody made after it.
        self.dissolve_lone_groups();
        back
    }

    /// [`well_formed`]'s input for the stack as it stands.
    ///
    /// Named apart from [`LayerStack::shape`], which now answers a different
    /// question — this one is the depth sequence and that one is the record a
    /// structural undo entry holds.
    fn shape_pairs(&self) -> Vec<(u8, bool)> {
        self.layers.iter().map(|l| (l.depth, l.folder)).collect()
    }

    fn next_name_number(&self) -> usize {
        // Not the layer count: deleting "Layer 2" of three should not make the
        // next new layer collide with the existing "Layer 3".
        let highest = self
            .layers
            .iter()
            .filter_map(|l| l.name.strip_prefix("Layer "))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        highest + 1
    }
}

/// One position in a recorded stack shape.
#[derive(Clone, Debug)]
enum ShapeEntry {
    /// An entry that survived the edit. Put it back here, at this depth, and
    /// **leave everything else about it alone** — its opacity may have been
    /// changed since, and that is not this entry's to revert.
    Kept { id: u32, depth: u8 },
    /// An entry the edit removed. Put the whole layer back: nothing could have
    /// changed it, because it has not been in the stack to be changed.
    ///
    /// This is also what parks the slice. The [`Layer`] owns its
    /// [`SlotClaim`], so holding it here is what stops the number being handed
    /// to the next layer that asks — no copy, no readback, and no pixel path
    /// involved in a structural undo at all.
    Gone { layer: Layer },
}

/// The stack as it was before one structural edit.
///
/// What an undo entry holds, and the argument for its shape is in
/// `docs/structural-undo.md`. Two things it is deliberately not:
///
/// * **Not a snapshot of the `Vec`.** See [`ShapeEntry::Kept`]: an undo of a
///   reorder that reverted an opacity somebody set afterwards would be an undo
///   damaging something it was never asked about.
/// * **Not a per-operation inverse.** One recorded shape covers a delete of a
///   whole folder in one step, with no index arithmetic — which is where
///   `remove_many`'s reverse loop once deleted a layer nobody ticked — and it
///   is well formed because it was well formed when it was recorded.
#[derive(Clone, Debug)]
pub struct StackShape {
    entries: Vec<ShapeEntry>,
    /// Which entry was selected, **by [`Layer::id`]**: the selection follows
    /// the layer, which is the rule [`LayerStack::reorder_to`] already keeps.
    active: u32,
    /// The mask each named entry had, for the two edits that change one.
    ///
    /// Deliberately not a field of [`ShapeEntry::Kept`], which restores shape
    /// and not values — a `Kept` row carrying a mask would take masks *off*
    /// every layer whenever a reorder was undone. It cannot simply be left out
    /// either, the way an opacity is: a mask is a *slice*, so taking one off
    /// frees storage, and that is the whole reason removing one used to clear
    /// the history.
    masks: Vec<(u32, Option<SlotClaim>)>,
    /// What one layer slice costs, from `Document::layer_bytes`.
    ///
    /// Carried so [`StackShape::byte_len`] can charge for the slices this shape
    /// is holding. See that method: it is the whole cost of the design and it
    /// is invisible in the count of entries.
    slice_bytes: u64,
}

impl StackShape {
    /// Fold the entries an edit removed into this shape, so it holds them —
    /// and therefore their slices — rather than merely naming them.
    ///
    /// Called with what [`LayerStack::remove_many`] handed back, on a shape
    /// taken immediately before it ran.
    pub fn with_removed(mut self, mut removed: Vec<Layer>) -> Self {
        self.adopt(&mut removed);
        debug_assert!(
            removed.is_empty(),
            "a removed entry was not in the shape recorded before the removal"
        );
        self
    }

    /// Turn every `Kept` row naming one of `layers` into a `Gone` holding it,
    /// taking those layers out of the list.
    fn adopt(&mut self, layers: &mut Vec<Layer>) {
        for entry in &mut self.entries {
            let ShapeEntry::Kept { id, .. } = entry else {
                continue;
            };
            if let Some(at) = layers.iter().position(|l| l.id == *id) {
                *entry = ShapeEntry::Gone {
                    layer: layers.remove(at),
                };
            }
        }
    }

    /// How many entries the stack had.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// What this costs, which is what the undo budget counts.
    ///
    /// **Dominated by the slices, not by the entries**, and getting that
    /// backwards is the trap this design walks straight into. The shape itself
    /// is tens of bytes a row; what a `Gone` row *holds* is a claim on a
    /// canvas-sized texture slice, which is 16 MB at 2048² and 400 MB at
    /// 10000². Counting only the rows would leave the one figure the user sets
    /// to bound undo blind to the whole cost of the feature, and a session of
    /// deleting and adding layers would walk the layer array to its 256-slice
    /// ceiling — 4.29 GB at 2048², 102.4 GB at 10000² — with the budget
    /// reporting a few kilobytes. `LayerStack::slot_capacity_needed` never
    /// falls while a slice is parked, and `CanvasRenderer::ensure_slots` never
    /// shrinks, so that memory is allocated and stays allocated.
    ///
    /// Charging for them puts parked slices in the same currency as a patch,
    /// which is the honest one: on a 10000² canvas the 512 MB budget holds one
    /// parked layer exactly as it holds one full-canvas stroke, and
    /// `evict_to_budget` gives the slice back on the second. The slice ceiling
    /// stays as the hard backstop it always was; what this stops is the ceiling
    /// being the *only* bound.
    ///
    /// A mask is another slice of the same array, so it is charged the same.
    pub fn byte_len(&self) -> usize {
        let slices: u64 = self
            .entries
            .iter()
            .map(|e| match e {
                ShapeEntry::Kept { .. } => 0,
                // Its own, and its mask's: a masked layer parks two slices.
                ShapeEntry::Gone { layer } => {
                    u64::from(layer.slot.is_some()) + u64::from(layer.mask.is_some())
                }
            })
            .chain(self.masks.iter().map(|(_, m)| u64::from(m.is_some())))
            .sum();
        let held = slices.saturating_mul(self.slice_bytes);
        self.entries.len() * std::mem::size_of::<ShapeEntry>()
            + self
                .entries
                .iter()
                .map(|e| match e {
                    ShapeEntry::Kept { .. } => 0,
                    // The layer's own bytes plus what it owns on the heap. All
                    // of it is noise beside a parked slice, and all of it is
                    // counted for the reason `name.len()` always was: a figure
                    // that skips the parts it thinks are small is one nobody can
                    // check. `capacity`, not `len`, because the block held is the
                    // capacity — and `plan_set_effect` builds the vector by
                    // `collect` and then `push`es, so the two routinely differ.
                    // The text record is charged on that same argument, and it is
                    // the one thing parked here with no bound of the canvas's:
                    // its only bound is `textobj::MAX_RECORD_BYTES`.
                    ShapeEntry::Gone { layer } => {
                        std::mem::size_of::<Layer>()
                            + layer.name.len()
                            + layer.effects.capacity() * std::mem::size_of::<Effect>()
                            + layer.text_bytes()
                    }
                })
                .sum::<usize>()
            + self.masks.len() * std::mem::size_of::<(u32, Option<SlotClaim>)>()
            + usize::try_from(held).unwrap_or(usize::MAX)
    }
}

/// Background shown beneath the bottom layer. Currently always transparent;
/// exists so a white-paper document mode is an additive change.
pub const DEFAULT_BACKGROUND: Color = Color::TRANSPARENT;

#[cfg(test)]
mod tests {
    use super::*;

    /// A layer's slice, for the tests that compare stacks by slot.
    ///
    /// `Layer::slot` is an `Option` because a folder holds none; these tests
    /// are all about layers, and unwrapping at the edge keeps them readable
    /// rather than turning every comparison into one over `Option`s.
    fn slot_of(layer: &Layer) -> u32 {
        layer
            .slot()
            .expect("a layer has a slice; a folder is not a layer")
    }

    /// Every slice a removal gave back, which for a layer is one — or two,
    /// where it had a mask.
    fn removed(stack: &mut LayerStack, index: usize) -> Option<u32> {
        stack
            .remove(index)
            .and_then(|gone| gone.first().and_then(Layer::slot))
    }

    #[test]
    fn a_new_stack_has_one_active_layer() {
        let s = LayerStack::new();
        assert_eq!(s.len(), 1);
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.active_slot(), Some(0));
    }

    #[test]
    fn adding_inserts_above_active_and_selects_it() {
        let mut s = LayerStack::new();
        let slot = s.add().unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(s.active_index(), 1, "new layer should be selected");
        assert_eq!(slot, 1);
    }

    #[test]
    fn slots_are_stable_across_reordering() {
        // The whole point of slots: moving a layer must not move its pixels.
        let mut s = LayerStack::new();
        s.add();
        let bottom_slot = slot_of(s.get(0).unwrap());
        let top_slot = slot_of(s.get(1).unwrap());

        s.move_down(1);

        assert_eq!(slot_of(s.get(0).unwrap()), top_slot);
        assert_eq!(slot_of(s.get(1).unwrap()), bottom_slot);
    }

    #[test]
    fn moving_a_layer_keeps_the_same_layer_active() {
        let mut s = LayerStack::new();
        s.add();
        s.set_active(0);
        let slot_before = s.active_slot();
        s.move_up(0);
        assert_eq!(s.active_index(), 1);
        assert_eq!(
            s.active_slot(),
            slot_before,
            "selection followed the wrong layer"
        );
    }

    #[test]
    fn moving_past_the_ends_is_a_no_op() {
        let mut s = LayerStack::new();
        s.add();
        assert!(s.move_up(1).is_none());
        assert!(s.move_down(0).is_none());
        assert_eq!(s.len(), 2);
    }

    /// Reordering must be a shuffle of the order and nothing else. A patch
    /// names a slot, so a slot that followed a *position* would make every
    /// recorded patch in the history wrong the moment anything was dragged.
    #[test]
    fn reordering_preserves_every_layers_slot() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        let mut slots: Vec<u32> = s.layers().iter().map(slot_of).collect();
        assert_eq!(slots.len(), 4);

        // Bottom to top, which is the longest move there is.
        s.reorder(0, 3);
        let moved = slots.remove(0);
        slots.push(moved);
        assert_eq!(s.layers().iter().map(slot_of).collect::<Vec<_>>(), slots);

        // And back down again, past two layers rather than to an end.
        s.reorder(3, 1);
        let moved = slots.remove(3);
        slots.insert(1, moved);
        assert_eq!(s.layers().iter().map(slot_of).collect::<Vec<_>>(), slots);
        // Every slot still present exactly once: nothing was freed or reissued.
        let unique: std::collections::HashSet<_> = slots.iter().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn reordering_a_layer_onto_its_own_position_is_a_no_op() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.set_active(1);
        let before: Vec<u32> = s.layers().iter().map(slot_of).collect();

        assert!(
            !s.reorder(1, 1),
            "a move to where it already is moved nothing"
        );
        assert_eq!(s.layers().iter().map(slot_of).collect::<Vec<_>>(), before);
        assert_eq!(s.active_index(), 1);
    }

    #[test]
    fn reordering_off_the_end_moves_nothing() {
        let mut s = LayerStack::new();
        s.add();
        let before: Vec<u32> = s.layers().iter().map(slot_of).collect();
        assert!(!s.reorder(0, 2));
        assert!(!s.reorder(7, 0));
        assert_eq!(s.layers().iter().map(slot_of).collect::<Vec<_>>(), before);
    }

    /// The selection is a layer, not a row number: whichever layer was being
    /// painted on has to still be the one being painted on afterwards.
    #[test]
    fn the_active_layer_follows_the_layer_and_not_the_position() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();

        // The moved layer itself.
        s.set_active(0);
        let slot = s.active_slot();
        s.reorder(0, 2);
        assert_eq!(s.active_index(), 2);
        assert_eq!(s.active_slot(), slot);

        // A layer the move steps over: it shifts by one, the other way.
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        s.set_active(2);
        let slot = s.active_slot();
        s.reorder(0, 3);
        assert_eq!(s.active_index(), 1, "stepped over, so down one");
        assert_eq!(s.active_slot(), slot);

        s.reorder(3, 0);
        assert_eq!(s.active_index(), 2, "stepped over the other way, so up one");
        assert_eq!(s.active_slot(), slot);

        // And a layer outside the span the move covers does not move at all.
        s.set_active(3);
        let slot = s.active_slot();
        s.reorder(0, 1);
        assert_eq!(s.active_index(), 3);
        assert_eq!(s.active_slot(), slot);
    }

    /// The buttons are `reorder` over one place, so they must still behave
    /// exactly as they did when each did its own swap.
    #[test]
    fn a_step_is_a_reorder_over_one_place() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        let slots: Vec<u32> = s.layers().iter().map(slot_of).collect();

        assert_eq!(s.move_up(0), Some(1));
        assert_eq!(
            s.layers().iter().map(slot_of).collect::<Vec<_>>(),
            vec![slots[1], slots[0], slots[2]]
        );
        assert_eq!(s.move_down(1), Some(0));
        assert_eq!(s.layers().iter().map(slot_of).collect::<Vec<_>>(), slots);
    }

    #[test]
    fn the_last_layer_cannot_be_removed() {
        let mut s = LayerStack::new();
        assert!(s.remove(0).is_none(), "a document needs somewhere to paint");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn removing_recycles_the_slot() {
        let mut s = LayerStack::new();
        let added = s.add().unwrap();
        let freed = removed(&mut s, 1).unwrap();
        assert_eq!(freed, added);

        let reused = s.add().unwrap();
        assert_eq!(reused, added, "freed slots should be reused before growing");
        assert_eq!(
            s.slot_capacity_needed(),
            2,
            "recycling must not grow the texture array"
        );
    }

    #[test]
    fn removing_the_active_layer_keeps_the_selection_in_range() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.set_active(2);
        s.remove(2);
        assert!(s.active_index() < s.len());
    }

    #[test]
    fn new_layer_names_do_not_collide_after_deletion() {
        let mut s = LayerStack::new();
        s.add(); // Layer 2
        s.add(); // Layer 3
        s.remove(1); // delete Layer 2
        s.add();
        let names: Vec<&str> = s.layers().iter().map(|l| l.name.as_str()).collect();
        let unique: std::collections::HashSet<_> = names.iter().collect();
        assert_eq!(
            names.len(),
            unique.len(),
            "duplicate layer names: {names:?}"
        );
    }

    // --- masks -------------------------------------------------------------

    /// The two ceilings are different numbers and have to stay so: a mask
    /// occupies a slice and no stack entry, so a full stack of masked layers
    /// needs twice `MAX` slices, plus the one a floating transform previews
    /// into, plus the effect-draw headroom above that. `MAX_SLOTS` in
    /// `umber-render` is the same arithmetic and caps the texture array;
    /// conflating either with `MAX` would have quietly halved how many layers
    /// could carry a mask.
    ///
    /// It asks `has_headroom` rather than `slot_capacity_needed() <
    /// MAX_SLOTS`, and **that is readability and not strength**: `has_headroom`
    /// *is* `next < MAX_SLOTS` and `slot_capacity_needed` returns `next`, so
    /// the two are the identical comparison on the identical field. What it
    /// buys is that the assertion is spelled as the question `begin_float`
    /// asks. An earlier draft of this comment claimed the swap made the test
    /// harder to satisfy, which was false — the check is exactly as slack
    /// either way, and it would still pass with the float's `+ 1` removed from
    /// the ceiling. That `+ 1` is pinned in `umber-render`'s
    /// `the_slice_ceiling_agrees_with_umber_core`, which can call the
    /// derivation; nothing reachable from here can.
    #[test]
    fn the_slot_ceiling_covers_a_fully_masked_stack_and_the_floats_spare() {
        let mut s = LayerStack::new();
        let room = s.room();
        while s.len() < LayerStack::MAX {
            s.add().unwrap();
        }
        for i in 0..s.len() {
            s.add_mask(i).expect("every layer can take a mask");
        }
        assert_eq!(s.slot_capacity_needed(), LayerStack::MAX as u32 * 2);
        assert!(
            room.has_headroom(),
            "a fully masked stack left a floating transform nowhere to preview"
        );
    }

    /// A mask is another slice of the same array, so it comes off the same free
    /// list — and a layer without one costs nothing at all, which is the whole
    /// reason it is an `Option` rather than a slice reserved per layer.
    #[test]
    fn a_mask_takes_a_slot_of_its_own_and_gives_it_back() {
        let mut s = LayerStack::new();
        assert_eq!(s.slot_capacity_needed(), 1, "an unmasked layer costs one");
        assert_eq!(s.active_mask(), None);

        let mask = s.add_mask(0).expect("a layer with no mask can gain one");
        assert_eq!(mask, 1);
        assert_eq!(s.active_mask(), Some(1));
        assert_eq!(s.slot_capacity_needed(), 2);
        assert_eq!(s.add_mask(0), None, "a second mask is not a thing");

        assert_eq!(
            s.remove_mask(0).map(|claim| claim.number()),
            Some(1),
            "the claim comes back so a caller can park it"
        );
        assert_eq!(s.active_mask(), None);
        assert_eq!(
            s.add().unwrap(),
            1,
            "the freed mask slice must be reused before the array grows"
        );
        assert_eq!(s.slot_capacity_needed(), 2);
    }

    /// The prediction a caller reserves storage from has to be what actually
    /// happens, in **both** directions: a claim off the free list must ask for
    /// nothing, and a claim off the top must ask for exactly one more.
    ///
    /// This measures the answer against the observation rather than restating
    /// the rule — the prediction is taken, the claim is then made, and the two
    /// are compared. Written the tempting way, `slot_capacity_needed() + 1`,
    /// the second half fails: a stack with a parked slice on its free list
    /// would grow the texture array by a slice nobody would ever use, which at
    /// 10000² is 400 MB.
    #[test]
    fn the_reservation_for_one_more_slice_is_what_a_claim_actually_needs() {
        let mut s = LayerStack::new();

        // Nothing free, so the next claim is off the top and needs one more.
        let predicted = s.slot_capacity_after_one_claim();
        s.add().expect("a second layer");
        assert_eq!(
            s.slot_capacity_needed(),
            predicted,
            "a claim off the top needs one slice more than the array holds"
        );
        assert_eq!(predicted, 2, "slots 0 and 1 are claimed");

        // A third, then park the **middle** slice. Parking the highest would be
        // compacted away by `give_back` and leave the free list empty again,
        // which is the case above rather than this one.
        s.add().expect("a third layer");
        let middle = (0..s.len())
            .find(|i| s.get(*i).and_then(Layer::slot) == Some(1))
            .expect("some layer holds slot 1");
        assert_eq!(removed(&mut s, middle), Some(1));
        let mark = s.slot_capacity_needed();
        assert_eq!(mark, 3, "the mark does not fall for a gap below the top");

        // A claim now fills that gap, so nothing more may be reserved for it.
        let predicted = s.slot_capacity_after_one_claim();
        assert_eq!(
            predicted, mark,
            "reserving mark + 1 here would grow the array for a slice nobody claims"
        );
        assert_eq!(s.add(), Some(1), "the parked slice is what the claim takes");
        assert_eq!(
            s.slot_capacity_needed(),
            predicted,
            "a claim off the free list needs no more storage than the array already has"
        );
    }

    /// Deleting a masked layer has to give **both** slices back. Leaking the
    /// mask's would grow the texture array by one slice per masked layer
    /// deleted, for the rest of the session.
    #[test]
    fn deleting_a_masked_layer_frees_its_mask_too() {
        let mut s = LayerStack::new();
        s.add();
        let mask = s.add_mask(1).unwrap();
        let slot = slot_of(s.get(1).unwrap());
        // Measured *before* the removal: what "the array grew" means is that
        // re-adding needed a slice the document had never used.
        let capacity = s.slot_capacity_needed();
        assert_eq!(removed(&mut s, 1), Some(slot));

        // Both come back, in some order, before the array grows.
        let a = s.add().unwrap();
        let b = s.add_mask(s.active_index()).unwrap();
        assert_eq!(s.slot_capacity_needed(), capacity, "the array grew");
        let mut freed = [a, b];
        freed.sort_unstable();
        let mut expected = [slot, mask];
        expected.sort_unstable();
        assert_eq!(freed, expected);
    }

    /// A mask belongs to its layer and follows it, exactly as the layer's own
    /// slice does — otherwise reordering would have to move pixels, which is
    /// the one thing slots exist to avoid.
    #[test]
    fn a_mask_follows_its_layer_through_a_reorder() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        let mask = s.add_mask(2).unwrap();
        s.reorder(2, 0);
        assert_eq!(s.mask_at(0), Some(mask));
        assert_eq!(s.mask_at(1), None);
        assert_eq!(s.mask_at(2), None);
    }

    // --- locking -----------------------------------------------------------

    #[test]
    fn a_lock_is_read_off_the_layer_the_edit_would_land_on() {
        let mut s = LayerStack::new();
        s.add();
        assert!(!s.active_is_locked());
        assert!(!s.any_locked());

        s.get_mut(0).unwrap().locked = true;
        assert!(s.any_locked(), "the flip has to see a lock anywhere");
        assert!(
            !s.active_is_locked(),
            "locking one layer must not stop painting on another"
        );
        s.set_active(0);
        assert!(s.active_is_locked());
        assert!(!s.locked_at(9), "an index off the end is not locked");
    }

    // --- linking -----------------------------------------------------------

    /// The one thing linking drives today: moving a linked layer carries the
    /// rest of the set with it, and lands them contiguously.
    #[test]
    fn moving_a_linked_layer_carries_the_whole_set() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        // Slots 0..3 at positions 0..3. Link the bottom and the top.
        let slots: Vec<u32> = s.layers().iter().map(slot_of).collect();
        assert_eq!(s.link(&[0, 3]), Some(0));

        // Drag the bottom one to the top; its partner comes too, and the two
        // arrive side by side in the order they were already in.
        assert!(s.reorder(0, 3));
        assert_eq!(
            s.layers().iter().map(slot_of).collect::<Vec<_>>(),
            vec![slots[1], slots[2], slots[0], slots[3]]
        );
    }

    /// An unlinked layer moves alone even when other layers are linked to each
    /// other — the set is what is linked, not the stack.
    #[test]
    fn an_unlinked_layer_moves_alone() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        let slots: Vec<u32> = s.layers().iter().map(slot_of).collect();
        s.link(&[0, 1]);

        assert!(s.reorder(2, 0));
        assert_eq!(
            s.layers().iter().map(slot_of).collect::<Vec<_>>(),
            vec![slots[2], slots[0], slots[1]]
        );
    }

    /// The whole point of groups: a layer carries *its own* set, not every
    /// linked layer in the document. Before groups this test could not have
    /// been written, because there was only one set.
    #[test]
    fn a_move_carries_the_layers_own_group_and_no_other() {
        let mut s = LayerStack::new();
        for _ in 0..5 {
            s.add();
        }
        let slots: Vec<u32> = s.layers().iter().map(slot_of).collect();
        assert_eq!(s.link(&[0, 1]), Some(0));
        assert_eq!(s.link(&[3, 4]), Some(1), "a second, independent group");

        // Drag the bottom of the first group to the top. Its partner comes;
        // the other group stays exactly where it was.
        assert!(s.reorder(0, 5));
        assert_eq!(
            s.layers().iter().map(slot_of).collect::<Vec<_>>(),
            vec![slots[2], slots[3], slots[4], slots[5], slots[0], slots[1]]
        );
        assert_eq!(
            s.group_indices(1),
            vec![1, 2],
            "the other group did not move"
        );
    }

    /// The selection still follows the layer it was on when a group moves —
    /// the same promise `the_active_layer_follows_the_layer_and_not_the_
    /// position` makes for a single one.
    #[test]
    fn the_selection_follows_its_layer_through_a_group_move() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        s.link(&[0, 1]);
        s.set_active(2);
        let slot = s.active_slot();

        s.reorder(0, 3);
        assert_eq!(s.active_slot(), slot);
        assert_eq!(s.active_index(), 0, "two layers moved up past it");
    }

    /// What the chain button asks before it decides whether it links or
    /// unlinks. All three "no" answers mean the same thing to the caller:
    /// these are not currently one linked set.
    #[test]
    fn a_set_shares_a_group_only_when_every_one_of_them_is_in_it() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        assert_eq!(s.shared_group(&[]), None, "nothing is not a set");
        assert_eq!(s.shared_group(&[0, 1]), None, "unlinked layers");

        s.link(&[0, 1]);
        assert_eq!(s.shared_group(&[0, 1]), Some(0));
        assert_eq!(
            s.shared_group(&[0]),
            Some(0),
            "one layer of a group is in it"
        );
        assert_eq!(s.shared_group(&[0, 2]), None, "one of them is in no group");

        s.link(&[2, 3]);
        assert_eq!(s.shared_group(&[1, 2]), None, "two different groups");
    }

    /// A group of one says nothing, and a layer cannot be in two sets that
    /// move independently.
    #[test]
    fn linking_needs_two_layers_and_takes_them_out_of_their_old_groups() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        assert_eq!(
            s.link(&[1]),
            None,
            "a group of one is a statement about nothing"
        );
        assert_eq!(s.link(&[]), None);

        assert_eq!(s.link(&[0, 1]), Some(0));
        assert_eq!(
            s.link(&[1, 2]),
            Some(1),
            "a new group, and 1 leaves the old"
        );
        assert_eq!(s.group_indices(0), vec![0]);
        assert_eq!(s.group_indices(1), vec![1, 2]);
    }

    /// Nothing may leave a group of one standing, because `link` refuses to
    /// make one: a lone member draws a chain meaning "moves together with
    /// nothing", and it holds a colour `free_group` can then never hand back.
    /// Both routes that can shrink a group are covered.
    #[test]
    fn a_group_that_falls_to_one_member_dissolves() {
        let mut s = LayerStack::new();
        for _ in 0..3 {
            s.add();
        }
        // Unticking down to one and pressing the chain: the strip's own path.
        assert_eq!(s.link(&[0, 1]), Some(0));
        s.unlink(&[0]);
        assert_eq!(s.get(1).unwrap().link, None, "the survivor is not a group");
        assert_eq!(s.free_group(), Some(0), "and the colour came back");

        // Deleting one of a pair.
        assert_eq!(s.link(&[2, 3]), Some(0));
        s.remove(3);
        assert_eq!(s.get(2).unwrap().link, None);
        assert_eq!(s.free_group(), Some(0));

        // A group of three losing one is still a group.
        let mut s = LayerStack::new();
        for _ in 0..2 {
            s.add();
        }
        assert_eq!(s.link(&[0, 1, 2]), Some(0));
        s.unlink(&[0]);
        assert_eq!(s.group_indices(0), vec![1, 2]);
    }

    /// A caller that hands the same layer twice, or one off the end, must not
    /// get round the "two or more" rule by arithmetic.
    #[test]
    fn linking_counts_layers_rather_than_indices() {
        let mut s = LayerStack::new();
        s.add();
        assert_eq!(s.link(&[0, 0]), None, "one layer named twice is one layer");
        assert_eq!(
            s.link(&[0, 99]),
            None,
            "an index off the end is not a layer"
        );
        assert_eq!(s.link(&[0, 1, 1]), Some(0));
    }

    /// Numbers — and therefore colours — come back when a group empties, so a
    /// session of linking and unlinking does not walk off the end of the
    /// palette while most of it stands unused.
    #[test]
    fn an_emptied_group_gives_its_colour_back() {
        let mut s = LayerStack::new();
        for _ in 0..3 {
            s.add();
        }
        assert_eq!(s.link(&[0, 1]), Some(0));
        assert_eq!(s.link(&[2, 3]), Some(1));
        s.unlink(&[0, 1]);
        assert_eq!(s.free_group(), Some(0));
        assert_eq!(
            s.link(&[0, 3]),
            Some(0),
            "the lowest free number, not the next"
        );
    }

    /// Every group in use is a refusal rather than a repeated colour, because
    /// a chain mark that two independent sets share is a mark that lies. The
    /// one exception is re-linking a set that already holds a whole group,
    /// which frees that group in the same breath.
    #[test]
    fn a_seventh_group_is_refused_rather_than_given_a_used_colour() {
        let mut s = LayerStack::new();
        while s.len() < LayerStack::LINK_GROUPS * 2 + 2 {
            s.add();
        }
        for g in 0..LayerStack::LINK_GROUPS {
            assert_eq!(s.link(&[g * 2, g * 2 + 1]), Some(g as u8));
        }
        let spare = s.len() - 2;
        assert_eq!(
            s.link(&[spare, spare + 1]),
            None,
            "there is no colour left to tell a seventh group apart"
        );
        // Re-linking exactly one existing group keeps its own number.
        assert_eq!(s.link(&[0, 1]), Some(0));
        // And a set that swallows a whole group whole takes that group's
        // number, because it is emptied on the way.
        assert_eq!(s.link(&[0, 1, spare]), Some(0));
    }

    // --- ticking -----------------------------------------------------------

    /// The one rule for what a bulk operation reaches. The fallback is what
    /// keeps a bulk operation and a single one from being two pieces of code.
    #[test]
    fn a_bulk_operation_reaches_the_ticked_layers_or_the_selected_one() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.set_active(1);
        assert_eq!(s.targets(), vec![1], "nothing ticked is the selected layer");
        assert_eq!(s.picked_count(), 0);

        s.get_mut(0).unwrap().picked = true;
        s.get_mut(2).unwrap().picked = true;
        assert_eq!(s.targets(), vec![0, 2]);
        assert_eq!(s.picked_count(), 2);
        assert!(
            !s.targets().contains(&1),
            "the selected layer is not reached once something is ticked"
        );

        s.pick_all(false);
        assert_eq!(s.targets(), vec![1]);
        s.pick_all(true);
        assert_eq!(s.targets(), vec![0, 1, 2]);
    }

    /// A tick belongs to the layer, not to a position — which is the whole
    /// reason it is a field rather than a set of positions held beside the
    /// stack. Both of these would have had to be maintained by hand.
    #[test]
    fn a_tick_follows_its_layer_and_goes_when_the_layer_does() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.get_mut(0).unwrap().picked = true;
        let slot = slot_of(s.get(0).unwrap());

        s.reorder(0, 2);
        assert_eq!(s.picked_indices(), vec![2]);
        assert_eq!(slot_of(s.get(2).unwrap()), slot);

        s.remove(2);
        assert_eq!(s.picked_indices(), Vec::<usize>::new());
    }

    #[test]
    fn the_stack_is_capped() {
        let mut s = LayerStack::new();
        while s.len() < LayerStack::MAX {
            assert!(s.add().is_some());
        }
        assert!(s.add().is_none(), "must not exceed the shader's array size");
    }

    // --- folders -----------------------------------------------------------

    /// The stack as `(name, depth, is folder)`, bottom first — what nearly
    /// every test below asserts against, because a folder is a shape and a
    /// shape is easier to read than four separate assertions about it.
    fn shape_of(s: &LayerStack) -> Vec<(String, u8, bool)> {
        s.layers()
            .iter()
            .map(|l| (l.name.clone(), l.depth, l.is_folder()))
            .collect()
    }

    /// Three layers, the top two put in a group:
    ///
    /// ```text
    ///   3  Group 1    depth 0   folder
    ///   2    Layer 3  depth 1
    ///   1    Layer 2  depth 1
    ///   0  Layer 1    depth 0
    /// ```
    ///
    /// Note where the folder sits: **above** its contents, which is what a
    /// layers panel draws, what ORA's nested `<stack>` writes, and what makes
    /// the folder the end of its own group as the composite walks bottom to
    /// top.
    fn grouped() -> LayerStack {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        assert_eq!(s.group(&[1, 2]), Some(3));
        s
    }

    #[test]
    fn a_folder_owns_the_run_of_entries_beneath_it() {
        let s = grouped();
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 0, false),
                ("Layer 2".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
        assert_eq!(s.subtree(3), 1..4, "the folder and both its layers");
        assert_eq!(s.subtree(1), 1..2, "a layer owns only itself");
        assert_eq!(s.subtree(0), 0..1);
        assert_eq!(s.ancestors_of(1).collect::<Vec<_>>(), vec![3]);
        assert_eq!(s.ancestors_of(0).collect::<Vec<_>>(), Vec::<usize>::new());
        assert_eq!(s.pixel_count(), 3, "a folder holds no pixels");
        assert_eq!(s.len(), 4, "three layers and one folder");
    }

    /// A folder holds **no slot**, which is the whole reason folders cost the
    /// undo history nothing: a `PixelPatch` names a slice, and one that is
    /// never handed out can never be inherited by anything.
    #[test]
    fn a_folder_takes_no_slice_and_grouping_frees_none() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        let before: Vec<Option<u32>> = s.layers().iter().map(Layer::slot).collect();
        let capacity = s.slot_capacity_needed();

        assert_eq!(s.group(&[0, 1, 2]), Some(3));
        assert_eq!(s.get(3).unwrap().slot(), None, "a folder holds no slice");
        assert_eq!(
            s.slot_capacity_needed(),
            capacity,
            "grouping must not allocate a slice"
        );
        let after: Vec<Option<u32>> = s
            .layers()
            .iter()
            .filter(|l| !l.is_folder())
            .map(Layer::slot)
            .collect();
        assert_eq!(
            after, before,
            "every layer kept the slice it already had, so every patch still \
             names its own pixels"
        );
    }

    /// **Ticking a folder ticks what is in it**, which is the half of "mark a
    /// group visible to show every layer in it" that the model owes. Written
    /// into the ticks rather than derived when they are read: unticking one
    /// layer afterwards has to stay unticked.
    #[test]
    fn ticking_a_folder_ticks_everything_in_it() {
        let mut s = grouped();
        s.pick(3, true);
        assert_eq!(s.picked_indices(), vec![1, 2, 3]);
        assert_eq!(s.targets(), vec![1, 2, 3]);

        // And a layer unticked afterwards stays unticked — a rule that
        // re-derived the set from the folder would put it straight back.
        s.pick(1, false);
        assert_eq!(s.picked_indices(), vec![2, 3]);

        s.pick(3, false);
        assert_eq!(s.picked_indices(), Vec::<usize>::new());
    }

    /// A folder's eye reaches its contents, and its lock does too. Both are
    /// booleans, which is exactly why a *pass-through* folder can carry them
    /// and cannot carry an opacity: `hidden ∧ anything = hidden`, where a
    /// group at 50% over two overlapping children is not two children at 50%.
    #[test]
    fn a_folders_eye_and_lock_reach_what_is_inside_it() {
        let mut s = grouped();
        assert!(s.effective_visible(1));
        assert!(!s.effective_locked(1));

        s.get_mut(3).unwrap().visible = false;
        assert!(!s.effective_visible(1), "hidden by the folder");
        assert!(!s.effective_visible(2));
        assert!(s.effective_visible(0), "outside the folder, still showing");
        assert!(
            s.get(1).unwrap().visible,
            "the layer's own eye is untouched, so opening the folder reveals it"
        );

        s.get_mut(3).unwrap().visible = true;
        s.get_mut(3).unwrap().locked = true;
        assert!(s.effective_locked(1));
        assert!(!s.effective_locked(0));
        s.set_active(1);
        assert!(
            s.active_is_locked(),
            "the one gate every operation asks has to see a folder's lock"
        );
    }

    /// A stack whose every layer is inside a hidden folder shows nothing —
    /// and a document of nothing but folders shows nothing either, however
    /// many eyes are open, because a folder holds no pixels.
    #[test]
    fn a_hidden_folder_makes_the_document_show_nothing() {
        let mut s = grouped();
        s.get_mut(0).unwrap().visible = false;
        assert!(s.any_visible(), "the group's two layers still show");
        s.get_mut(3).unwrap().visible = false;
        assert!(!s.any_visible());
    }

    /// A folder moves as a unit, and its contents keep their nesting.
    #[test]
    fn a_folder_travels_with_its_contents() {
        let mut s = grouped();
        assert!(s.reorder(3, 0), "the group to the bottom");
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 2".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
                ("Layer 1".into(), 0, false),
            ]
        );
        assert_eq!(s.subtree(2), 0..3, "still one group");
    }

    /// Dragging into a folder and out again — the whole of what a drop's depth
    /// is for.
    #[test]
    fn a_layer_can_be_dragged_into_a_folder_and_out_again() {
        let mut s = grouped();
        // "Layer 1", at the bottom, dropped onto the group's lower row at the
        // group's own depth: it lands inside.
        assert!(s.reorder_to(0, 1, 1));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 2".into(), 1, false),
                ("Layer 1".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
        assert_eq!(s.subtree(3), 0..4, "all three are inside now");

        // And back out: the same position at depth 0.
        assert!(s.reorder_to(1, 0, 0));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 0, false),
                ("Layer 2".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
    }

    /// The refusals. Each of these is a stack that would not describe a tree,
    /// and each is judged *before* anything is written — a refused move leaves
    /// the stack byte for byte as it was.
    #[test]
    fn a_move_that_would_not_describe_a_tree_is_refused() {
        let mut s = grouped();
        let before = shape_of(&s);

        assert!(
            !s.reorder_to(3, 1, 0),
            "a folder cannot be dropped inside itself"
        );
        assert!(!s.reorder_to(3, 2, 0), "nor onto its other content");
        assert!(
            !s.reorder_to(0, 1, 2),
            "nor two levels inside a folder that is one level deep"
        );
        assert!(!s.reorder_to(9, 0, 0), "an index off the end");
        assert!(
            !s.reorder_to(0, 0, 0),
            "a drop where it already is, at the depth it already has"
        );
        assert_eq!(shape_of(&s), before, "a refusal changed something");

        // And `can_reorder` is the same decision, which is what lets the drag
        // light a row up only where the drop will really happen.
        assert!(!s.can_reorder(3, 1, 0));
        assert!(s.can_reorder(0, 1, 1));
        assert_eq!(shape_of(&s), before, "asking must not change anything");
    }

    /// Nesting is capped in the model, not hoped for in the interface: the
    /// eventual group stack in the fragment shader is a fixed-size array, and
    /// a document too deep for it has to be refused where somebody can be told.
    #[test]
    fn nesting_stops_at_the_depth_the_shader_could_hold() {
        let mut s = LayerStack::new();
        // One layer, wrapped in folder after folder.
        for _ in 0..=LayerStack::MAX_DEPTH {
            if s.group(&[s.active_index()]).is_none() {
                break;
            }
        }
        let deepest = s.layers().iter().map(|l| l.depth).max().unwrap();
        assert_eq!(deepest, LayerStack::MAX_DEPTH);
        assert!(well_formed(
            &s.layers()
                .iter()
                .map(|l| (l.depth, l.is_folder()))
                .collect::<Vec<_>>()
        ));
        // The layer at the bottom of it all is as deep as anything gets.
        assert_eq!(s.ancestors_of(0).count(), LayerStack::MAX_DEPTH as usize);
    }

    /// Deleting a folder takes its contents and hands back every entry inside
    /// it, so a caller that parks them holds every slice — and one that drops
    /// them frees every slice, which is what happens here.
    #[test]
    fn deleting_a_folder_takes_its_contents_and_frees_every_slice() {
        let mut s = grouped();
        let capacity = s.slot_capacity_needed();
        let inside: Vec<u32> = s.layers()[1..3].iter().filter_map(Layer::slot).collect();
        assert_eq!(inside.len(), 2);

        let gone = s.remove(3).expect("the group can go");
        let freed: Vec<u32> = gone.iter().filter_map(Layer::slot).collect();
        assert_eq!(freed, inside, "both slices came back");
        // Dropped rather than parked, which is what puts them on the free list.
        drop(gone);
        assert_eq!(shape_of(&s), vec![("Layer 1".into(), 0, false)]);

        // And they are reused before the texture array grows, exactly as a
        // deleted layer's is.
        s.add();
        s.add();
        assert_eq!(s.slot_capacity_needed(), capacity);
    }

    /// **Deleting a set takes exactly the set, and this was a bug.**
    ///
    /// A folder's contents sit *below* it, so removing one shifts every index
    /// beneath it. Deleting the ticked entries one at a time — even backwards,
    /// which looks like the safe direction and is the direction that fails —
    /// handed the next step an index naming a different entry, and took a layer
    /// nobody had ticked. `remove_many` resolves the whole set first.
    #[test]
    fn deleting_a_set_takes_the_set_and_nothing_else() {
        // [A, B(1), F(0) over B, C] — the shape that breaks a reverse walk,
        // because F's subtree reaches *down* past B.
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        s.set_active(1);
        assert_eq!(s.group(&[1]), Some(2), "Layer 2 alone in a group");
        let names: Vec<String> = s.layers().iter().map(|l| l.name.clone()).collect();
        assert_eq!(
            names,
            ["Layer 1", "Layer 2", "Group 1", "Layer 3", "Layer 4"]
        );

        // Tick the bottom layer and the group. `pick` cascades, so the tick set
        // is {0, 1, 2} — and 1 is *below* 2.
        s.pick(0, true);
        s.pick(2, true);
        assert_eq!(s.targets(), vec![0, 1, 2]);

        s.remove_many(&s.targets()).expect("two layers survive");
        assert_eq!(
            s.layers()
                .iter()
                .map(|l| l.name.clone())
                .collect::<Vec<_>>(),
            ["Layer 3", "Layer 4"],
            "an entry nobody ticked was deleted"
        );
    }

    /// A refusal changes nothing at all — it must not delete the entries it got
    /// through before discovering the last one would empty the document.
    #[test]
    fn a_refused_deletion_removes_nothing() {
        let mut s = LayerStack::new();
        s.add();
        let before: Vec<Option<u32>> = s.layers().iter().map(Layer::slot).collect();
        assert!(s.remove_many(&[0, 1]).is_none(), "that is every layer");
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            before
        );
    }

    /// A document needs somewhere to paint, and a folder is not somewhere. So
    /// the folder holding the last layer cannot be deleted either — the
    /// refusal counts *pixel layers*, not entries.
    #[test]
    fn the_folder_holding_the_last_layer_cannot_be_deleted() {
        let mut s = LayerStack::new();
        assert_eq!(s.group(&[0]), Some(1));
        assert_eq!(s.len(), 2, "one layer inside one folder");
        assert!(
            s.remove(1).is_none(),
            "deleting the group would leave nowhere to paint"
        );
        assert!(s.remove(0).is_none(), "and so would deleting the layer");
        assert_eq!(s.len(), 2);
    }

    /// The delete buttons draw themselves from this, so it has to agree with
    /// [`LayerStack::remove`] exactly — a control offering an operation the
    /// model then declines is the lying control the interface rules refuse.
    ///
    /// "More than one entry" is emphatically **not** the same question: two
    /// entries can be one layer inside one folder, which cannot give up either.
    #[test]
    fn the_delete_buttons_ask_the_same_question_the_removal_answers() {
        let mut s = LayerStack::new();
        assert!(!s.can_remove(&[0]), "the only layer");
        assert!(!s.can_remove(&[]), "nothing named is nothing to delete");

        s.group(&[0]);
        assert_eq!(s.len(), 2, "two entries, and still only one layer");
        assert!(!s.can_remove(&[1]), "the group holding the only layer");
        assert!(!s.can_remove(&[0]));

        // A second layer beside the group, and now either can go.
        s.set_active(1);
        s.add().expect("room for another layer");
        let added = s.active_index();
        s.reorder_to(added, 1, 0);
        assert_eq!(s.pixel_count(), 2);
        assert!(s.can_remove(&[s.len() - 1]) || s.can_remove(&[0]));

        // And a set that covers every layer is refused whole, however it is
        // spelled — which is what stops the ticked strip's bin emptying a
        // document.
        let everything: Vec<usize> = (0..s.len()).collect();
        assert!(!s.can_remove(&everything));

        // The refusal matches what `remove` actually does.
        for i in 0..s.len() {
            let allowed = s.can_remove(&[i]);
            let mut copy = LayerStack::empty();
            for l in s.layers() {
                copy.push_imported(l.is_folder(), l.depth, l.name.clone());
            }
            assert_eq!(
                copy.remove(i).is_some(),
                allowed,
                "entry {i}: the button and the model disagree"
            );
        }
    }

    /// A new layer made with a folder selected goes **inside** it, which is
    /// what every application does and the only way to fill a group without
    /// dragging.
    #[test]
    fn a_new_layer_goes_inside_the_selected_folder() {
        let mut s = grouped();
        s.set_active(3);
        assert!(s.add().is_some());
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 0, false),
                ("Layer 2".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Layer 4".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
        assert_eq!(s.active_index(), 3, "and it is selected");
        assert_eq!(s.subtree(4), 1..5);
    }

    /// Grouping entries that are already nested keeps them nested relative to
    /// each other. Flattening them to one level instead would silently
    /// rearrange somebody's stack.
    #[test]
    fn grouping_keeps_the_nesting_the_entries_already_had() {
        let mut s = grouped();
        // Group the folder together with the loose layer below it.
        assert_eq!(s.group(&[0, 3]), Some(4));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 1, false),
                ("Layer 2".into(), 2, false),
                ("Layer 3".into(), 2, false),
                ("Group 1".into(), 1, true),
                ("Group 2".into(), 0, true),
            ]
        );
        assert_eq!(s.subtree(4), 0..5, "the outer group holds everything");
        assert_eq!(s.subtree(3), 1..4, "the inner one still holds its two");
    }

    /// A folder has no slot to find the selection by, so it is followed by
    /// where the move put it — and a *layer* is still found by its slice,
    /// which is what it always was.
    #[test]
    fn the_selection_follows_a_folder_through_a_move() {
        let mut s = grouped();
        s.set_active(3);
        assert!(s.reorder(3, 0));
        assert_eq!(s.active_index(), 2, "the folder, now above its contents");
        assert!(s.active_is_folder());
        assert_eq!(s.active_slot(), None, "and there is nowhere to paint");

        s.set_active(3);
        let slot = s.active_slot();
        assert!(s.reorder(3, 0));
        assert_eq!(s.active_slot(), slot, "a layer is still found by its slice");
    }

    /// The one shape a `Vec` of depths can hold that is not a tree: something
    /// nested inside a thing that cannot hold it.
    #[test]
    fn only_a_folder_can_enclose_anything() {
        // Bottom first, so the folder comes last.
        assert!(well_formed(&[(1, false), (0, true)]), "a layer in a folder");
        assert!(well_formed(&[(0, false), (0, false)]), "two loose layers");
        assert!(well_formed(&[]), "nothing at all");
        assert!(
            well_formed(&[(2, false), (1, true), (0, true)]),
            "a folder in a folder"
        );
        assert!(
            !well_formed(&[(1, false), (0, false)]),
            "a layer cannot hold a layer"
        );
        assert!(
            !well_formed(&[(1, false)]),
            "and nothing at all cannot hold one"
        );
        assert!(
            !well_formed(&[(2, false), (0, true)]),
            "a folder holds one level, not two"
        );
        assert!(
            !well_formed(&[(LayerStack::MAX_DEPTH + 1, false)]),
            "deeper than the shader could ever hold"
        );
    }

    /// An import can name depths that do not nest — a file capped at
    /// `MAX_DEPTH` on the way in, or one another application wrote. The
    /// picture is refused over none of it; only the grouping changes.
    #[test]
    fn an_ill_formed_import_is_straightened_rather_than_refused() {
        let mut s = LayerStack::empty();
        s.push_imported(false, 3, "far too deep".into());
        s.push_imported(true, 0, "Group".into());
        s.push_imported(false, 7, "deeper than its folder".into());
        s.flatten_ill_formed();
        assert!(well_formed(
            &s.layers()
                .iter()
                .map(|l| (l.depth, l.is_folder()))
                .collect::<Vec<_>>()
        ));
        assert_eq!(
            s.layers().iter().map(|l| l.depth).collect::<Vec<_>>(),
            // Bottom first: the layer under the folder keeps one level, the
            // folder is at the top level, and the one above it — enclosed by
            // nothing at all — is pulled out to the top level too.
            vec![1, 0, 0],
            "each pulled out to the deepest level something could hold it at"
        );
    }

    /// A linked layer inside a folder brings its group with it, and the folder
    /// brings its contents — the two ways of saying "these travel together"
    /// compose rather than fighting.
    #[test]
    fn a_link_group_and_a_folder_travel_together() {
        let mut s = LayerStack::new();
        for _ in 0..3 {
            s.add();
        }
        // The bottom two in a group, leaving two loose layers above it.
        assert_eq!(s.group(&[0, 1]), Some(2));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 1, false),
                ("Layer 2".into(), 1, false),
                ("Group 1".into(), 0, true),
                ("Layer 3".into(), 0, false),
                ("Layer 4".into(), 0, false),
            ]
        );

        // Link the group's own row to the topmost loose layer, then drag that
        // layer to the bottom. It brings the folder, and the folder brings the
        // two layers inside it — the two ways of saying "these travel
        // together", composing rather than fighting.
        assert_eq!(s.link(&[2, 4]), Some(0));
        assert!(s.reorder(4, 0));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 1, false),
                ("Layer 2".into(), 1, false),
                ("Group 1".into(), 0, true),
                ("Layer 4".into(), 0, false),
                ("Layer 3".into(), 0, false),
            ]
        );
    }

    /// **Dropping onto a folder's own row puts the entry inside it**, from
    /// above and from below alike, and an *empty* folder can be filled that way
    /// — which is the only way it can be filled by dragging at all.
    ///
    /// This is the gesture `layerdrag`'s depth rule exists for, and it was
    /// broken in one direction: a folder sits above its contents, so "insert at
    /// the folder's position" placed the entry *above* the folder, at a depth
    /// that folder no longer enclosed, and `well_formed` rightly refused it.
    #[test]
    fn a_folders_own_row_takes_a_drop_inside_it_from_either_side() {
        // From below, into an empty folder at the top.
        let mut s = LayerStack::new();
        s.add();
        assert_eq!(s.group(&[1]), Some(2));
        // [Layer 1, Layer 2(1), Group 1] — now empty the group out again.
        assert!(s.reorder_to(1, 0, 0));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 2".into(), 0, false),
                ("Layer 1".into(), 0, false),
                ("Group 1".into(), 0, true),
            ],
            "an empty folder at the top"
        );
        assert_eq!(s.subtree(2), 2..3, "and it really is empty");

        assert!(
            s.reorder_to(0, 2, 1),
            "the bottom layer, dropped onto the empty folder's row"
        );
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 0, false),
                ("Layer 2".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
        assert_eq!(s.subtree(2), 1..3, "the folder now holds it");

        // And from above: the layer below the folder, dropped onto its row.
        assert!(s.reorder_to(0, 2, 1));
        assert_eq!(s.subtree(2), 0..3, "both are inside now");
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 2".into(), 1, false),
                ("Layer 1".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
    }

    /// A drop at the bottom of a folder's contents is a drop *into* it — the
    /// position is the one the bottom layer already has, and the depth is what
    /// says which side of the boundary it lands on. Without that, the bottom of
    /// a group would be the one place in the stack a drag could not reach.
    #[test]
    fn the_bottom_of_a_folder_is_reachable_by_depth_alone() {
        let mut s = grouped();
        assert!(s.reorder_to(0, 0, 1), "same place, one level in");
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 1, false),
                ("Layer 2".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
            ]
        );
        assert_eq!(s.subtree(3), 0..4, "all three are inside now");
    }

    /// A stack whose every entry is inside one folder can still be got back out
    /// at either end, and a second top-level folder made out of what came out.
    ///
    /// This is the model half of `layerdrag`'s "past either end of the list is
    /// the top level". The gesture was unreachable in practice — a step of
    /// nesting is twelve pixels — but the moves themselves have to be legal, or
    /// the drag would light nothing up and the way out would still not exist.
    #[test]
    fn a_stack_entirely_inside_one_folder_can_still_reach_the_top_level() {
        let mut s = grouped();
        assert!(s.reorder_to(0, 0, 1), "everything into the one group");
        assert_eq!(s.subtree(3), 0..4);

        // Out at the bottom: the position it already holds, at the top level.
        assert!(s.reorder_to(0, 0, 0));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 0, false),
                ("Layer 2".into(), 1, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
            ],
            "the bottom layer is beside the group, not in it"
        );

        // And out at the top: the folder's own row, at the top level, which
        // puts it above the folder rather than inside it.
        assert!(s.reorder_to(1, 3, 0));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 0, false),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
                ("Layer 2".into(), 0, false),
            ]
        );

        // Two top-level layers now, and grouping one of them is the second
        // top-level folder the whole gesture exists to make possible.
        assert_eq!(s.group(&[0]), Some(1));
        assert_eq!(
            shape_of(&s),
            vec![
                ("Layer 1".into(), 1, false),
                ("Group 2".into(), 0, true),
                ("Layer 3".into(), 1, false),
                ("Group 1".into(), 0, true),
                ("Layer 2".into(), 0, false),
            ],
            "two folders, neither inside the other"
        );
        assert_eq!(s.subtree(1), 0..2);
        assert_eq!(s.subtree(3), 2..4);
    }

    // --- text layers --------------------------------------------------------

    fn some_text() -> TextObject {
        use crate::text::{Align, TextBlock};
        use crate::textobj::{Placement, TextFace};
        TextObject::new(
            TextBlock {
                text: "Caption".into(),
                size: 24.0,
                line_spacing: 1.0,
                tracking: 0.0,
                align: Align::Left,
            },
            TextFace {
                family: "Archivo".into(),
                style: "Regular".into(),
                postscript: "Archivo-Regular".into(),
            },
            Color::BLACK,
            Placement::identity(crate::geom::PixelRect {
                x: 10,
                y: 10,
                width: 40,
                height: 20,
            }),
        )
    }

    /// **The paint gate, and it is one gate.** Painting on a text layer is
    /// refused with a reason, and painting on its **mask** is not — a mask bounds
    /// the alpha the composite reads and changes none of the layer's own pixels,
    /// so it cannot put the record out of step with them.
    #[test]
    fn painting_on_a_text_layer_is_refused_and_painting_on_its_mask_is_not() {
        let mut s = LayerStack::new();
        assert_eq!(
            s.refusal_at(0, EditTarget::Layer),
            None,
            "plain paint layer"
        );
        assert!(s.set_text(0, some_text()));

        assert_eq!(
            s.refusal_at(0, EditTarget::Layer),
            Some(EditRefusal::Text),
            "a brush must not reach a text layer's own pixels"
        );
        assert_eq!(
            s.refusal_at(0, EditTarget::Mask),
            None,
            "a stroke on the mask cannot make the record wrong"
        );
        assert_eq!(s.active_refusal(EditTarget::Layer), Some(EditRefusal::Text));

        // Taking the record off is what "convert to paint" is, and the pixels
        // are untouched by it.
        assert!(s.take_text(0).is_some());
        assert_eq!(s.refusal_at(0, EditTarget::Layer), None);
        assert!(s.take_text(0).is_none(), "and it is idempotent");
    }

    /// **The gate fails closed.** An index off the end is a caller bug and not a
    /// permission, and answering `None` there would make one `Option` mean both
    /// "go ahead" and "no such layer" on the one function whose purpose is to
    /// refuse.
    #[test]
    fn the_gate_refuses_a_layer_that_is_not_there() {
        let s = LayerStack::new();
        assert_eq!(
            s.refusal_at(7, EditTarget::Layer),
            Some(EditRefusal::Missing)
        );
        assert_eq!(
            s.refusal_at(7, EditTarget::Mask),
            Some(EditRefusal::Missing)
        );
    }

    /// [`EditRefusal::ALL`] is checked by an **exhaustive match whose arms index
    /// it**, never by walking it: a test that iterates the array can only ever
    /// check what somebody remembered to put in it, and a hand-written length is
    /// exactly what shipped an `[EditKind; 11]` that was short.
    ///
    /// **It is still not total, and the hole is worth naming rather than
    /// implying it is closed**: an arm that returns the wrong index compiles and
    /// passes. What this does catch is a variant added and not listed, which is
    /// the failure that actually happens.
    #[test]
    fn every_refusal_is_named_in_the_all_array() {
        for refusal in EditRefusal::ALL {
            let at = match refusal {
                EditRefusal::Folder => 0,
                EditRefusal::Locked => 1,
                EditRefusal::Text => 2,
                EditRefusal::Missing => 3,
            };
            assert_eq!(EditRefusal::ALL[at], refusal);
        }
    }

    /// The three refusals share the one gate, in the order the sentences read.
    ///
    /// A lock beats text because unlocking is what somebody would do next either
    /// way, and a folder beats both because it has no pixels for any of this to
    /// be about.
    #[test]
    fn a_lock_and_a_folder_are_refused_at_the_same_gate_text_is() {
        let mut s = grouped();
        // The folder at 3, a layer inside it at 1.
        assert_eq!(
            s.refusal_at(3, EditTarget::Layer),
            Some(EditRefusal::Folder)
        );
        assert_eq!(s.refusal_at(1, EditTarget::Layer), None);

        assert!(s.set_text(1, some_text()));
        assert_eq!(s.refusal_at(1, EditTarget::Layer), Some(EditRefusal::Text));

        // A lock on the *folder* reaches the layer inside it, and reads as a
        // lock rather than as text: it is the one of the two that can be undone
        // by clicking something.
        s.get_mut(3).unwrap().locked = true;
        assert_eq!(
            s.refusal_at(1, EditTarget::Layer),
            Some(EditRefusal::Locked)
        );
        assert_eq!(
            s.refusal_at(1, EditTarget::Mask),
            Some(EditRefusal::Locked),
            "a lock refuses the mask too, where text does not"
        );

        // Every refusal has something to say, and `reason` is exhaustive so a
        // fourth cannot arrive without a sentence.
        for refusal in EditRefusal::ALL {
            assert!(!refusal.reason().is_empty());
            assert!(
                !refusal.reason().contains('—'),
                "no em-dash in a notice: {}",
                refusal.reason()
            );
        }
    }

    /// A folder cannot carry a record: it holds no pixels for one to describe,
    /// and a folder claiming to be a text layer would be written into a file
    /// beside a layer image that does not exist.
    #[test]
    fn a_folder_cannot_be_a_text_layer() {
        let mut s = grouped();
        assert!(!s.set_text(3, some_text()), "the folder");
        assert!(s.text_at(3).is_none());
        assert!(
            !s.set_text(9, some_text()),
            "and nor can an index off the end"
        );
    }

    /// **Deleting a folder parks the text along with the slices, and an undo
    /// brings both back** — with nothing written to make that happen, because the
    /// record travels inside the `Layer`.
    ///
    /// That is the whole argument for text being a field on a layer rather than a
    /// third kind beside a folder: every path that already moves a `Layer` moves
    /// the record for free.
    #[test]
    fn a_text_layer_deleted_inside_a_folder_comes_back_as_text() {
        let mut s = grouped();
        assert!(s.set_text(1, some_text()));
        let before = s.shape(64 * 64 * 4);
        let removed = s.remove(3).expect("the folder and its contents");
        assert_eq!(removed.len(), 3);
        assert!(
            removed.iter().any(|l| l.is_text()),
            "the record left the stack with its layer"
        );

        let parked = before.with_removed(removed);
        // Charged to the undo budget beside the slices, for the reason a parked
        // slice is: a budget blind to part of what it holds is one that will be
        // wrong later.
        // Two slices, because the folder holds none, plus the record on top of
        // them.
        assert!(
            parked.byte_len() >= 2 * 64 * 64 * 4 + some_text().byte_len(),
            "the record is charged beside the slices, not instead of them"
        );

        s.restore_shape(parked);
        assert_eq!(s.len(), 4);
        assert_eq!(
            s.text_at(1).map(|t| t.block.text.clone()),
            Some("Caption".into()),
            "the record came back with the layer, and nothing wrote it down"
        );
    }

    /// **Both of `byte_len`'s new terms, and each on its own.**
    ///
    /// The text and the effects sides added a term to the same sum four days
    /// apart, and a fixture carrying only one of them leaves the other
    /// contributing zero — so deleting it from the expression fails nothing. That
    /// is exactly what the test above did until this was written: it parks text
    /// and no effect, with a `>=`, so the effects term was unguarded on **both**
    /// sides of the merge. `byte_len` is asserted in this file and nowhere else in
    /// the workspace, which is what makes that a real hole rather than a
    /// duplicate.
    ///
    /// The comparisons differ in **one** thing at a time, so neither term can be
    /// met by the other's bytes. Both were checked by mutation, separately.
    #[test]
    fn a_parked_layer_is_charged_for_its_text_and_for_its_effects() {
        let park = |text: bool, effect: bool| {
            let mut s = LayerStack::new();
            s.add();
            if text {
                assert!(s.set_text(1, some_text()));
            }
            if effect {
                assert!(s.set_effect(1, Effect::outline()));
            }
            let before = s.shape(64 * 64 * 4);
            let removed = s.remove(1).expect("not the last layer");
            before.with_removed(removed).byte_len()
        };

        let bare = park(false, false);
        assert!(
            park(true, false) >= bare + some_text().byte_len(),
            "the text record is charged"
        );
        assert!(
            park(false, true) >= bare + std::mem::size_of::<Effect>(),
            "the effect is charged"
        );
        // And together, so neither term can be standing in for the other.
        assert!(park(true, true) > park(true, false));
        assert!(park(true, true) > park(false, true));
    }

    /// **A canvas flip mirrors the record rather than dropping it.** Undoing a
    /// flip is another flip, so a record dropped here is one no undo could put
    /// back — the failure `Selection::flipped` exists to avoid, in the same place.
    #[test]
    fn a_canvas_flip_mirrors_a_text_layers_record() {
        let canvas = UVec2::new(100, 80);
        let mut s = LayerStack::new();
        assert!(s.set_text(0, some_text()));

        assert_eq!(s.flip_text(FlipAxis::Horizontal, canvas), 0, "none dropped");
        let flipped = s.text_at(0).expect("still text").placement;
        assert_eq!(flipped.source.x, 100 - 10 - 40);
        assert_eq!(flipped.scale, [-1.0, 1.0]);

        // And flipping again is exactly where it started, because that is what
        // undoing a flip is.
        assert_eq!(s.flip_text(FlipAxis::Horizontal, canvas), 0);
        assert_eq!(s.text_at(0).unwrap().placement, some_text().placement);
    }

    /// **A canvas flip mirrors every effect's lighting, on every layer.**
    ///
    /// The sibling of the record's mirror above and it was missing: a flip turned
    /// every pixel over and left every shadow cast the way it was, which is a
    /// whole document's lighting disagreeing with its forms. Found by the agent
    /// that had just solved the same problem for text, which is the argument for
    /// [`LayerStack::flip_effects`]'s note that anything carrying a *direction*
    /// says so at its own method.
    ///
    /// Over several layers, because the loop is what could visit one and stop.
    #[test]
    fn a_canvas_flip_mirrors_every_effects_lighting() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        let shadow = Effect {
            angle: 120.0,
            distance: 10.0,
            ..Effect::drop_shadow()
        };
        for i in 0..3 {
            assert!(s.set_effect(i, shadow));
        }

        s.flip_effects(FlipAxis::Horizontal);
        for i in 0..3 {
            let there = s.get(i).unwrap().effect(EffectKind::DropShadow).unwrap();
            let (dx, dy) = there.offset();
            let (was_x, was_y) = shadow.offset();
            assert!((dx + was_x).abs() < 1e-3, "layer {i}: {dx} against {was_x}");
            assert!((dy - was_y).abs() < 1e-3, "layer {i}: {dy} against {was_y}");
        }

        // And flipping again is where it started, because that is what undoing a
        // flip is. The bound is `Effect::flipped`'s, not this method's.
        s.flip_effects(FlipAxis::Horizontal);
        for i in 0..3 {
            let back = s.get(i).unwrap().effect(EffectKind::DropShadow).unwrap();
            assert!((back.angle - shadow.angle).abs() <= 180.0 * f32::EPSILON);
        }

        // A layer with no effects is not disturbed, and neither is a folder —
        // which cannot hold one at all.
        assert!(s.group(&[0]).is_some());
        s.flip_effects(FlipAxis::Vertical);
    }

    /// **A resize drops every record**, for the reason a resize clears the undo
    /// history: a placement is a rectangle of a canvas that no longer exists, and
    /// a canvas that shrank has cropped the pixels the record describes. The
    /// pixels are all kept; what goes is the claim that they can be set again.
    #[test]
    fn a_resize_drops_every_text_record_and_keeps_every_pixel() {
        let mut s = LayerStack::new();
        s.add();
        assert!(s.set_text(0, some_text()));
        assert!(s.set_text(1, some_text()));
        let slots: Vec<Option<u32>> = s.layers().iter().map(Layer::slot).collect();

        assert_eq!(s.drop_text_objects(), 2, "and it says how many");
        assert!(s.layers().iter().all(|l| !l.is_text()));
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            slots,
            "not one pixel moved"
        );
        assert_eq!(s.drop_text_objects(), 0);
    }

    /// A text layer is an ordinary entry: one stack position, one slice, and
    /// `MAX` means what it always meant.
    ///
    /// Said out loud because "a layer kind" sounds like something that would have
    /// to be counted differently, and the whole reason this shape was chosen is
    /// that it does not.
    #[test]
    fn a_text_layer_costs_the_stack_exactly_what_a_layer_costs() {
        let mut s = LayerStack::new();
        let before = (s.len(), s.slot_capacity_needed());
        assert!(s.set_text(0, some_text()));
        assert_eq!((s.len(), s.slot_capacity_needed()), before);

        // And it takes a mask, clips and links like any other layer.
        assert!(s.add_mask(0).is_some());
        s.get_mut(0).unwrap().clipped = true;
        assert!(s.get(0).unwrap().is_text() && s.get(0).unwrap().has_mask());
    }

    // --- effects -----------------------------------------------------------

    use crate::effect::OutlinePosition;

    /// Every entry's effects, for the tests that have to say a refusal changed
    /// **nothing at all** rather than nothing they happened to look at.
    fn effects_of(stack: &LayerStack) -> Vec<Vec<Effect>> {
        stack
            .layers()
            .iter()
            .map(|l| l.effects().to_vec())
            .collect()
    }

    fn inside_outline() -> Effect {
        Effect {
            position: OutlinePosition::Inside,
            ..Effect::outline()
        }
    }

    #[test]
    fn a_fresh_layer_carries_no_effects() {
        let s = LayerStack::new();
        assert!(s.get(0).unwrap().effects().is_empty());
        assert_eq!(s.enabled_effect_count(), 0);
    }

    /// The invariant is the stack's to keep, not the caller's: whichever order
    /// they are handed over in, the layer holds them in the order they
    /// composite.
    #[test]
    fn effects_land_in_composite_order_however_they_arrived() {
        let mut s = LayerStack::new();
        assert!(s.set_effect(0, inside_outline()));
        assert!(s.set_effect(0, Effect::drop_shadow()));

        let kinds: Vec<EffectKind> = s.get(0).unwrap().effects().iter().map(|e| e.kind).collect();
        assert_eq!(kinds, [EffectKind::DropShadow, EffectKind::Outline]);

        // And the split at the layer is what §4 says: the shadow under it, an
        // inside outline over it.
        let below: Vec<EffectKind> = s.get(0).unwrap().effects_below().map(|e| e.kind).collect();
        let above: Vec<EffectKind> = s.get(0).unwrap().effects_above().map(|e| e.kind).collect();
        assert_eq!(below, [EffectKind::DropShadow]);
        assert_eq!(above, [EffectKind::Outline]);
    }

    /// Moving an outline from outside to inside moves its draw across the
    /// layer, which is the one parameter that reorders the list — and the
    /// reason the ordering cannot be established once when an effect is added.
    #[test]
    fn moving_an_outline_inside_moves_its_draw_over_the_layer() {
        let mut s = LayerStack::new();
        assert!(s.set_effect(0, Effect::outline()));
        assert!(s.set_effect(0, Effect::drop_shadow()));
        assert_eq!(s.get(0).unwrap().effects_above().count(), 0);

        assert!(s.set_effect(0, inside_outline()));
        let above: Vec<EffectKind> = s.get(0).unwrap().effects_above().map(|e| e.kind).collect();
        assert_eq!(above, [EffectKind::Outline]);
        assert_eq!(s.get(0).unwrap().effects().len(), 2, "still one of each");
    }

    #[test]
    fn a_layer_holds_at_most_one_effect_of_each_kind() {
        let mut s = LayerStack::new();
        assert!(s.set_effect(0, Effect::drop_shadow()));
        let mut second = Effect::drop_shadow();
        second.distance = 42.0;
        assert!(s.set_effect(0, second));

        assert_eq!(s.get(0).unwrap().effects(), [second]);
        assert_eq!(s.enabled_effect_count(), 1);
    }

    /// **A set naming a kind twice is refused whole**, where setting one
    /// effect at a time replaces.
    ///
    /// The two are not inconsistent and the difference is the whole reason
    /// [`LayerStack::set_effects`] exists. A *control* setting one effect means
    /// "make the drop shadow this", so replacing is the only sensible answer. A
    /// caller handing over a whole set is describing what the layer holds, and
    /// there replacing is a silent loss: `set_effect` in a loop would install
    /// the second, answer `true` both times, and leave the layer holding one of
    /// the two the caller offered.
    ///
    /// The rule is [`LayerStack::duplicate_effect_kind`], asked here and by
    /// `docimport::openraster::load_effects` — which has to ask before there is
    /// a stack — so the model and the reader cannot come to different views of
    /// what a duplicate is.
    #[test]
    fn a_set_of_effects_naming_one_kind_twice_is_refused_whole() {
        let mut shadow = Effect::drop_shadow();
        shadow.distance = 3.0;
        let mut louder = Effect::drop_shadow();
        louder.distance = 42.0;

        assert_eq!(
            LayerStack::duplicate_effect_kind(&[shadow, louder]),
            Some(EffectKind::DropShadow)
        );
        assert_eq!(
            LayerStack::duplicate_effect_kind(&[shadow, Effect::outline()]),
            None
        );
        assert_eq!(LayerStack::duplicate_effect_kind(&[]), None);

        let mut s = LayerStack::new();
        assert!(s.set_effects(0, &[Effect::outline()]), "a legal set lands");
        let before = effects_of(&s);

        assert!(!s.can_set_effects(0, &[shadow, louder]));
        assert!(!s.set_effects(0, &[shadow, louder]));
        assert_eq!(
            effects_of(&s),
            before,
            "a refused set must leave the layer exactly as it was"
        );

        // And the single-effect path still replaces, which is right for it.
        assert!(s.set_effect(0, shadow));
        assert!(s.set_effect(0, louder));
        assert_eq!(s.get(0).unwrap().effects().len(), 2, "outline plus shadow");
    }

    /// **`set_effects` replaces the whole set and puts it in composite order.**
    ///
    /// The order matters because a file's sequence is not the writer's to
    /// promise — an inside outline ranks above the layer and a drop shadow
    /// below it — and `docimport` relies on this rather than sorting for
    /// itself.
    #[test]
    fn setting_a_whole_set_replaces_what_was_there_and_orders_it() {
        let mut s = LayerStack::new();
        assert!(s.set_effect(0, Effect::drop_shadow()));

        let inside = Effect {
            position: OutlinePosition::Inside,
            ..Effect::outline()
        };
        let shadow = Effect::drop_shadow();
        assert!(shadow.rank() < inside.rank(), "the fixture is backwards");

        // Handed over the wrong way round.
        assert!(s.set_effects(0, &[inside, shadow]));
        assert_eq!(s.get(0).unwrap().effects(), [shadow, inside]);

        // And it is a replacement, not a join: the empty set empties the layer.
        assert!(s.set_effects(0, &[]));
        assert!(s.get(0).unwrap().effects().is_empty());
        assert_eq!(s.enabled_effect_count(), 0);
    }

    /// **The budget is counted once over the whole set**, which a loop of
    /// `set_effect` cannot do.
    ///
    /// A layer already at the budget being handed a *different* set of the same
    /// size is free — nothing new is being asked for — where installing them
    /// one at a time would pass through a state holding both the old and the
    /// new and be refused by it. The refusal would land on whichever effect
    /// happened to be last and leave a prefix of the caller's set behind.
    #[test]
    fn a_whole_set_meets_the_budget_once_rather_than_once_per_effect() {
        let mut s = LayerStack::new();
        while s.enabled_effect_count() < effect::MAX_ENABLED {
            let at = s.len() - 1;
            if !s.set_effect(at, Effect::drop_shadow()) || !s.set_effect(at, Effect::outline()) {
                break;
            }
            if s.enabled_effect_count() < effect::MAX_ENABLED {
                s.add();
            }
        }
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);

        // Swapping one layer's two effects for two others of the same kinds
        // costs the budget nothing, so it must be allowed.
        let at = 0;
        assert_eq!(s.get(at).unwrap().effects().len(), 2);
        let swapped = [
            Effect {
                distance: 99.0,
                ..Effect::drop_shadow()
            },
            Effect {
                spread: 9.0,
                ..Effect::outline()
            },
        ];
        assert!(
            s.can_set_effects(at, &swapped),
            "an exchange at the budget asks for nothing new"
        );
        assert!(s.set_effects(at, &swapped));
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);

        // One more enabled effect anywhere is over, and changes nothing.
        let elsewhere = s.len() - 1;
        let before = effects_of(&s);
        let three = [
            Effect::drop_shadow(),
            Effect::outline(),
            Effect {
                enabled: true,
                ..Effect::drop_shadow()
            },
        ];
        assert!(
            !s.set_effects(elsewhere, &three),
            "and that set is a duplicate as well, which is also a refusal"
        );
        assert_eq!(effects_of(&s), before);
    }

    /// `docs/layer-effects.md` §9.5: a folder holds no slot and its contents
    /// composite in place, so there is no coverage to derive an effect from
    /// until group compositing exists.
    #[test]
    fn a_folder_is_refused_an_effect_and_the_refusal_changes_nothing() {
        let mut s = LayerStack::new();
        s.add();
        assert_eq!(s.group(&[0]), Some(1));
        assert!(s.get(1).unwrap().is_folder());
        assert!(
            s.set_effect(2, Effect::drop_shadow()),
            "the layer above it is fine"
        );

        let before = effects_of(&s);
        assert!(!s.can_set_effect(1, Effect::drop_shadow()));
        assert!(!s.set_effect(1, Effect::drop_shadow()));
        assert!(!s.can_set_effect_enabled(1, EffectKind::DropShadow, true));
        // The set-install answers to the same gate, or the reader would have a
        // way round it that the controls do not.
        assert!(!s.can_set_effects(1, &[Effect::drop_shadow()]));
        assert!(!s.set_effects(1, &[Effect::drop_shadow()]));
        assert_eq!(effects_of(&s), before);
        assert_eq!(s.enabled_effect_count(), 1);
    }

    #[test]
    fn an_index_off_the_end_is_refused_an_effect() {
        let mut s = LayerStack::new();
        let before = effects_of(&s);
        assert!(!s.can_set_effect(9, Effect::outline()));
        assert!(!s.set_effect(9, Effect::outline()));
        assert!(!s.can_set_effects(9, &[Effect::outline()]));
        assert!(!s.set_effects(9, &[Effect::outline()]));
        assert_eq!(s.remove_effect(9, EffectKind::Outline), None);
        assert_eq!(effects_of(&s), before);
    }

    /// A stack of [`LayerStack::MAX`] layers, every effect switched on: as far
    /// as the budget lets it go, and one short of what was asked for.
    ///
    /// Shared by the tests below, which are all about what happens *at* the
    /// budget and would otherwise each restate how to get there. Folder-free,
    /// which is what "an ordinary document" means for reaching the cap: a
    /// folder occupies one of [`LayerStack::MAX`]'s entries and is refused
    /// effects, so any stack holding one asks for fewer than 128 and cannot
    /// meet it.
    fn stack_at_the_budget() -> LayerStack {
        let mut s = LayerStack::new();
        while s.len() < LayerStack::MAX {
            assert!(s.add().is_some(), "a fresh document holds MAX layers");
        }
        for i in 0..s.len() {
            for effect in [Effect::drop_shadow(), Effect::outline()] {
                // The last one is refused; that is the point of the fixture and
                // each test below asserts the refusal in its own terms.
                let _ = s.set_effect(i, effect);
            }
        }
        assert_eq!(
            s.enabled_effect_count(),
            effect::MAX_ENABLED,
            "the fixture's precondition, not the property under test"
        );
        s
    }

    /// **The budget is met by an ordinary document, and the next effect is
    /// refused.**
    ///
    /// Two kinds, one of each per layer, 64 layers: 128 asked for against
    /// [`effect::MAX_ENABLED`]'s 127, so exactly one is declined and it is the
    /// last one attempted. No synthetic stack and nothing past
    /// [`LayerStack::MAX`] — this is a document somebody could build.
    ///
    /// **It could not always be written this way.** The budget was 128 in the
    /// first draft, from a `MAX_DRAWS` of 192 the device could not have
    /// supplied, and at that figure `64 × 2` *was* the cap: the refusal was
    /// unreachable and this test had to reach past `add`'s bound through
    /// `push_imported` to exercise it at all. §6.3's corrected derivation off
    /// `max_texture_array_layers` — 256 slices, less the 129 layers and masks
    /// and the float's spare — put it at 127, and one lower is the whole
    /// difference between a guard nothing meets and a refusal somebody will.
    #[test]
    fn the_effect_budget_is_met_by_an_ordinary_document() {
        assert!(
            LayerStack::MAX * EffectKind::ALL.len() > effect::MAX_ENABLED,
            "a full stack no longer asks for more than the budget, so every \
             test below is vacuous. Read effect::MAX_ENABLED and \
             docs/layer-effects.md §6.3."
        );

        let mut s = LayerStack::new();
        while s.len() < LayerStack::MAX {
            assert!(s.add().is_some());
        }
        let mut refused = Vec::new();
        for i in 0..s.len() {
            for effect in [Effect::drop_shadow(), Effect::outline()] {
                if !s.set_effect(i, effect) {
                    refused.push((i, effect.kind));
                }
            }
        }
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);
        assert_eq!(
            refused,
            [(LayerStack::MAX - 1, EffectKind::Outline)],
            "exactly the last effect asked for is the one declined"
        );
    }

    /// One past the budget is refused, and the refusal changes **nothing at
    /// all** — the property [`LayerStack::plan_set_effect`] returning the
    /// vector to install rather than a verdict is what makes structural.
    #[test]
    fn the_effect_budget_refuses_the_one_past_it_and_changes_nothing() {
        let mut s = stack_at_the_budget();
        // The one layer the budget left short of a full pair.
        let last = LayerStack::MAX - 1;
        assert_eq!(s.get(last).unwrap().effects().len(), 1);

        let before = effects_of(&s);
        assert!(!s.can_set_effect(last, Effect::outline()));
        assert!(!s.set_effect(last, Effect::outline()));
        assert_eq!(effects_of(&s), before, "a refusal changed the stack");
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);

        // A *disabled* effect produces no draw, so it is not charged and it is
        // not refused — and switching it on afterwards is.
        let mut off = Effect::outline();
        off.enabled = false;
        assert!(s.set_effect(last, off));
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);

        let before = effects_of(&s);
        assert!(!s.can_set_effect_enabled(last, EffectKind::Outline, true));
        assert!(!s.set_effect_enabled(last, EffectKind::Outline, true));
        assert_eq!(effects_of(&s), before);

        // Give one back anywhere in the document and it fits.
        assert_eq!(
            s.remove_effect(0, EffectKind::DropShadow).map(|e| e.kind),
            Some(EffectKind::DropShadow)
        );
        assert!(s.set_effect_enabled(last, EffectKind::Outline, true));
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);
    }

    /// Replacing an enabled effect with another enabled one is free, even at
    /// the budget: the count does not move, so it must not be refused.
    #[test]
    fn editing_an_effect_at_the_budget_is_not_refused() {
        let mut s = stack_at_the_budget();

        let mut wider = Effect::outline();
        wider.spread = 12.0;
        assert!(s.can_set_effect(0, wider));
        assert!(s.set_effect(0, wider));
        assert_eq!(s.get(0).unwrap().effect(EffectKind::Outline), Some(&wider));
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED);
    }

    /// An undo may take a document *over* the budget, and must.
    ///
    /// [`effect::MAX_ENABLED`] governs adding an effect; the overflow is the
    /// draw path's, on `docs/layer-effects.md` §6.1's rule — the effect stays
    /// enabled and the document is said to be over its budget. Refusing here
    /// would mean an undo that silently dropped somebody's effects, which is
    /// worse than either.
    ///
    /// **The overflow is exactly one, and that is a bound rather than an
    /// incident.** A stack holds at most [`LayerStack::MAX`] entries and a
    /// layer at most one effect per kind, so no sequence of undos can put more
    /// than `MAX × EffectKind::ALL.len()` enabled effects in a document —
    /// `MAX_ENABLED + 1` today. **Which means the draw list reaches 192 while
    /// `MAX_DRAWS` is 191**, since a layer draw and an effect draw are counted
    /// together. That is reachable *now*, with two kinds, rather than waiting
    /// on a third, and it is the number `canvas.rs` and `composite.wgsl` are
    /// about to be sized against. §6.1's degrade-visibly path is what covers
    /// it; nothing here can, and nothing here should pretend to.
    #[test]
    fn undoing_a_delete_may_take_a_document_over_the_effect_budget() {
        let mut s = stack_at_the_budget();
        // Delete a layer carrying two effects, exactly as `app.rs` records it:
        // the shape before, with what left folded in afterwards.
        let before = s.shape(0);
        let gone = s.remove(0).unwrap();
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED - 2);

        // Two draws freed, so the layer the budget had left short can have its
        // pair completed and there is still one to spare.
        let last = s.len() - 1;
        assert!(s.set_effect(last, Effect::outline()));
        assert_eq!(s.enabled_effect_count(), effect::MAX_ENABLED - 1);

        s.restore_shape(before.with_removed(gone));
        assert_eq!(
            s.enabled_effect_count(),
            effect::MAX_ENABLED + 1,
            "an undo puts back what was deleted, budget or no budget"
        );
        assert!(!effect::within_budget(s.enabled_effect_count()));

        // And that is the ceiling, not one point on a scale: every layer now
        // holds one of each kind, which is all a stack of MAX entries can hold.
        assert_eq!(s.len(), LayerStack::MAX);
        assert_eq!(
            s.enabled_effect_count(),
            LayerStack::MAX * EffectKind::ALL.len(),
            "the most enabled effects any document can hold"
        );
    }

    #[test]
    fn switching_an_effect_off_gives_its_draw_back_and_keeps_its_settings() {
        let mut s = LayerStack::new();
        let mut shadow = Effect::drop_shadow();
        shadow.distance = 17.0;
        assert!(s.set_effect(0, shadow));
        assert_eq!(s.enabled_effect_count(), 1);

        assert!(s.set_effect_enabled(0, EffectKind::DropShadow, false));
        assert_eq!(s.enabled_effect_count(), 0);
        assert_eq!(
            s.get(0).unwrap().effects().len(),
            1,
            "the row is still there"
        );
        assert_eq!(
            s.get(0)
                .unwrap()
                .effect(EffectKind::DropShadow)
                .unwrap()
                .distance,
            17.0
        );
        assert_eq!(
            s.get(0).unwrap().effects_below().count(),
            0,
            "and draws nothing"
        );

        assert!(s.set_effect_enabled(0, EffectKind::DropShadow, true));
        assert_eq!(
            s.get(0).unwrap().effect(EffectKind::DropShadow),
            Some(&shadow)
        );
    }

    #[test]
    fn toggling_an_effect_a_layer_does_not_have_is_refused() {
        let mut s = LayerStack::new();
        assert!(!s.can_set_effect_enabled(0, EffectKind::Outline, false));
        assert!(!s.set_effect_enabled(0, EffectKind::Outline, false));
        assert!(s.get(0).unwrap().effects().is_empty());
    }

    #[test]
    fn removing_an_effect_leaves_the_rest_in_order() {
        let mut s = LayerStack::new();
        s.set_effect(0, inside_outline());
        s.set_effect(0, Effect::drop_shadow());

        assert_eq!(
            s.remove_effect(0, EffectKind::DropShadow).map(|e| e.kind),
            Some(EffectKind::DropShadow)
        );
        assert_eq!(s.get(0).unwrap().effects(), [inside_outline()]);
        assert_eq!(s.remove_effect(0, EffectKind::DropShadow), None);
        assert_eq!(s.enabled_effect_count(), 1);
    }

    /// Effects belong to the layer, exactly as its slice and its mask do, so
    /// reordering carries them — the property that makes them a field rather
    /// than a table beside the stack keyed by something that moves.
    #[test]
    fn effects_follow_their_layer_through_a_reorder() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.set_effect(2, Effect::outline());
        s.reorder(2, 0);
        assert_eq!(s.get(0).unwrap().effects(), [Effect::outline()]);
        assert!(s.get(1).unwrap().effects().is_empty());
        assert!(s.get(2).unwrap().effects().is_empty());
    }
}
