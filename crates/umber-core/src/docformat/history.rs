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
//! would hold — through the same [`TileMask`](crate::damage::TileMask) a real
//! stroke does, so the raw column is the pieces and not the boxes round them.
//!
//! | session (arguments) | raw patches | Deflate | PNG (fast) |
//! |---|---|---|---|
//! | 120 full-canvas strokes (`120 0 1.0`) | 54.8 MB | 18.3 MB (3.0×), 0.85 s | 12.3 MB (4.5×), 0.12 s |
//! | the same, heavily grained (`120 1.0 1.0`) | 61.2 MB | 30.9 MB (2.0×), 1.44 s | 30.6 MB (2.0×), 0.19 s |
//! | 300 small sketching strokes (`300 0 0.25`) | 2.6 MB | 0.34 MB (7.7×), 0.10 s | 0.38 MB (6.8×), 0.01 s |
//!
//! PNG wins on size everywhere but the sketch — where the difference is 40 kB —
//! and wins on *time* by an order of magnitude everywhere, because it filters
//! each row against the one above before it deflates and then has far less left
//! to compress. Deflate is what the ZIP would have done for free, so it is the
//! alternative worth measuring, and it loses.
//!
//! The ratios are lower than they were before patches were cut to the cells a
//! stroke reached, and that is the tiling working rather than the compressor
//! failing: what a patch used to hold and PNG used to squeeze out for nothing
//! was mostly canvas the stroke never went near. The bytes on disk are
//! unchanged or better; it is the raw column that fell.
//!
//! Encoding runs **newest first** and stops at the budget, so a session far
//! over it pays neither the time nor the space for the entries it will not
//! keep. What that costs end to end, on a one-layer 2048² document:
//!
//! | session | file | save | open |
//! |---|---|---|---|
//! | 300 small strokes | 2.67 → 3.15 MB | 0.07 → 0.09 s | 0.03 → 0.04 s |
//! | 40 full-canvas strokes | 5.28 → 6.97 MB | 0.12 → 0.14 s | 0.05 → 0.07 s |
//! | 120 full-canvas strokes | 9.68 → 22.13 MB | 0.13 → 0.25 s | 0.05 → 0.18 s |
//!
//! Opening is in there because the patches have to be decoded before the
//! document appears, and that is time the artist spends waiting.
//!
//! Saving the history is still a **preference** rather than a policy, and the
//! last row is still why: an afternoon of full-canvas painting more than
//! doubles the document, and somebody synchronising their work to a network
//! drive is entitled to say no. It is on by default because the common case is
//! the first row — eighteen per cent, and free — and because a feature nobody
//! knows to switch on is one nobody gets.
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
use crate::history::{EditKind, History, PatchPiece};
use crate::layer::LayerStack;
use crate::time::Timestamp;

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
///
/// The bar for raising it is the same one [`super::VERSION`] answers to, one
/// level down: a revision an older build would **misread**. Adding the
/// per-entry timestamp did not qualify and deliberately did not bump this.
/// `ManifestEdit::at` is an optional field, and serde ignores a field it has
/// never heard of, so a build that predates it restores every patch and every
/// position exactly as before and merely shows no times. Bumping would instead
/// have made that build throw the whole history away — all the pixels, to
/// avoid losing the clock — which is a plainly worse trade. The test
/// `a_manifest_from_a_newer_revision_is_discarded_and_not_refused` pins the
/// behaviour a bump would rely on.
///
/// **2 is the first revision that earned it.** A patch became a set of pieces
/// rather than one rectangle ([`crate::damage`]), and a build that reads only
/// revision 1 would ignore `ManifestEdit::pieces`, take the entry's first PNG
/// for the whole rectangle, and write it back over pixels that were never part
/// of the edit — pixels it would then have no way to restore. That is a
/// document quietly damaged by an undo, which is the one outcome worth losing
/// a history over. Such a build now discards the history and opens the picture
/// whole, exactly as it does for a document that has none.
///
/// This build still reads revision 1: an entry with no `pieces` is one piece
/// covering the whole rectangle, which is precisely what revision 1 meant.
pub const VERSION: u32 = 2;

/// How much encoded patch data a document will carry.
///
/// 32 MB, against 512 MB in memory. At the 2–7× that painted patches compress
/// by, that is 64–220 MB of raw history — measured, **all** of 120 full-canvas
/// strokes (12.3 MB encoded), all of 120 heavily grained ones (30.6 MB, only
/// just), and all 300 of a sketching session with room to spare. It bounds the
/// extra size of the file, the extra time a save takes and the extra time an
/// *open* takes; the module docs have what those measure at.
///
/// It used to bind at 62 of those 120 strokes. Cutting patches to the cells a
/// stroke reached is what moved it, and the number was left where it is: what
/// a file will carry is a judgement about documents on somebody's disk, not a
/// consequence of how well this release happens to pack them.
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
    /// When it was painted. `None` for an entry that came out of a document
    /// written before this was recorded and is on its way back into one.
    at: Option<Timestamp>,
    /// The whole region the stroke damaged. The pieces are inside it and
    /// generally do not fill it — see [`crate::damage`].
    rect: PixelRect,
    /// The parts of it the stroke actually touched, each with its own pixels.
    pieces: &'a [PatchPiece],
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
                at: edit.at,
                rect: edit.patch.rect,
                pieces: edit.patch.pieces(),
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
    /// Archive entry holding the whole rectangle, in a revision-1 manifest.
    ///
    /// Nothing writes it any more; it is read so that a document saved before
    /// patches were made of pieces still opens with its history. An entry with
    /// no `pieces` is exactly that entry: one piece, covering `x, y, w, h`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub src: String,
    /// The parts of the rectangle the stroke actually touched, in the order
    /// they were recorded.
    ///
    /// Empty in a revision-1 manifest, which is what [`VERSION`] was raised
    /// for: a build that reads only revision 1 would ignore this field, decode
    /// the first piece as though it were the whole rectangle, and write it back
    /// over pixels that were never part of the edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pieces: Vec<ManifestPiece>,
    /// When the edit was made, in milliseconds since the Unix epoch, UTC.
    ///
    /// Optional in both directions, and that is the whole compatibility story
    /// for it — see [`VERSION`]. `default` so a manifest written before this
    /// existed reads as "not known" rather than failing to parse and costing
    /// the document its whole history; `skip_serializing_if` so a history read
    /// out of such a file and saved again writes exactly the bytes it did
    /// before, instead of a column of nulls.
    ///
    /// A number, not a formatted date: parsing a date is a place to be wrong,
    /// and every consumer of this field wants the integer anyway.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<i64>,
}

/// One piece of an edit: where it goes, and what holds its pixels.
#[derive(Serialize, Deserialize)]
pub(crate) struct ManifestPiece {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Archive entry holding this piece's PNG.
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
    //
    // An entry goes in whole or not at all. Its pieces are the parts of one
    // stroke's damage; half of them would restore half a stroke, which is not a
    // state the canvas was ever in.
    let mut used = 0usize;
    let mut kept: Vec<(usize, Vec<Vec<u8>>)> = Vec::new();
    for (i, edit) in history.entries.iter().enumerate().rev() {
        let mut pngs = Vec::with_capacity(edit.pieces.len());
        let mut entry_bytes = 0usize;
        for piece in edit.pieces {
            let png = encode_png(
                UVec2::new(piece.rect.width, piece.rect.height),
                &piece.bytes(),
            )?;
            entry_bytes += png.len();
            pngs.push(png);
        }
        if used + entry_bytes > BUDGET_BYTES {
            break;
        }
        used += entry_bytes;
        kept.push((i, pngs));
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
                    src: String::new(),
                    pieces: edit
                        .pieces
                        .iter()
                        .enumerate()
                        .map(|(p, piece)| ManifestPiece {
                            x: piece.rect.x,
                            y: piece.rect.y,
                            w: piece.rect.width,
                            h: piece.rect.height,
                            src: patch_src(n, p),
                        })
                        .collect(),
                    at: edit.at.map(Timestamp::unix_millis),
                }
            })
            .collect(),
    };

    for (n, (_, pngs)) in kept.iter().enumerate() {
        for (p, png) in pngs.iter().enumerate() {
            // Stored: a PNG is deflated already, so deflating it again in the
            // ZIP costs time and gains nothing.
            zip.start_file(patch_src(n, p), stored())?;
            zip.write_all(png)?;
        }
    }

    let json =
        serde_json::to_vec(&manifest).map_err(|e| SaveError::Io(std::io::Error::other(e)))?;
    zip.start_file(MANIFEST, deflated())?;
    zip.write_all(&json)?;
    Ok(true)
}

pub(crate) fn patch_src(index: usize, piece: usize) -> String {
    format!("umber/history/{index:04}-{piece:03}.png")
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
    /// mid-timeline, with both stacks intact, every patch byte for byte and
    /// every entry still carrying the moment it was painted. Restoring only the
    /// undo half would silently throw away work the artist had undone and meant
    /// to come back to; restoring it in the wrong order would replay the wrong
    /// pixels; losing the times would leave the History list unable to say how
    /// long any of it took the moment a document was reopened.
    #[test]
    fn a_history_survives_a_round_trip() {
        let stack = stack(&["Paper", "Ink"]);
        let (a, b) = (stack.get(0).unwrap().slot(), stack.get(1).unwrap().slot());

        // Fixed stamps rather than the clock, so the assertions below are
        // equality rather than a tolerance — and so the gaps between them are
        // known values a reopened list would have to reproduce.
        let at = |secs: i64| Some(Timestamp::from_unix_millis(secs * 1000 + 250));
        let mut history = History::default();
        history.record(Edit::made_at(
            EditKind::Paint,
            at(1_785_542_400),
            patch(a, 5, 3, 11),
        ));
        history.record(Edit::made_at(
            EditKind::Erase,
            at(1_785_542_403),
            patch(b, 4, 4, 22),
        ));
        history.record(Edit::made_at(
            EditKind::Paint,
            at(1_785_542_500),
            patch(b, 6, 2, 33),
        ));
        // Step back one, so the timeline straddles both stacks and the redo
        // side has something in it.
        let undone = history.take_undo().unwrap();
        history.push_redo(Edit::made_at(undone.kind, undone.at, patch(b, 6, 2, 44)));
        assert_eq!((history.len(), history.position()), (3, 2));

        let (bytes, doc) = round_trip(&stack, &history);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let opened = doc.open();
        let back = &opened.history;

        assert_eq!(back.len(), history.len());
        assert_eq!(back.position(), history.position());
        assert_eq!(back.dropped(), history.dropped());
        // The gaps the History list reads off, which is the point of storing
        // the times at all.
        assert_eq!(back.gap_at(0), None);
        assert_eq!(back.gap_at(1), Some(std::time::Duration::from_secs(3)));
        assert_eq!(back.gap_at(2), Some(std::time::Duration::from_secs(97)));
        // Millisecond resolution really made it into the file, rather than
        // being rounded away by a seconds-wide field.
        assert_eq!(back.time_at(0).unwrap().unix_millis() % 1000, 250);
        // And the manifest is revision 2, which a patch made of pieces earned
        // and the per-entry timestamp deliberately did not. See `VERSION`.
        assert!(
            manifest_of(&bytes).contains("\"version\":2"),
            "the manifest revision is not the one this build writes"
        );

        for i in 0..history.len() {
            let (before, after) = (history.entry_at(i).unwrap(), back.entry_at(i).unwrap());
            assert_eq!(after.kind, before.kind, "entry {i}");
            assert_eq!(after.at, before.at, "entry {i} lost the moment it was made");
            assert_eq!(after.patch.rect, before.patch.rect, "entry {i}");
            assert_eq!(
                patch_pixels(&after.patch),
                patch_pixels(&before.patch),
                "entry {i}"
            );
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
            json.replace("\"version\":2", "\"version\":99")
        });
        let truncated = without_entry(&bytes, &patch_src(0, 0));

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
        let last = patch_pixels(&history.entry_at(count - 1).unwrap().patch);

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
            patch_pixels(&back.entry_at(back.len() - 1).unwrap().patch),
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

    /// Both halves of the timestamp's compatibility story, which is why it did
    /// not bump [`VERSION`].
    ///
    /// An entry with no time writes a manifest with no `at` at all — byte for
    /// byte what a build predating this wrote — so a document opened by an
    /// older Umber and saved again is not quietly filled with nulls. And a
    /// manifest that never had the field reads back as "not known" rather than
    /// failing to parse, which would have cost the document its whole history
    /// over a column the picture does not depend on.
    #[test]
    fn a_manifest_without_times_is_written_and_read_as_one() {
        let stack = stack(&["Ink"]);
        let mut history = History::default();
        history.record(Edit::made_at(EditKind::Paint, None, patch(0, 4, 4, 7)));

        let (bytes, doc) = round_trip(&stack, &history);
        let manifest = manifest_of(&bytes);
        assert!(
            !manifest.contains("\"at\""),
            "an untimed entry wrote a time field: {manifest}"
        );

        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let back = doc.open().history;
        assert_eq!(back.len(), 1);
        assert_eq!(back.time_at(0), None, "a time was invented on the way in");
        assert_eq!(back.gap_at(0), None);
    }

    /// The property a future bump of [`VERSION`] would rely on, and the reason
    /// this one is safe to raise when something genuinely earns it: a manifest
    /// from a revision this build does not know is **discarded**, not refused.
    /// The document opens, the picture is whole, and only the history is lost.
    #[test]
    fn a_manifest_from_a_newer_revision_is_discarded_and_not_refused() {
        let stack = stack(&["Paper", "Ink"]);
        let mut history = History::default();
        history.record(Edit::new(EditKind::Paint, patch(0, 4, 4, 5)));
        let (bytes, _) = round_trip(&stack, &history);

        let newer = with_entry(&bytes, MANIFEST, |json| {
            json.replace("\"version\":2", "\"version\":99")
        });
        let doc = docimport::read_openraster(&newer).expect("the document must still open");
        assert!(doc.history.is_none(), "a newer manifest was trusted");
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::HistoryDropped { .. })),
            "the drop was not reported"
        );
        assert_eq!(doc.layers.len(), 2, "the picture was refused with it");
        assert!(doc.open().history.is_empty());
    }

    /// Every piece of a patch, as `(rect, pixels)` — what two patches have to
    /// agree on to be the same patch.
    fn patch_pixels(patch: &PixelPatch) -> Vec<(PixelRect, Vec<u8>)> {
        patch
            .pieces()
            .iter()
            .map(|p| (p.rect, p.bytes().into_owned()))
            .collect()
    }

    /// The manifest JSON out of a built archive, as text.
    fn manifest_of(bytes: &[u8]) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut entry = zip.by_name(MANIFEST).expect("the archive has a manifest");
        let mut json = String::new();
        std::io::Read::read_to_string(&mut entry, &mut json).unwrap();
        json
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
