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

use serde::{Deserialize, Serialize};

use crate::color::Color;

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
        !self.free.is_empty() || self.next < LayerStack::MAX_SLOTS
    }

    fn take(&mut self) -> Option<u32> {
        if let Some(n) = self.free.pop() {
            return Some(n);
        }
        if self.next >= LayerStack::MAX_SLOTS {
            return None;
        }
        let n = self.next;
        self.next += 1;
        Some(n)
    }

    /// Take a number back, and **compact the tail** so `next` is one past the
    /// highest slice still claimed.
    ///
    /// The compaction is not tidiness. `next` is what
    /// [`LayerStack::slot_capacity_needed`] reports and therefore what
    /// `CanvasRenderer::begin_float` reserves its preview slice at, and that
    /// reservation is refused once it reaches [`LayerStack::MAX_SLOTS`].
    /// Without compacting, a session of deleting and adding layers walks `next`
    /// up to the ceiling one slice at a time — parking is what stops a delete
    /// returning the number immediately — and the transform tool then refuses
    /// for ever, on a document with a handful of layers and no way back.
    ///
    /// The array itself never shrinks (`ensure_slots` only grows), so a
    /// capacity that falls and rises again costs nothing.
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
        // A poisoned pool means another thread panicked holding it. Losing a
        // slice is a leak; panicking in a `Drop` while something else is
        // already unwinding is an abort.
        if let Ok(mut pool) = self.pool.lock() {
            pool.give_back(self.number);
        }
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
    pub fn has_room(&self) -> bool {
        self.0.lock().is_ok_and(|pool| pool.has_room())
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

/// How a layer combines with everything beneath it.
///
/// The numeric values are consumed directly by `composite.wgsl`; keep them in
/// step with the `switch` in `blend_rgb`.
///
/// Serialised because a *brush* carries one too — see [`crate::Brush::blend`] —
/// and a brush is what a preset file holds. Deliberately the same enum rather
/// than a second one beside it: the arithmetic is one shared WGSL function, so
/// a layer set to Multiply and a brush set to Multiply mean the same thing, and
/// two enums would eventually stop agreeing about which modes exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal = 0,
    Multiply = 1,
    Screen = 2,
    Overlay = 3,
    Add = 4,
}

impl BlendMode {
    pub const ALL: [BlendMode; 5] = [
        Self::Normal,
        Self::Multiply,
        Self::Screen,
        Self::Overlay,
        Self::Add,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Add => "Add",
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
    /// Mirrored by `MAX_LAYERS` in `composite.wgsl`, which sizes a uniform
    /// array. Raising it means raising both.
    ///
    /// It bounds **stack entries**, folders included, not the layers that hold
    /// pixels. A pass-through folder reaches the shader as nothing at all — it
    /// is flattened away in the app's `layer_draws` — so counting it here is
    /// stricter than the array needs today. It is counted anyway, because a
    /// folder that composites its contents as a group *will* occupy an entry in
    /// that array, and a cap that had to be tightened later would shut documents
    /// this build had already written.
    pub const MAX: usize = 64;

    /// The deepest a folder may be nested: eight levels, 0 through 7.
    ///
    /// Enforced here rather than left to the interface for the reason
    /// [`well_formed`] gives — the eventual group stack in the fragment shader
    /// is a fixed-size array, and a document too deep for it has to be refused
    /// where the refusal can be seen.
    pub const MAX_DEPTH: u8 = 7;

    /// Slices the renderer may have to allocate: one per layer, one per mask,
    /// and one spare for a floating transform's preview.
    ///
    /// Distinct from [`LayerStack::MAX`], which bounds *stack positions* and is
    /// what sizes the uniform array in `composite.wgsl`. A mask occupies no
    /// stack position, so the two numbers genuinely differ; conflating them
    /// would have capped a document at 32 masked layers.
    pub const MAX_SLOTS: u32 = Self::MAX as u32 * 2 + 1;

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
        self.pool.lock().map_or(Self::MAX_SLOTS, |pool| pool.next)
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
        let name = format!("Layer {}", self.next_name_number());
        let mut layer = Layer::named(&name, self.take_id(), Some(slot));
        layer.depth = depth;
        self.layers.insert(at, layer);
        debug_assert!(
            well_formed(&self.shape_pairs()),
            "add left a malformed stack"
        );
        self.active = at;
        Some(number)
    }

    /// Hand out the next free slice, recycling before growing.
    ///
    /// `None` when every slice is claimed — by the stack, or by a layer parked
    /// in an undo entry. See [`SlotPool::has_room`].
    fn take_slot(&mut self) -> Option<SlotClaim> {
        let number = self.pool.lock().ok()?.take()?;
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
    pub fn shape(&self) -> StackShape {
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
        }
    }

    /// The same, also recording the mask the entry at `index` has now.
    ///
    /// For the two edits that change one — adding a mask and taking one off.
    /// The claim is *cloned*, so the slice stays alive when the layer's own
    /// copy is dropped; that is the whole of how removing a mask stopped
    /// clearing the history.
    pub fn shape_with_mask(&self, index: usize) -> StackShape {
        let mut shape = self.shape();
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
                    // Missing means an entry this shape was recorded against is
                    // not in the stack, which the stepped-not-seeked guarantee
                    // says cannot happen: an older shape is only ever reached
                    // with everything above it already undone.
                    match find(id) {
                        Some(mut layer) => {
                            layer.depth = depth;
                            self.layers.push(layer);
                        }
                        None => {
                            debug_assert!(false, "a recorded shape named an entry that is gone")
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

    /// What this costs in memory, which is what the undo budget counts.
    ///
    /// Tens of bytes an entry against patches measured in megabytes, so it is
    /// never what evicts anything — the slice ceiling is what bounds these, not
    /// the budget. Counted anyway, because a session of thousands of structural
    /// edits is the case where a budget that could not see them would be
    /// counting the wrong thing.
    pub fn byte_len(&self) -> usize {
        self.entries.len() * std::mem::size_of::<ShapeEntry>()
            + self
                .entries
                .iter()
                .map(|e| match e {
                    ShapeEntry::Kept { .. } => 0,
                    ShapeEntry::Gone { layer } => std::mem::size_of::<Layer>() + layer.name.len(),
                })
                .sum::<usize>()
            + self.masks.len() * std::mem::size_of::<(u32, Option<SlotClaim>)>()
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
    /// occupies a slice and no stack position, so a full stack of masked layers
    /// needs twice `MAX` slices, plus the one a floating transform previews
    /// into. `MAX_SLOTS` in `umber-render` is the same arithmetic and caps the
    /// texture array; conflating either with `MAX` would have quietly halved
    /// how many layers could carry a mask.
    #[test]
    fn the_slot_ceiling_covers_a_fully_masked_stack_and_the_floats_spare() {
        let mut s = LayerStack::new();
        while s.len() < LayerStack::MAX {
            s.add().unwrap();
        }
        for i in 0..s.len() {
            s.add_mask(i).expect("every layer can take a mask");
        }
        assert_eq!(s.slot_capacity_needed(), LayerStack::MAX as u32 * 2);
        assert!(
            s.slot_capacity_needed() < LayerStack::MAX_SLOTS,
            "no slice left for a floating transform to preview into"
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
}
