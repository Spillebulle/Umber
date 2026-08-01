//! Writing the undo history into a saved document.
//!
//! # Why it is in the file at all
//!
//! A history that dies with the session means a document reopened tomorrow
//! cannot be stepped back through, however carefully it was built up. Nothing
//! about the format prevents carrying it: an ORA is a ZIP, and a ZIP may hold
//! entries nobody else reads. So the patches go in under `umber/`, named by a
//! manifest, and pointed at by one more `umber-` attribute on `<image>`.
//! Every other OpenRaster reader ignores all of it, which is what keeps the
//! file a plain `.ora`.
//!
//! # Slots are not layer identities
//!
//! [`crate::history::PixelPatch::slot`] is a texture-array slice, handed out by
//! [`LayerStack`] and **recycled** when a layer is deleted. That is why
//! deleting a layer clears the history: a patch replayed into a layer that
//! merely inherited a slot corrupts it. A slot written into a file and read
//! back into a different session's allocation is that same bug, made permanent.
//!
//! So a slot is never written. Every entry names a **stack position** instead —
//! an index into [`super::SaveDocument::layers`], bottom first, which is the
//! order the file itself is in and the order the reader rebuilds. The mapping
//! is made once, at save time, by [`SaveHistory::new`], and if any patch names
//! a slot no layer holds the whole history is refused rather than written half
//! safe.
//!
//! The manifest also carries the canvas size and the layer names in order, and
//! the reader requires both to match what it loaded. A history that replays
//! into the wrong layer is far worse than no saved history, so anything that
//! does not line up exactly is dropped.
//!
//! # Size, which is the whole problem
//!
//! The in-memory budget is 512 MB of raw RGBA
//! ([`History::default`](crate::history::History::default)). Writing that
//! verbatim would produce multi-gigabyte documents from an ordinary session, so
//! the file gets its own, much smaller budget — [`BUDGET_BYTES`] of *encoded*
//! patch data — and the oldest entries are dropped first, which is the
//! direction `evict_to_budget` already ages them out in.
//!
//! Patches are written as PNG at [`png::Compression::Fast`], the same encoder
//! and level the layer images use. `examples/measure-history.rs` is where these
//! numbers come from and is checked in so they can be re-measured; it paints
//! synthetic sessions on a 2048² canvas and captures exactly what `History`
//! would hold.
//!
//! | session | raw patches | Deflate | PNG (fast) |
//! |---|---|---|---|
//! | 120 full-canvas strokes | 221 MB | 68 MB (3.3×), 4.2 s | 42 MB (5.2×), 0.35 s |
//! | the same, heavily grained | 237 MB | 108 MB (2.2×), 5.6 s | 93 MB (2.6×), 0.41 s |
//! | 300 small sketching strokes | 7.5 MB | 0.33 MB (23×), 0.02 s | 0.38 MB (20×), 0.00 s |
//!
//! PNG wins on size everywhere but the sketch — where the difference is 50 kB —
//! and wins on *time* by an order of magnitude everywhere, because it filters
//! each row against the one above before it deflates and then has far less left
//! to compress. Deflate is what the ZIP would have done for free, so it is the
//! alternative worth measuring, and it loses.
//!
//! Encoding runs **newest first** and stops at the budget, so a session far
//! over it pays neither the time nor the space for the entries it will not
//! keep. What that costs end to end, on a one-layer 2048² document:
//!
//! | session | file | save | open |
//! |---|---|---|---|
//! | 300 small strokes | 2.67 → 3.09 MB | 0.09 → 0.08 s | 0.03 → 0.04 s |
//! | 40 full-canvas strokes | 5.28 → 10.68 MB | 0.10 → 0.15 s | 0.04 → 0.10 s |
//! | 120 full-canvas strokes | 9.68 → 41.53 MB | 0.12 → 0.39 s | 0.04 → 0.28 s |
//!
//! Opening is in there because the patches have to be decoded before the
//! document appears, and that is time the artist spends waiting.
//!
//! The last row is the budget saturating, and it is why saving the history is a
//! **preference** rather than a policy: an afternoon of full-canvas painting
//! makes the document four times the size, and somebody synchronising their
//! work to a network drive is entitled to say no. It is on by default because
//! the common case is the first row — sixteen per cent, and free — and because
//! a feature nobody knows to switch on is one nobody gets.
//!
//! # Pixels
//!
//! Patch PNGs hold layer-texture bytes **unconverted** — sRGB-encoded with
//! alpha premultiplied in linear space, exactly what `read_layer_rect` returned
//! and exactly what `write_layer_rect` wants back. The straight-alpha
//! conversion the layer images go through exists so other applications can read
//! them; nothing else reads these, so converting would be two passes over a
//! hundred megabytes to arrive back where it started, and it would put a
//! rounding step between undo and the pixels it restores.

use std::io::Write;

use glam::UVec2;
use serde::{Deserialize, Serialize};
use zip::ZipWriter;

use super::{SaveError, deflated, encode_png, stored};
use crate::geom::PixelRect;
use crate::history::{EditKind, History};
use crate::layer::LayerStack;

/// `<image>` attribute naming the history manifest inside the archive.
///
/// Without it the manifest is not looked for at all, even if the entry is
/// there. Every writer of an ORA rewrites `stack.xml`, so an application that
/// copied Umber's private entries across while rearranging the document cannot
/// leave a history pointing at layers that have moved.
pub const HISTORY_ATTR: &str = "umber-history";

/// Where the manifest goes. Under a directory of Umber's own, so nothing here
/// can collide with anything the specification may later name.
pub const MANIFEST: &str = "umber/history/index.json";

/// Revision of the history layout, independent of
/// [`super::VERSION`](super::VERSION).
///
/// Separate because it governs something an older build **discards** rather
/// than misreads: a history it cannot parse is simply not restored, and the
/// document opens exactly as it would have before histories were saved at all.
/// That is why saving a history does not bump the document version — see the
/// module docs of [`super`].
pub const VERSION: u32 = 1;

/// How much encoded patch data a document will carry.
///
/// 32 MB, against 512 MB in memory. At the 2.6–5× that painted patches compress
/// by, that is 80–170 MB of raw history — measured, the newest 62 of 120
/// full-canvas strokes, 26 of 120 heavily grained ones, and all 300 of a
/// sketching session with room to spare. It bounds the extra size of the file,
/// the extra time a save takes and the extra time an *open* takes; the module
/// docs have what those measure at.
///
/// One number rather than a fraction of the document, because a rule nobody can
/// predict is worse than a limit everybody can. A larger one would not help the
/// case it is for: a session deep enough to saturate this has already put more
/// in memory than any file should carry.
pub const BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// One recorded edit on its way to disk.
pub struct SaveEdit<'a> {
    /// Index into [`super::SaveDocument::layers`], bottom first. **Never a
    /// texture slot** — see the module docs.
    layer: usize,
    kind: EditKind,
    rect: PixelRect,
    /// Layer-texture bytes, `rect.area() * 4` of them.
    bytes: &'a [u8],
}

/// An undo history resolved against the stack it belongs to.
///
/// Built by [`SaveHistory::new`], which is the only place a texture slot is
/// turned into a stack position, and which refuses outright if any of them
/// cannot be.
pub struct SaveHistory<'a> {
    entries: Vec<SaveEdit<'a>>,
    /// How many of `entries` are applied — the cursor within the timeline.
    position: usize,
    /// How many older entries are already gone, so a reopened document's list
    /// can still say it does not reach the beginning.
    dropped: usize,
}

impl<'a> SaveHistory<'a> {
    /// Resolve every patch in `history` against `layers`.
    ///
    /// `None` when any patch names a slot no layer in the stack holds. That
    /// cannot arise from the editor — deleting a layer clears the history for
    /// exactly this reason — but it is the one check that stands between a
    /// saved history and a patch replayed into a layer that merely inherited a
    /// slot, so it is made here rather than assumed, and it refuses the *whole*
    /// history: the entries are a sequence, each restoring the pixels the next
    /// one expects, and one missing from the middle is not a shorter history
    /// but a wrong one.
    pub fn new(history: &'a History, layers: &LayerStack) -> Option<Self> {
        let mut entries = Vec::with_capacity(history.len());
        for i in 0..history.len() {
            let edit = history.entry_at(i)?;
            let layer = layers
                .layers()
                .iter()
                .position(|l| l.slot() == edit.patch.slot)?;
            entries.push(SaveEdit {
                layer,
                kind: edit.kind,
                rect: edit.patch.rect,
                bytes: &edit.patch.bytes,
            });
        }
        Some(Self {
            entries,
            position: history.position(),
            dropped: history.dropped(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The manifest, which is the whole of what the reader needs besides the PNGs.
///
/// Shared with the reader (`docimport::history`) rather than described twice —
/// serde derives both directions from this one definition, so the two cannot
/// drift the way a hand-written parser beside a hand-written writer would.
#[derive(Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub version: u32,
    /// The canvas the rectangles are in. A patch is a rectangle of a *particular*
    /// canvas, so a document that has been resized since cannot use them — the
    /// same reason resizing clears the history in the first place.
    pub canvas: [u32; 2],
    /// Layer names, bottom first, as a fingerprint of the stack the positions
    /// index into. If the document that comes back does not have this exact
    /// list, the positions mean something else and the history is dropped.
    pub layers: Vec<String>,
    pub position: usize,
    pub dropped: usize,
    /// Timeline order, oldest first.
    pub entries: Vec<ManifestEdit>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct ManifestEdit {
    pub layer: usize,
    /// [`EditKind`] by name — see [`kind_id`].
    pub kind: String,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Archive entry holding the patch.
    pub src: String,
}

/// Stable name for an [`EditKind`]. The debug spelling, like
/// [`super::blend_id`], so it cannot drift out of step with the enum the way a
/// second hand-written table would.
pub fn kind_id(kind: EditKind) -> String {
    format!("{kind:?}")
}

/// Inverse of [`kind_id`]. `None` for a name this build does not know, which
/// drops the history rather than guessing at what an entry was.
pub fn kind_from_id(id: &str) -> Option<EditKind> {
    EditKind::ALL.into_iter().find(|k| kind_id(*k) == id)
}

/// Write the history into an archive being built, returning whether anything
/// went in — which is what tells the caller to write [`HISTORY_ATTR`].
///
/// Nothing here reaches the GPU: the patches are already in memory, having been
/// captured at commit time. A save's blocking readbacks are unchanged.
pub(crate) fn write(
    zip: &mut ZipWriter<std::io::Cursor<Vec<u8>>>,
    canvas: UVec2,
    layers: &[String],
    history: &SaveHistory<'_>,
) -> Result<bool, SaveError> {
    // Newest first, stopping at the budget: the oldest entries are the ones to
    // lose, and encoding them only to throw them away would be the bulk of the
    // cost of a session that is far over.
    let mut used = 0usize;
    let mut kept: Vec<(usize, Vec<u8>)> = Vec::new();
    for (i, edit) in history.entries.iter().enumerate().rev() {
        let png = encode_png(UVec2::new(edit.rect.width, edit.rect.height), edit.bytes)?;
        if used + png.len() > BUDGET_BYTES {
            break;
        }
        used += png.len();
        kept.push((i, png));
    }
    if kept.is_empty() {
        return Ok(false);
    }
    kept.reverse();

    // Everything before the first survivor is gone, so the cursor moves back by
    // that much and the drop count forward by it. A cursor that ran off the
    // front means every applied entry was aged out and what is left is entries
    // the user had undone — still worth keeping, and still stepped back to from
    // position zero.
    let first = kept[0].0;
    let manifest = Manifest {
        version: VERSION,
        canvas: [canvas.x, canvas.y],
        layers: layers.to_vec(),
        position: history.position.saturating_sub(first),
        dropped: history.dropped + first,
        entries: kept
            .iter()
            .enumerate()
            .map(|(n, (i, _))| {
                let edit = &history.entries[*i];
                ManifestEdit {
                    layer: edit.layer,
                    kind: kind_id(edit.kind),
                    x: edit.rect.x,
                    y: edit.rect.y,
                    w: edit.rect.width,
                    h: edit.rect.height,
                    src: patch_src(n),
                }
            })
            .collect(),
    };

    for (n, (_, png)) in kept.iter().enumerate() {
        // Stored: a PNG is deflated already, so deflating it again in the ZIP
        // costs time and gains nothing.
        zip.start_file(patch_src(n), stored())?;
        zip.write_all(png)?;
    }

    let json =
        serde_json::to_vec(&manifest).map_err(|e| SaveError::Io(std::io::Error::other(e)))?;
    zip.start_file(MANIFEST, deflated())?;
    zip.write_all(&json)?;
    Ok(true)
}

pub(crate) fn patch_src(index: usize) -> String {
    format!("umber/history/{index:04}.png")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docformat::{SaveDocument, SaveLayer, encode};
    use crate::docimport::{self, ImportWarning};
    use crate::document::{Background, Document};
    use crate::geom::PixelRect;
    use crate::history::{Edit, PixelPatch};
    use crate::layer::BlendMode;

    const CANVAS: UVec2 = UVec2::new(64, 64);

    fn blank() -> Vec<u8> {
        vec![0; CANVAS.x as usize * CANVAS.y as usize * 4]
    }

    /// A patch of `w × h` filled with one byte, so a round trip can be checked
    /// by looking at one value — and so two patches are never accidentally
    /// equal.
    fn patch(slot: u32, w: u32, h: u32, fill: u8) -> PixelPatch {
        let rect = PixelRect {
            x: 1,
            y: 2,
            width: w,
            height: h,
        };
        PixelPatch::new(rect, slot, vec![fill; (w * h * 4) as usize])
    }

    /// A stack of named layers, in the order given.
    fn stack(names: &[&str]) -> LayerStack {
        let mut stack = LayerStack::new();
        for _ in 1..names.len() {
            stack.add();
        }
        for (i, name) in names.iter().enumerate() {
            stack.get_mut(i).unwrap().name = (*name).to_string();
        }
        stack
    }

    /// Write a document carrying `history`, and read it back the way the editor
    /// does — through the one ORA reader, and then through `open`, which is
    /// where stack positions become texture slots again.
    fn round_trip(stack: &LayerStack, history: &History) -> (Vec<u8>, docimport::ImportedDocument) {
        let pixels = blank();
        let layers: Vec<SaveLayer<'_>> = stack
            .layers()
            .iter()
            .map(|l| SaveLayer {
                name: &l.name,
                visible: true,
                opacity: 1.0,
                blend: BlendMode::Normal,
                pixels: &pixels,
            })
            .collect();
        let (bytes, _) = encode(&SaveDocument {
            size: CANVAS,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: &pixels,
            history: SaveHistory::new(history, stack),
        })
        .expect("encode");
        let doc = docimport::read_openraster(&bytes).expect("read back");
        (bytes, doc)
    }

    /// The one test that matters: a document saved mid-timeline comes back
    /// mid-timeline, with both stacks intact and every patch byte for byte.
    /// Restoring only the undo half would silently throw away work the artist
    /// had undone and meant to come back to; restoring it in the wrong order
    /// would replay the wrong pixels.
    #[test]
    fn a_history_survives_a_round_trip() {
        let stack = stack(&["Paper", "Ink"]);
        let (a, b) = (stack.get(0).unwrap().slot(), stack.get(1).unwrap().slot());

        let mut history = History::default();
        history.record(Edit::new(EditKind::Paint, patch(a, 5, 3, 11)));
        history.record(Edit::new(EditKind::Erase, patch(b, 4, 4, 22)));
        history.record(Edit::new(EditKind::Paint, patch(b, 6, 2, 33)));
        // Step back one, so the timeline straddles both stacks and the redo
        // side has something in it.
        let undone = history.take_undo().unwrap();
        history.push_redo(Edit::new(undone.kind, patch(b, 6, 2, 44)));
        assert_eq!((history.len(), history.position()), (3, 2));

        let (_, doc) = round_trip(&stack, &history);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let opened = doc.open();
        let back = &opened.history;

        assert_eq!(back.len(), history.len());
        assert_eq!(back.position(), history.position());
        assert_eq!(back.dropped(), history.dropped());
        for i in 0..history.len() {
            let (before, after) = (history.entry_at(i).unwrap(), back.entry_at(i).unwrap());
            assert_eq!(after.kind, before.kind, "entry {i}");
            assert_eq!(after.patch.rect, before.patch.rect, "entry {i}");
            assert_eq!(after.patch.bytes, before.patch.bytes, "entry {i}");
            // The same *layer*, which is what the slot has to mean again.
            let slot = opened
                .stack
                .layers()
                .iter()
                .position(|l| l.slot() == after.patch.slot);
            let was = stack
                .layers()
                .iter()
                .position(|l| l.slot() == before.patch.slot);
            assert_eq!(slot, was, "entry {i} came back on a different layer");
        }
        assert!(back.can_undo() && back.can_redo());
    }

    /// A patch belongs to a *layer*, and a texture slot is not one — slots are
    /// recycled, so a stack whose order no longer matches its slot numbers is
    /// exactly where writing a slot down would corrupt a document. Reordered
    /// here, which is the cheap way to produce that state; deleting a layer
    /// clears the history and so cannot.
    #[test]
    fn a_patch_finds_its_layer_again_however_the_slots_fell_out() {
        let mut stack = stack(&["Paper", "Ink", "Wash"]);
        // Positions 0,1,2 hold slots 0,1,2; after this they hold 0,2,1.
        stack.move_down(2).unwrap();
        let slots: Vec<u32> = stack.layers().iter().map(|l| l.slot()).collect();
        assert_eq!(slots, [0, 2, 1], "the fixture is not testing anything");

        // A patch on the middle layer — "Wash", holding slot 2.
        let mut history = History::default();
        history.record(Edit::new(EditKind::Paint, patch(2, 4, 4, 77)));

        let (_, doc) = round_trip(&stack, &history);
        let opened = doc.open();
        assert_eq!(opened.history.len(), 1);

        let slot = opened.history.entry_at(0).unwrap().patch.slot;
        let landed = opened
            .stack
            .layers()
            .iter()
            .find(|l| l.slot() == slot)
            .expect("the patch names a layer of the reopened stack");
        assert_eq!(
            landed.name, "Wash",
            "the patch was replayed into the wrong layer"
        );
        // And the reopened session's slots really are different numbers, so the
        // test would have caught a slot written straight through.
        assert_ne!(slot, 2, "the fixture is not testing anything");
    }

    /// Every document Umber wrote before this, and every ORA from every other
    /// application. It has to open, with an empty history and nothing said.
    #[test]
    fn a_document_with_no_history_opens_with_an_empty_one() {
        let pixels = blank();
        let layers = vec![SaveLayer {
            name: "Ink",
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            pixels: &pixels,
        }];
        let (bytes, _) = encode(&SaveDocument {
            size: CANVAS,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: &pixels,
            history: None,
        })
        .unwrap();

        let zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..])).unwrap();
        assert!(
            !zip.file_names().any(|n| n.starts_with("umber/")),
            "a document with no history must carry nothing under umber/"
        );
        let doc = docimport::read_openraster(&bytes).unwrap();
        assert!(doc.history.is_none());
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert!(doc.open().history.is_empty());
    }

    /// The rule this whole module exists for: anything that does not line up
    /// exactly drops the history and says so, rather than replaying patches
    /// into layers that are not the ones they were recorded against.
    #[test]
    fn a_history_that_cannot_be_placed_is_dropped_rather_than_replayed() {
        let stack = stack(&["Paper", "Ink"]);
        let mut history = History::default();
        history.record(Edit::new(EditKind::Paint, patch(0, 4, 4, 5)));
        let (bytes, _) = round_trip(&stack, &history);

        // Each of these is a document that no longer matches the history it
        // carries: renamed layers, a canvas that has been resized, a manifest
        // from a later revision, and a patch that is simply not in the file.
        let renamed = with_entry(&bytes, MANIFEST, |json| json.replace("Paper", "Card"));
        let resized = with_entry(&bytes, MANIFEST, |json| {
            json.replace("\"canvas\":[64,64]", "\"canvas\":[65,64]")
        });
        let newer = with_entry(&bytes, MANIFEST, |json| {
            json.replace("\"version\":1", "\"version\":99")
        });
        let truncated = without_entry(&bytes, &patch_src(0));

        for (what, doctored) in [
            ("renamed", renamed),
            ("resized", resized),
            ("newer", newer),
            ("truncated", truncated),
        ] {
            let doc = docimport::read_openraster(&doctored)
                .unwrap_or_else(|e| panic!("{what} must still open: {e}"));
            assert!(doc.history.is_none(), "{what}: the history was trusted");
            assert!(
                doc.warnings
                    .iter()
                    .any(|w| matches!(w, ImportWarning::HistoryDropped { .. })),
                "{what}: the drop was not reported"
            );
            // The picture is untouched — a dropped history costs the history.
            assert_eq!(doc.layers.len(), 2, "{what}");
            assert!(doc.open().history.is_empty(), "{what}");
        }
    }

    /// The check that stands between a saved history and a patch replayed into
    /// a layer that merely inherited a slot. It refuses the *whole* history:
    /// the entries are a sequence in which each restores the pixels the next
    /// one expects, so one missing from the middle is not a shorter history but
    /// a wrong one.
    #[test]
    fn a_patch_naming_a_slot_no_layer_holds_refuses_the_whole_history() {
        let stack = stack(&["Ink"]);
        let mut history = History::default();
        history.record(Edit::new(EditKind::Paint, patch(0, 4, 4, 1)));
        assert!(SaveHistory::new(&history, &stack).is_some());

        history.record(Edit::new(EditKind::Paint, patch(9, 4, 4, 2)));
        assert!(
            SaveHistory::new(&history, &stack).is_none(),
            "one unplaceable patch must refuse all of them"
        );
    }

    /// A session far over what a file will carry writes a bounded document and
    /// loses its *oldest* entries, which is the direction the in-memory budget
    /// already ages them out in.
    #[test]
    fn the_budget_bounds_the_file_and_drops_the_oldest() {
        // Patches of incompressible noise, so the budget is reached in a
        // sensible number of them rather than in thousands of flat ones.
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut noise = |n: usize| -> Vec<u8> {
            (0..n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    (seed >> 24) as u8
                })
                .collect()
        };

        let stack = stack(&["Ink"]);
        let mut history = History::default();
        let side = 1024u32;
        let count = 48;
        for _ in 0..count {
            let rect = PixelRect {
                x: 0,
                y: 0,
                width: side,
                height: side,
            };
            history.record(Edit::new(
                EditKind::Paint,
                PixelPatch::new(rect, 0, noise((side * side * 4) as usize)),
            ));
        }
        // Well past the file's budget, and comfortably inside memory's.
        assert!(history.used_bytes() > BUDGET_BYTES * 2);
        let last = history.entry_at(count - 1).unwrap().patch.bytes.clone();

        let pixels = vec![0u8; (side * side * 4) as usize];
        let layers = vec![SaveLayer {
            name: "Ink",
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            pixels: &pixels,
        }];
        let (bytes, _) = encode(&SaveDocument {
            size: UVec2::new(side, side),
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: &pixels,
            history: SaveHistory::new(&history, &stack),
        })
        .unwrap();

        // The whole archive, not only the history, so the bound is the one a
        // user would see on disk.
        assert!(
            bytes.len() < BUDGET_BYTES + 8 * 1024 * 1024,
            "the file ran to {} bytes",
            bytes.len()
        );

        let doc = docimport::read_openraster(&bytes).unwrap();
        let back = doc.open().history;
        assert!(back.len() < count, "nothing was dropped");
        assert!(back.len() > 1, "everything was dropped");
        assert_eq!(
            back.dropped() + back.len(),
            count,
            "an eviction that is not counted is one the list cannot admit to"
        );
        assert_eq!(
            back.entry_at(back.len() - 1).unwrap().patch.bytes,
            last,
            "the newest entry is the one that must survive"
        );
    }

    /// The manifest fingerprints the stack by name, so it has to record the
    /// names the *file* comes back with — `stack.xml` cannot hold a control
    /// character, and a document with one in a layer name would otherwise lose
    /// its history every single time it was saved.
    #[test]
    fn a_layer_name_the_xml_cannot_hold_does_not_cost_the_history() {
        let mut stack = stack(&["Ink"]);
        stack.get_mut(0).unwrap().name = "In\u{7}k".into();
        let mut history = History::default();
        history.record(Edit::new(EditKind::Paint, patch(0, 4, 4, 3)));

        let (_, doc) = round_trip(&stack, &history);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.layers[0].name, "Ink");
        assert_eq!(doc.open().history.len(), 1);
    }

    #[test]
    fn edit_kinds_survive_their_own_round_trip() {
        for kind in EditKind::ALL {
            assert_eq!(kind_from_id(&kind_id(kind)), Some(kind));
        }
        assert_eq!(kind_from_id("Smudge"), None);
    }

    /// Rebuild an archive with one entry passed through `f`.
    fn with_entry(bytes: &[u8], name: &str, f: impl Fn(String) -> String) -> Vec<u8> {
        rebuild(bytes, |entry, body| {
            if entry == name {
                Some(f(String::from_utf8(body.to_vec()).unwrap()).into_bytes())
            } else {
                Some(body.to_vec())
            }
        })
    }

    fn without_entry(bytes: &[u8], name: &str) -> Vec<u8> {
        rebuild(bytes, |entry, body| (entry != name).then(|| body.to_vec()))
    }

    fn rebuild(bytes: &[u8], f: impl Fn(&str, &[u8]) -> Option<Vec<u8>>) -> Vec<u8> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut body).unwrap();
            if let Some(body) = f(&name, &body) {
                out.start_file(&name, stored()).unwrap();
                out.write_all(&body).unwrap();
            }
        }
        out.finish().unwrap().into_inner()
    }
}
