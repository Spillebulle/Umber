//! The layer list's thumbnails: what is cached, and when it is thrown away.
//!
//! A thumbnail is a cache with an invalidation rule, and the invalidation rule
//! is most of the work. The rule here is a single number:
//! `CanvasRenderer::slot_revision`, which the renderer bumps in **every** method
//! that writes a layer slice. So a cached picture is stale exactly when that
//! number has moved since it was taken, and there is no list of "things that
//! change a layer" to keep in step — a stroke committing, a transform being put
//! down, an undo, a clear, a new mask, a flip and a resize all pass through one
//! of those methods by construction.
//!
//! What this module decides, and what makes it worth being a module:
//!
//! * **which** slot is read next, when several are stale. One thumbnail is read
//!   at a time — see `CanvasRenderer::begin_thumb` — so the order matters, and
//!   the active layer goes first because that is the row the painter has just
//!   changed and is looking at.
//! * that "the layer is empty" is a cached *answer* and not a missing one.
//!   Without that, a document with a blank layer would re-read it every frame
//!   for as long as it was open.
//! * that a slot no longer in the stack loses its picture. Slots are recycled,
//!   so an entry left behind would be the deleted layer's picture drawn on
//!   whichever layer inherited its slice.
//!
//! # Why it is keyed by document
//!
//! A slot is a slice of one document's texture array, so slot 3 means a
//! different layer in every tab. The cache therefore names the document it
//! belongs to and empties itself when that changes, which is what lets it live
//! *above* the `--- documents ---` line in `editor.rs` — like the autosave's map
//! and the clipboard, and unlike anything that would have to be moved in and out
//! of [`crate::session::DocumentState`] on every tab switch.

use crate::editor::Editor;
use crate::session::DocId;
use egui::{Context, TextureHandle};
use std::collections::HashMap;
use umber_core::{LayerStack, thumbnail};
use umber_render::{CanvasRenderer, Thumbnail};

/// Ask the renderer for the next thumbnail the list is missing.
///
/// The one place the cache and the renderer meet, so the pair of rules that
/// have to agree — "a slot no longer in the stack loses its picture" and "the
/// picture is stale when the slice's revision has moved" — are stated once,
/// together, against the same list of slots.
///
/// Called every frame, and therefore **allocates nothing**: the two lists are
/// fixed arrays bounded by `LayerStack::MAX`, about a kilobyte of stack. A
/// `Vec` a frame here would be the one place on this path that took the heap,
/// and the rule that nothing on it allocates is only worth anything if it is
/// kept where it is inconvenient.
pub fn request(editor: &mut Editor, canvas: &mut CanvasRenderer) {
    // Also called from the top of `app.rs`'s `render`, which is the call that
    // matters — see `Thumbs::follow`. Repeated here so this function is correct
    // on its own; it is idempotent, and one rule stated twice is not two rules.
    editor.thumbs.follow(editor.session.active_id());

    let mut slots = [(0u32, 0u64); LayerStack::MAX];
    let mut live = [0u32; LayerStack::MAX];
    let mut count = 0;
    // A folder has no slice to draw a picture of. The list shows it a folder
    // mark instead — the honest answer, where a thumbnail of one arbitrary
    // child would be a picture that lies about what the group holds. Compositing
    // the contents is the right answer and is a third mode for `thumbnail.wgsl`;
    // see `docs/layer-folders.md`.
    for slot in editor
        .layers
        .layers()
        .iter()
        .filter_map(|l| l.slot())
        .take(LayerStack::MAX)
    {
        slots[count] = (slot, canvas.slot_revision(slot));
        live[count] = slot;
        count += 1;
    }
    let (slots, live) = (&slots[..count], &live[..count]);
    editor.thumbs.retain(live);

    if canvas.thumb_in_flight() {
        return;
    }
    // `u32::MAX` where a folder is selected: no slot is preferred, so the
    // queue simply takes them in order.
    let active = editor.layers.active_slot().unwrap_or(u32::MAX);
    if let Some(slot) = editor.thumbs.wanted(slots, active) {
        canvas.begin_thumb(slot);
    }
}

/// What is known about one slot.
struct Entry {
    /// The slot's revision when the picture was read.
    revision: u64,
    /// `None` where the layer holds no non-transparent pixel at all. A real
    /// answer, cached like any other.
    picture: Option<TextureHandle>,
}

#[derive(Default)]
pub struct Thumbs {
    /// Which document these slots belong to. See the module docs.
    doc: Option<DocId>,
    entries: HashMap<u32, Entry>,
}

impl Thumbs {
    /// Point the cache at a document, emptying it if that is a different one.
    ///
    /// Called every frame rather than from the tab switch: a switch is four
    /// moves in `Editor` and a fifth that had to be remembered here is the one
    /// that would be forgotten by the path that closes a tab.
    ///
    /// **From the top of `app.rs`'s `render`, before the interface is built.**
    /// `request` below runs after it — it needs the frame's encoder — and a
    /// cache still naming the previous document while the list is drawn hands
    /// the new document's rows the old one's pictures for the same slots. One
    /// frame of the wrong picture, and precisely the confusion this cache being
    /// keyed by document exists to prevent.
    pub fn follow(&mut self, doc: DocId) {
        if self.doc != Some(doc) {
            self.doc = Some(doc);
            self.entries.clear();
        }
    }

    /// The picture for `slot`, where one has arrived and the layer had
    /// something on it.
    pub fn picture(&self, slot: u32) -> Option<&TextureHandle> {
        self.entries.get(&slot)?.picture.as_ref()
    }

    /// Which slot to read next, given the stack's slots in the order the list
    /// draws them and what the renderer says each is up to.
    ///
    /// `slots` is `(slot, revision)`, bottom of the stack first, with `active`
    /// naming the selected layer's slot. The active layer is answered first
    /// because it is the one the painter has just changed; everything else
    /// follows in stack order, so a freshly opened document fills in from the
    /// bottom rather than in whatever order a hash map happened to iterate.
    ///
    /// `None` when every slot has an answer for the revision it is at, which is
    /// the common case and costs one pass over a list of at most 64.
    pub fn wanted(&self, slots: &[(u32, u64)], active: u32) -> Option<u32> {
        let stale = |slot: u32, revision: u64| match self.entries.get(&slot) {
            Some(entry) => entry.revision != revision,
            None => true,
        };
        if let Some((slot, revision)) = slots.iter().find(|(slot, _)| *slot == active)
            && stale(*slot, *revision)
        {
            return Some(*slot);
        }
        slots
            .iter()
            .find(|(slot, revision)| stale(*slot, *revision))
            .map(|(slot, _)| *slot)
    }

    /// Take a picture the renderer has read back.
    pub fn accept(&mut self, ctx: &Context, thumb: Thumbnail) {
        let picture = (!thumb.is_empty()).then(|| {
            let side = thumbnail::SIZE as usize;
            let image = egui::ColorImage::from_rgba_unmultiplied([side, side], &thumb.rgba);
            // `Linear` filtering both ways: the picture is read back at 64 and
            // drawn at 24 points, which is a reduction on an ordinary display
            // and a slight magnification on none of them.
            ctx.load_texture(
                format!("layer-thumb-{}", thumb.slot),
                image,
                egui::TextureOptions::LINEAR,
            )
        });
        self.entries.insert(
            thumb.slot,
            Entry {
                revision: thumb.revision,
                picture,
            },
        );
    }

    /// Forget every slot not in `live`.
    ///
    /// Slots are recycled, so an entry for a deleted layer is not merely waste:
    /// it is that layer's picture, waiting to be drawn on whichever layer
    /// inherits the slice.
    pub fn retain(&mut self, live: &[u32]) {
        self.entries.retain(|slot, _| live.contains(slot));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(cache: &mut Thumbs, slot: u32, revision: u64) {
        // A `Context` with no window and no renderer is enough to hold a
        // texture: `load_texture` only records a delta.
        let ctx = Context::default();
        cache.accept(
            &ctx,
            Thumbnail {
                slot,
                revision,
                rgba: vec![0; (thumbnail::SIZE * thumbnail::SIZE * 4) as usize],
            },
        );
    }

    #[test]
    fn a_slot_with_no_answer_is_wanted() {
        let cache = Thumbs::default();
        assert_eq!(cache.wanted(&[(3, 0), (5, 0)], 5), Some(5));
        assert_eq!(cache.wanted(&[], 0), None);
    }

    /// The whole invalidation rule: a picture is stale exactly when the
    /// renderer's counter for its slice has moved.
    #[test]
    fn a_written_slot_is_wanted_again_and_an_untouched_one_is_not() {
        let mut cache = Thumbs::default();
        accepted(&mut cache, 3, 7);
        accepted(&mut cache, 5, 2);
        assert_eq!(cache.wanted(&[(3, 7), (5, 2)], 3), None);
        assert_eq!(cache.wanted(&[(3, 8), (5, 2)], 5), Some(3));
    }

    /// The row the painter has just changed is the one that redraws first.
    #[test]
    fn the_active_layer_is_answered_before_the_rest() {
        let cache = Thumbs::default();
        assert_eq!(cache.wanted(&[(1, 0), (2, 0), (3, 0)], 3), Some(3));
        // And when the active layer is up to date, the stack order decides.
        let mut cache = Thumbs::default();
        accepted(&mut cache, 3, 0);
        assert_eq!(cache.wanted(&[(1, 0), (2, 0), (3, 0)], 3), Some(1));
    }

    /// "There is nothing on this layer" is an answer, not a missing one. Left
    /// as missing, a blank layer would be re-read on every frame of the
    /// session.
    #[test]
    fn an_empty_layer_is_a_cached_answer() {
        let mut cache = Thumbs::default();
        let ctx = Context::default();
        cache.accept(
            &ctx,
            Thumbnail {
                slot: 1,
                revision: 4,
                rgba: Vec::new(),
            },
        );
        assert!(cache.picture(1).is_none(), "there is nothing to draw");
        assert_eq!(cache.wanted(&[(1, 4)], 1), None, "and nothing to ask again");
    }

    /// A slot that has left the stack loses its picture, because the next layer
    /// to take that slice would otherwise be drawn wearing it.
    #[test]
    fn a_recycled_slot_loses_its_picture() {
        let mut cache = Thumbs::default();
        accepted(&mut cache, 3, 1);
        accepted(&mut cache, 4, 1);
        cache.retain(&[4]);
        assert!(cache.picture(3).is_none());
        assert!(cache.picture(4).is_some());
    }

    /// The wiring, on a real device.
    ///
    /// Every part of this is tested on its own — the two passes and the
    /// revision counter in `umber-render`, the cache above — so what is left is
    /// [`request`] and the order the frame loop calls it in. That is exactly
    /// the seam where a mistake means "the list silently never fills in", which
    /// no unit test would notice. The same argument, and the same shape, as
    /// `autosave`'s `a_frame_loop_writes_the_document_out_by_itself`.
    ///
    /// Skips rather than fails with no adapter, like the GPU tests.
    #[test]
    fn a_frame_loop_fills_the_layer_list_in_and_keeps_it_true() {
        use umber_core::PixelRect;
        use umber_render::{CanvasRenderer, Gpu};

        let instance = Gpu::create_instance();
        let Ok(gpu) = pollster::block_on(Gpu::new(instance, None)) else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };

        let mut editor = Editor::default();
        editor.doc = umber_core::Document::new(64, 64);
        let mut canvas = CanvasRenderer::new(
            &gpu.device,
            editor.doc.size,
            wgpu::TextureFormat::Rgba8Unorm,
        );
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_all_layers(&mut enc);
        gpu.queue.submit(Some(enc.finish()));

        let ctx = Context::default();
        let slot = editor
            .layers
            .active_slot()
            .expect("the default stack is one layer");
        // Generous: a thumbnail is two passes and each takes a frame or two,
        // and the point of the bound is that a job which never answers fails
        // rather than hangs.
        let spin = |editor: &mut Editor, canvas: &mut CanvasRenderer| {
            for _ in 0..32 {
                let mut enc = gpu
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                request(editor, canvas);
                canvas.drive_thumb(&gpu.device, &mut enc);
                gpu.queue.submit(Some(enc.finish()));
                canvas.submit_thumb();
                let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
                if let Some(thumb) = canvas.take_thumb(&gpu.device) {
                    editor.thumbs.accept(&ctx, thumb);
                }
            }
        };

        spin(&mut editor, &mut canvas);
        assert!(
            editor.thumbs.picture(slot).is_none(),
            "a layer nobody has painted on draws the empty state"
        );

        // Something on the layer, by the route an undo writes a patch back.
        let rect = PixelRect {
            x: 8,
            y: 8,
            width: 16,
            height: 16,
        };
        let bytes: Vec<u8> = [255u8, 0, 0, 255]
            .iter()
            .copied()
            .cycle()
            .take((rect.area() * 4) as usize)
            .collect();
        canvas.write_layer_rect(&gpu.queue, slot, rect, &bytes);

        spin(&mut editor, &mut canvas);
        assert!(
            editor.thumbs.picture(slot).is_some(),
            "a layer with a mark on it should have a picture"
        );

        // And nothing asks again until the pixels move, which is the whole of
        // why the list can be redrawn every frame.
        let revision = canvas.slot_revision(slot);
        assert_eq!(editor.thumbs.wanted(&[(slot, revision)], slot), None);
    }

    /// Slot 3 is a different layer in every tab.
    #[test]
    fn switching_document_empties_the_cache() {
        let mut cache = Thumbs::default();
        cache.follow(DocId::for_test(1));
        accepted(&mut cache, 3, 1);
        cache.follow(DocId::for_test(1));
        assert!(cache.picture(3).is_some(), "the same document keeps it");
        cache.follow(DocId::for_test(2));
        assert!(cache.picture(3).is_none());
    }
}
