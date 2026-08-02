//! The layer stack.
//!
//! # Slots
//!
//! A layer's pixels live in a fixed slice ("slot") of a GPU texture array,
//! assigned when the layer is created and never changed. The stack order is
//! just the order of this `Vec`, so reordering layers is a pointer shuffle
//! rather than 16 MB of texture copies per move.
//!
//! Slots are recycled when a layer is deleted, which is why deleting a layer
//! invalidates undo history — an entry recorded against slot 3 would otherwise
//! be replayed into whatever new layer inherited that slot.
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
//! The consequences are the same ones slots always had, and both are load
//! bearing: removing a mask frees its slot for the next layer or mask to
//! inherit, so it **clears the undo history** for precisely the reason deleting
//! a layer does; and a mask slice is ordinary RGBA, of which the composite
//! reads one channel.

use crate::color::Color;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
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
    /// Travels with the other linked layers when any of them is moved.
    pub linked: bool,
    /// Texture-array slice holding this layer's pixels. Stable for the layer's
    /// lifetime.
    slot: u32,
    /// Slice holding this layer's mask, when it has one. Another slot of the
    /// same array — see the module docs.
    mask: Option<u32>,
}

impl Layer {
    pub fn slot(&self) -> u32 {
        self.slot
    }

    /// The slice holding this layer's mask, if it has one.
    pub fn mask(&self) -> Option<u32> {
        self.mask
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
    fn named(name: &str, slot: u32) -> Self {
        Self {
            name: name.to_string(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            clipped: false,
            locked: false,
            linked: false,
            slot,
            mask: None,
        }
    }
}

/// Bottom-to-top stack of layers. Index 0 is the bottom.
#[derive(Debug)]
pub struct LayerStack {
    layers: Vec<Layer>,
    active: usize,
    /// Slots freed by deletion, reused before allocating new ones.
    free_slots: Vec<u32>,
    next_slot: u32,
}

impl Default for LayerStack {
    fn default() -> Self {
        Self::new()
    }
}

impl LayerStack {
    /// Mirrored by `MAX_LAYERS` in `composite.wgsl`, which sizes a uniform
    /// array. Raising it means raising both.
    pub const MAX: usize = 64;

    /// Slices the renderer may have to allocate: one per layer, one per mask,
    /// and one spare for a floating transform's preview.
    ///
    /// Distinct from [`LayerStack::MAX`], which bounds *stack positions* and is
    /// what sizes the uniform array in `composite.wgsl`. A mask occupies no
    /// stack position, so the two numbers genuinely differ; conflating them
    /// would have capped a document at 32 masked layers.
    pub const MAX_SLOTS: u32 = Self::MAX as u32 * 2 + 1;

    pub fn new() -> Self {
        Self {
            layers: vec![Layer::named("Layer 1", 0)],
            active: 0,
            free_slots: Vec::new(),
            next_slot: 1,
        }
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

    /// Slot that strokes should be committed into.
    pub fn active_slot(&self) -> u32 {
        self.layers[self.active].slot
    }

    pub fn set_active(&mut self, index: usize) {
        if index < self.layers.len() {
            self.active = index;
        }
    }

    /// How many texture-array slices the renderer must have allocated.
    ///
    /// This is one past the highest slot ever handed out, not the layer count:
    /// slots are stable, so a stack of two layers can still be using slot 5.
    pub fn slot_capacity_needed(&self) -> u32 {
        self.next_slot
    }

    /// Insert a new empty layer directly above the active one and select it.
    ///
    /// Returns the new layer's slot, which the caller must clear on the GPU —
    /// a recycled slot still holds the deleted layer's pixels.
    pub fn add(&mut self) -> Option<u32> {
        if self.layers.len() >= Self::MAX {
            return None;
        }
        let slot = self.take_slot();
        let name = format!("Layer {}", self.next_name_number());
        let at = self.active + 1;
        self.layers.insert(at, Layer::named(&name, slot));
        self.active = at;
        Some(slot)
    }

    /// Hand out the next free slice, recycling before growing.
    fn take_slot(&mut self) -> u32 {
        match self.free_slots.pop() {
            Some(s) => s,
            None => {
                let s = self.next_slot;
                self.next_slot += 1;
                s
            }
        }
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
        let slot = self.take_slot();
        self.layers[index].mask = Some(slot);
        Some(slot)
    }

    /// Take the mask off the layer at `index`, returning the slice it gave
    /// back.
    ///
    /// **The caller must clear the undo history**, for the reason deleting a
    /// layer does: the slice goes on the free list, and a patch recorded
    /// against it would be replayed into whichever layer or mask inherits it.
    pub fn remove_mask(&mut self, index: usize) -> Option<u32> {
        let slot = self.layers.get_mut(index)?.mask.take()?;
        self.free_slots.push(slot);
        Some(slot)
    }

    /// The mask slice of the layer at `index`, if it has one.
    pub fn mask_at(&self, index: usize) -> Option<u32> {
        self.layers.get(index)?.mask
    }

    /// The mask slice of the selected layer, if it has one.
    pub fn active_mask(&self) -> Option<u32> {
        self.layers[self.active].mask
    }

    /// Remove a layer, returning its freed slot.
    ///
    /// Its mask's slice is freed too, and is deliberately *not* returned: the
    /// caller has nothing to do with it, because a freed slice is cleared when
    /// it is next handed out rather than when it is given back. What the caller
    /// does owe is the same thing it always owed — clearing the undo history,
    /// since both slices are now on the free list.
    ///
    /// Refuses to remove the last layer — a document with no layers has nowhere
    /// to paint.
    pub fn remove(&mut self, index: usize) -> Option<u32> {
        if self.layers.len() <= 1 || index >= self.layers.len() {
            return None;
        }
        let layer = self.layers.remove(index);
        self.free_slots.push(layer.slot);
        if let Some(mask) = layer.mask {
            self.free_slots.push(mask);
        }
        if self.active >= self.layers.len() {
            self.active = self.layers.len() - 1;
        }
        Some(layer.slot)
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
    pub fn active_is_locked(&self) -> bool {
        self.layers[self.active].locked
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

    /// Stack positions of every linked layer, ascending.
    pub fn linked_indices(&self) -> Vec<usize> {
        self.layers
            .iter()
            .enumerate()
            .filter(|(_, l)| l.linked)
            .map(|(i, _)| i)
            .collect()
    }

    /// Move the layer at `from` so that it sits at position `to`, shifting
    /// everything between them along by one.
    ///
    /// **This is a `Vec` shuffle and nothing else.** A layer's slot is fixed
    /// for its lifetime, so no pixels move and no slot changes hands — which is
    /// exactly why reordering, unlike *deleting*, does not have to clear the
    /// undo history. A `PixelPatch` names a slot; deleting frees one for the
    /// next layer to inherit, and an entry replayed after that would land in
    /// the wrong layer. Nothing here frees or reassigns one, so every patch
    /// still names the pixels it was captured from.
    ///
    /// Returns `false` where nothing moved: an index off the end, or a layer
    /// asked to move to where it already is. The caller wants to know, because
    /// a move that did nothing is not a document modification.
    /// **Linked layers travel together.** Moving one that is linked carries
    /// every other linked layer with it, and lands them contiguously at the
    /// destination in the order they were already in. That is the one sense of
    /// "move as a unit" this architecture can carry today — see the note on
    /// [`Layer::linked`] and `Floating` in the app, which cannot.
    pub fn reorder(&mut self, from: usize, to: usize) -> bool {
        if from >= self.layers.len() || to >= self.layers.len() || from == to {
            return false;
        }
        // The selection follows the *layer*, not the position it was at, so it
        // is remembered by slot and found again afterwards. Written this way
        // rather than as index arithmetic because the group case has no
        // arithmetic that is obviously right, and two rules for one question is
        // how they come to disagree.
        let selected = self.layers[self.active].slot;

        let group: Vec<usize> = if self.layers[from].linked {
            self.linked_indices()
        } else {
            vec![from]
        };
        if group.len() < 2 {
            let layer = self.layers.remove(from);
            self.layers.insert(to, layer);
        } else {
            // Where the group lands, counted among the layers that are *not*
            // moving: after everything at or before `to` when it is travelling
            // up, before whatever is at `to` when it is travelling down.
            let moving_up = to > from;
            let insert_at = (0..self.layers.len())
                .filter(|i| !group.contains(i))
                .filter(|i| if moving_up { *i <= to } else { *i < to })
                .count();
            let mut taken = Vec::with_capacity(group.len());
            for i in group.iter().rev() {
                taken.push(self.layers.remove(*i));
            }
            taken.reverse();
            for (n, layer) in taken.into_iter().enumerate() {
                self.layers.insert(insert_at + n, layer);
            }
        }

        self.active = self
            .layers
            .iter()
            .position(|l| l.slot == selected)
            .unwrap_or(self.active.min(self.layers.len() - 1));
        true
    }

    /// Move a layer one step towards the top. Returns its new index.
    pub fn move_up(&mut self, index: usize) -> Option<usize> {
        if index + 1 >= self.layers.len() {
            return None;
        }
        // A step is a reorder over one place. Written in terms of it rather
        // than as its own swap so there is one piece of code keeping the
        // selection with its layer, instead of three that have to agree.
        self.reorder(index, index + 1).then_some(index + 1)
    }

    /// Move a layer one step towards the bottom. Returns its new index.
    pub fn move_down(&mut self, index: usize) -> Option<usize> {
        if index == 0 || index >= self.layers.len() {
            return None;
        }
        self.reorder(index, index - 1).then_some(index - 1)
    }

    /// True when at least one layer would contribute to the composite.
    pub fn any_visible(&self) -> bool {
        self.layers.iter().any(|l| l.visible && l.opacity > 0.0)
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

/// Background shown beneath the bottom layer. Currently always transparent;
/// exists so a white-paper document mode is an additive change.
pub const DEFAULT_BACKGROUND: Color = Color::TRANSPARENT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_stack_has_one_active_layer() {
        let s = LayerStack::new();
        assert_eq!(s.len(), 1);
        assert_eq!(s.active_index(), 0);
        assert_eq!(s.active_slot(), 0);
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
        let bottom_slot = s.get(0).unwrap().slot();
        let top_slot = s.get(1).unwrap().slot();

        s.move_down(1);

        assert_eq!(s.get(0).unwrap().slot(), top_slot);
        assert_eq!(s.get(1).unwrap().slot(), bottom_slot);
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

    /// Reordering must be a shuffle of the order and nothing else. If a slot
    /// ever followed a position, deleting a layer would not be the only thing
    /// that had to clear the undo history — a patch names a slot.
    #[test]
    fn reordering_preserves_every_layers_slot() {
        let mut s = LayerStack::new();
        s.add();
        s.add();
        s.add();
        let mut slots: Vec<u32> = s.layers().iter().map(Layer::slot).collect();
        assert_eq!(slots.len(), 4);

        // Bottom to top, which is the longest move there is.
        s.reorder(0, 3);
        let moved = slots.remove(0);
        slots.push(moved);
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            slots
        );

        // And back down again, past two layers rather than to an end.
        s.reorder(3, 1);
        let moved = slots.remove(3);
        slots.insert(1, moved);
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            slots
        );
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
        let before: Vec<u32> = s.layers().iter().map(Layer::slot).collect();

        assert!(
            !s.reorder(1, 1),
            "a move to where it already is moved nothing"
        );
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            before
        );
        assert_eq!(s.active_index(), 1);
    }

    #[test]
    fn reordering_off_the_end_moves_nothing() {
        let mut s = LayerStack::new();
        s.add();
        let before: Vec<u32> = s.layers().iter().map(Layer::slot).collect();
        assert!(!s.reorder(0, 2));
        assert!(!s.reorder(7, 0));
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            before
        );
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
        let slots: Vec<u32> = s.layers().iter().map(Layer::slot).collect();

        assert_eq!(s.move_up(0), Some(1));
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            vec![slots[1], slots[0], slots[2]]
        );
        assert_eq!(s.move_down(1), Some(0));
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            slots
        );
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
        let freed = s.remove(1).unwrap();
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

        assert_eq!(s.remove_mask(0), Some(1));
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
        let slot = s.get(1).unwrap().slot();
        assert_eq!(s.remove(1), Some(slot));

        // Both come back, in some order, before the array grows.
        let capacity = s.slot_capacity_needed();
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
        let slots: Vec<u32> = s.layers().iter().map(Layer::slot).collect();
        s.get_mut(0).unwrap().linked = true;
        s.get_mut(3).unwrap().linked = true;

        // Drag the bottom one to the top; its partner comes too, and the two
        // arrive side by side in the order they were already in.
        assert!(s.reorder(0, 3));
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
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
        let slots: Vec<u32> = s.layers().iter().map(Layer::slot).collect();
        s.get_mut(0).unwrap().linked = true;
        s.get_mut(1).unwrap().linked = true;

        assert!(s.reorder(2, 0));
        assert_eq!(
            s.layers().iter().map(Layer::slot).collect::<Vec<_>>(),
            vec![slots[2], slots[0], slots[1]]
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
        s.get_mut(0).unwrap().linked = true;
        s.get_mut(1).unwrap().linked = true;
        s.set_active(2);
        let slot = s.active_slot();

        s.reorder(0, 3);
        assert_eq!(s.active_slot(), slot);
        assert_eq!(s.active_index(), 0, "two layers moved up past it");
    }

    #[test]
    fn the_stack_is_capped() {
        let mut s = LayerStack::new();
        while s.len() < LayerStack::MAX {
            assert!(s.add().is_some());
        }
        assert!(s.add().is_none(), "must not exceed the shader's array size");
    }
}
