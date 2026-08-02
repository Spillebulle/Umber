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

use crate::color::Color;

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
    /// Texture-array slice holding this layer's pixels. Stable for the layer's
    /// lifetime.
    slot: u32,
}

impl Layer {
    pub fn slot(&self) -> u32 {
        self.slot
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

    pub fn new() -> Self {
        Self {
            layers: vec![Layer {
                name: "Layer 1".to_string(),
                visible: true,
                opacity: 1.0,
                blend: BlendMode::Normal,
                slot: 0,
            }],
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
        let slot = match self.free_slots.pop() {
            Some(s) => s,
            None => {
                let s = self.next_slot;
                self.next_slot += 1;
                s
            }
        };
        let name = format!("Layer {}", self.next_name_number());
        let at = self.active + 1;
        self.layers.insert(
            at,
            Layer {
                name,
                visible: true,
                opacity: 1.0,
                blend: BlendMode::Normal,
                slot,
            },
        );
        self.active = at;
        Some(slot)
    }

    /// Remove a layer, returning its freed slot.
    ///
    /// Refuses to remove the last layer — a document with no layers has nowhere
    /// to paint.
    pub fn remove(&mut self, index: usize) -> Option<u32> {
        if self.layers.len() <= 1 || index >= self.layers.len() {
            return None;
        }
        let layer = self.layers.remove(index);
        self.free_slots.push(layer.slot);
        if self.active >= self.layers.len() {
            self.active = self.layers.len() - 1;
        }
        Some(layer.slot)
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
    pub fn reorder(&mut self, from: usize, to: usize) -> bool {
        if from >= self.layers.len() || to >= self.layers.len() || from == to {
            return false;
        }
        let layer = self.layers.remove(from);
        self.layers.insert(to, layer);
        // The selection follows the *layer*, not the position it was at. Three
        // cases: the moved layer itself, a layer the move stepped over — every
        // one of those shifts by exactly one, in the opposite direction — and
        // everything outside the span, which does not move at all.
        if self.active == from {
            self.active = to;
        } else if from < to && self.active > from && self.active <= to {
            self.active -= 1;
        } else if to < from && self.active >= to && self.active < from {
            self.active += 1;
        }
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

    #[test]
    fn the_stack_is_capped() {
        let mut s = LayerStack::new();
        while s.len() < LayerStack::MAX {
            assert!(s.add().is_some());
        }
        assert!(s.add().is_none(), "must not exceed the shader's array size");
    }
}
