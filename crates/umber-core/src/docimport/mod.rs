//! Importing layered documents written by other painting applications.
//!
//! ```no_run
//! # use std::path::Path;
//! let doc = umber_core::docimport::import(Path::new("sketch.ora"))?;
//! for warning in &doc.warnings {
//!     eprintln!("{warning}");
//! }
//! let opened = doc.open();
//! # Ok::<(), umber_core::docimport::ImportError>(())
//! ```
//!
//! # What is here and what is not
//!
//! Five formats land: OpenRaster (`.ora`), Krita (`.kra`), Photoshop (`.psd`),
//! Clip Studio Paint (`.clip`) and a flat `.png` as a single layer. MediBang,
//! Procreate, GIMP and layered TIFF are declined, each for a reason written
//! down in `docs/document-import.md`. The governing rule is that an import
//! which produces subtly wrong pixels is worse than one that refuses: a refusal
//! sends the artist to export a PNG or an ORA, whereas a wrong import wastes an
//! afternoon before they notice the colours moved.
//!
//! The same rule shapes what happens *inside* a supported format. Umber has
//! five blend modes and one mask per layer, so a real Photoshop file cannot
//! arrive intact. Every such loss appends an [`ImportWarning`], and the UI is
//! expected to show them. Clipping is no longer among the losses — Umber's own
//! flag means what Photoshop's does, so a clipped PSD layer arrives clipped.
//!
//! Masks are read out of ORA, out of a `.kra`'s **transparency masks** — the
//! one kind of Krita mask that means what Umber's does — and out of a `.clip`'s
//! layer masks. Krita's other four kinds (filter, transform, selection and
//! colorize) and Photoshop's masks are still reported as lost, the last of
//! those because `psd` 0.3.5 does not carry them out of the file at all. See
//! [`krita`], [`clipstudio`] and [`photoshop`].
//!
//! # Pixel convention
//!
//! [`ImportedLayer::pixels`] is canvas-sized RGBA8 in exactly the form a layer
//! texture holds — sRGB-encoded, alpha premultiplied in linear space. It can go
//! straight to `queue.write_texture`. See [`srgb`] for why that is the right
//! form and what the wrong ones look like.

mod blend;
mod clipstudio;
mod container;
/// The flattened picture a document already carries, for a file manager.
pub mod preview;
// Visible inside the crate so `tip::TipMask::from_picture` can read a PNG
// through the decoder that already exists rather than growing a second one.
pub(crate) mod flat;
mod history;
mod krita;
mod lzf;
mod openraster;
mod photoshop;
pub(crate) mod srgb;

#[cfg(test)]
mod fixtures;

use std::fmt;
use std::path::Path;

use glam::UVec2;

use crate::document::{Background, Document};
use crate::effect;
use crate::history::{Edit, EditBody, History, PatchPiece, PixelPatch};
use crate::layer::{BlendMode, LayerStack};

pub use history::{ImportedBody, ImportedEdit, ImportedHistory, ImportedPiece};

/// Formats [`import`] can read.
///
/// Ordered so that the file-open dialog can list the layered formats first.
pub fn supported_extensions() -> &'static [&'static str] {
    &["ora", "kra", "psd", "clip", "png"]
}

/// Read a document written by another application.
///
/// Dispatches on the file extension. The readers still check the file's own
/// magic, so a mislabelled file fails with [`ImportError::Malformed`] rather
/// than by misreading.
/// How far a decode has got, for something drawing a progress bar.
///
/// **Layers rather than bytes**, because layers are what the readers loop over
/// and what the wait actually is: measured on real documents, reading the file
/// off disk is 55 ms of a 13.4 second open and building the stack afterwards is
/// nothing — the whole of it is decoding one layer's blocks after another. See
/// `examples/measure-open.rs`, which is what settled that and is the thing to
/// re-run before anybody reports a different shape of wait.
///
/// `done` counts layers finished, `total` is how many the file declares. A
/// caller may be told `total` is zero, which is a reader that has not counted
/// them yet — a bar must draw that as "no idea" rather than as complete, the
/// rule `update::Stage::progress` already keeps with its `Option`.
pub type Progress<'a> = &'a (dyn Fn(u32, u32) + Send + Sync);

/// A `Progress` that discards, for every caller that is not drawing a bar.
fn silent(_: u32, _: u32) {}

pub fn import(path: &Path) -> Result<ImportedDocument, ImportError> {
    import_reporting(path, &silent)
}

/// The same, telling `progress` as each layer lands.
///
/// Separate from [`import`] rather than an argument on it, because almost every
/// caller — the autosave's recovery, the tests, the examples — wants nothing to
/// do with a bar, and threading `&silent` through all of them would be noise
/// around the one call site that cares.
pub fn import_reporting(
    path: &Path,
    progress: Progress<'_>,
) -> Result<ImportedDocument, ImportError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Checked before the read so that an unreadable name gives the useful
    // answer ("Umber cannot open .mdp files") rather than an I/O error.
    if !supported_extensions().contains(&ext.as_str()) {
        return Err(ImportError::UnsupportedExtension(ext));
    }

    let bytes = std::fs::read(path)?;
    let doc = match ext.as_str() {
        "ora" => openraster::read(&bytes, progress),
        "kra" => krita::read(&bytes, progress),
        "psd" => photoshop::read(&bytes, progress),
        "clip" => clipstudio::read(&bytes, progress),
        // A flat picture is one layer and is decoded in one step, so there is
        // nothing between "started" and "finished" to report.
        "png" => flat::read_png(&bytes),
        _ => unreachable!("extension was checked against supported_extensions"),
    }?;

    doc.validate()?;
    Ok(doc)
}

/// Read an OpenRaster archive already in memory.
///
/// The same reader [`import`] uses, without the filesystem. Public because ORA
/// is also what [`crate::docformat`] writes, so this is the other half of the
/// document round trip — and because a caller with the bytes in hand should not
/// have to write them to a temporary file to read them.
pub fn read_openraster(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let doc = openraster::read(bytes, &silent)?;
    doc.validate()?;
    Ok(doc)
}

/// Which application's format a document came out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    OpenRaster,
    Krita,
    Photoshop,
    ClipStudio,
    Png,
}

impl SourceFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenRaster => "OpenRaster",
            Self::Krita => "Krita",
            Self::Photoshop => "Photoshop",
            Self::ClipStudio => "Clip Studio Paint",
            Self::Png => "PNG",
        }
    }
}

/// One imported layer, already the size of the canvas.
///
/// Source formats store layers as sub-rectangles with an offset; that is
/// resolved during import because Umber's layers are all canvas-sized slices of
/// one texture array.
#[derive(Clone)]
pub struct ImportedLayer {
    pub name: String,
    pub visible: bool,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub blend: BlendMode,
    /// `width * height * 4` bytes, sRGB-encoded with premultiplied alpha —
    /// see the module docs.
    pub pixels: Vec<u8>,
    /// The layer's mask, canvas-sized and in the same form as `pixels` — so it
    /// goes straight to `write_texture` like everything else here.
    ///
    /// Only the **red** channel is ever read, and it holds sRGB-encoded
    /// coverage rather than a linear multiplier; `srgb::encode_coverage` is
    /// the one place another application's mask byte becomes one of these.
    ///
    /// Filled by ORA and by `.kra`'s transparency masks. A `.psd` mask is
    /// still reported as lost, and that is the `psd` crate's limit rather than
    /// a decision: see `photoshop`'s module docs.
    pub mask: Option<Vec<u8>>,
    /// The layer's effects, in composite order.
    ///
    /// Filled by ORA and by nothing else. Photoshop's `.psd` carries layer
    /// effects and `photoshop.rs` does not read them — see
    /// `docs/layer-effects.md` §8.3, which says why the prior is poor and that
    /// whether `psd` 0.3.5 can even reach them has not been checked.
    ///
    /// Enabled and disabled alike, exactly as the file held them: a switched-off
    /// effect is a set of parameters somebody dialled, and it costs the budget
    /// nothing. [`ImportedDocument::open`] is where they reach a
    /// [`LayerStack`].
    pub effects: Vec<crate::effect::Effect>,
    /// Bounded by the alpha of the nearest unclipped layer below.
    pub clipped: bool,
    /// Refuses edits until unlocked.
    pub locked: bool,
    /// Which link group this layer belongs to, if any. See
    /// [`crate::docformat::LINK_GROUP_ATTR`].
    pub link: Option<u8>,
    /// What set this layer's pixels, where the file said and the fingerprint
    /// agreed.
    ///
    /// `None` for every format but Umber's own — no other application records
    /// one — and `None` too where the record was unreadable, from a newer
    /// revision, or **fingerprinted a different image than the one in the file**,
    /// which is what a build that painted over the text leaves behind. Every one
    /// of those raises an [`ImportWarning::TextDropped`] and the layer opens as
    /// ordinary paint, which is what it now is. See [`crate::textobj`].
    pub text: Option<Box<crate::textobj::TextObject>>,
    /// How deeply nested, 0 at the top level. See [`crate::layer`]'s docs.
    ///
    /// Only ORA can carry nesting today. `.kra` has groups and `.psd` has them
    /// too, and both still flatten — reading either is a change to that
    /// decoder, not to this field.
    pub depth: u8,
    /// This entry is a folder: it holds no pixels and takes no slot.
    ///
    /// `pixels` is empty for one, which is why nothing that walks the layer
    /// list for something to upload may assume every entry has any.
    pub folder: bool,
}

impl ImportedLayer {
    /// A visible, fully opaque layer with none of the flags set.
    ///
    /// Every reader builds its layers through this, so a field added above does
    /// not mean touching four readers and their fixtures.
    pub fn new(name: impl Into<String>, blend: BlendMode, pixels: Vec<u8>) -> Self {
        Self {
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend,
            pixels,
            mask: None,
            effects: Vec::new(),
            clipped: false,
            locked: false,
            link: None,
            text: None,
            depth: 0,
            folder: false,
        }
    }

    /// A folder: a name, an eye, a nesting level and no pixels at all.
    pub fn folder(name: impl Into<String>, depth: u8, visible: bool) -> Self {
        Self {
            visible,
            depth,
            folder: true,
            ..Self::new(name, BlendMode::Normal, Vec::new())
        }
    }
}

impl fmt::Debug for ImportedLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The pixel buffer is megabytes; printing it helps nobody.
        f.debug_struct("ImportedLayer")
            .field("name", &self.name)
            .field("visible", &self.visible)
            .field("opacity", &self.opacity)
            .field("blend", &self.blend.label())
            .field("pixels", &format_args!("{} bytes", self.pixels.len()))
            .field("mask", &self.mask.is_some())
            .field("effects", &self.effects.len())
            .field("clipped", &self.clipped)
            .field("locked", &self.locked)
            .field("link", &self.link)
            .field("text", &self.text.is_some())
            .finish()
    }
}

/// A document read from another application's file.
#[derive(Debug)]
pub struct ImportedDocument {
    pub format: SourceFormat,
    pub size: UVec2,
    /// Bottom to top, matching [`LayerStack`]'s own order.
    pub layers: Vec<ImportedLayer>,
    /// Which layer was selected when the document was written.
    ///
    /// `None` for every format but Umber's own, which is nearly all of them:
    /// ORA, KRA and PSD each have a notion of a current layer, but it lives in
    /// application-private data rather than in the part of the file this module
    /// reads. `None` means the top layer, which is what a painter expects to
    /// land on when opening someone else's document anyway.
    pub active: Option<usize>,
    /// What lies under the stack.
    ///
    /// [`Background::Transparent`] for every format but Umber's own, and that
    /// is not a default so much as a fact: no other application's ORA, KRA or
    /// PSD states a document background, so anything else here would be a
    /// colour the file does not contain. Umber writes its own as a bottom layer
    /// *and* an attribute — see [`crate::docformat`] — which is what lets this
    /// come back without the file becoming unreadable elsewhere.
    pub background: Background,
    /// Pixels per inch, when the file says. `None` means it did not, and the
    /// document opens at [`Document::DEFAULT_DPI`].
    ///
    /// Not reported as a loss. Resolution changes no pixel — the picture is
    /// identical either way — and a warning on every PSD and PNG would be noise
    /// in the one list that has to stay worth reading.
    pub dpi: Option<f32>,
    /// The undo history the document was saved with, if it had one.
    ///
    /// `None` for every format but Umber's own — no other application records
    /// one in a file — and `None` too for an Umber document written before
    /// histories were saved, or one whose history could not be trusted against
    /// the stack that loaded. See [`history`].
    pub history: Option<ImportedHistory>,
    /// Everything the import could not represent. Empty means nothing was lost.
    pub warnings: Vec<ImportWarning>,
}

/// A layer's pixels together with the texture-array slot they belong in.
pub struct LayerUpload {
    pub slot: u32,
    pub pixels: Vec<u8>,
}

/// Everything a document needs to be opened: the engine state built from an
/// import, ready for the caller to give it GPU storage.
///
/// A struct rather than a tuple because there are now four of them and the
/// history's absence has to be readable at the call site.
pub struct Opened {
    pub document: Document,
    pub stack: LayerStack,
    pub uploads: Vec<LayerUpload>,
    /// Empty unless the file carried one that resolved against `stack`.
    pub history: History,
}

impl ImportedDocument {
    pub fn document(&self) -> Document {
        Document::new(self.size.x, self.size.y)
            .with_background(self.background)
            .with_dpi(self.dpi.unwrap_or(Document::DEFAULT_DPI))
    }

    /// Build the engine-side state for this import.
    ///
    /// Returns the uploads separately rather than hanging pixels off the stack:
    /// `LayerStack` deliberately holds no pixel data — it holds slots, and the
    /// pixels live on the GPU. The caller writes each `LayerUpload` into its
    /// slot and the document is open.
    pub fn open(mut self) -> Opened {
        let document = self.document();
        let mut stack = LayerStack::empty();
        let mut uploads = Vec::with_capacity(self.layers.len());

        // **The budget is settled here as well as in the reader, and that is
        // what makes it total.** `openraster::read` calls this too and turns
        // what it disables into an `ImportWarning`, which is the only place a
        // warning can be raised; but `ImportedLayer::effects` is a public field
        // on a public struct, so a second importer, or a caller building an
        // `ImportedDocument` by hand, can arrive over budget without ever going
        // through that reader. Left to `set_effect` alone the excess would be
        // refused by a `bool` nobody reads — the shrug CLAUDE.md's "Partial
        // exhaustiveness" section ends on, one level up from where this diff
        // already avoided it.
        //
        // Calling it twice costs nothing and cannot double-count: it is
        // idempotent by construction, because after it runs the enabled count
        // is within the budget and a second pass finds nothing to disable.
        // `an_over_budget_document_is_trimmed_by_open_even_with_no_reader_
        // between` is the guard.

        disable_effects_over_budget(&mut self.layers);

        let saved_history = self.history;

        // Built entry by entry rather than by adding layers and then correcting
        // them, because a folder is not something `add` can be asked for: it
        // takes no slot, and `add`'s whole contract is to hand one back. The
        // count is guaranteed to fit by `validate`.
        for layer in &self.layers {
            stack.push_imported(layer.folder, layer.depth, layer.name.clone());
        }
        // Whatever the file said about nesting, the stack has to describe a
        // tree — a depth capped at `MAX_DEPTH` on the way in can leave a folder
        // and its contents at the same level. The pixels are all present either
        // way; only the grouping changes, and the reader has already said so.
        stack.flatten_ill_formed();

        for (i, layer) in self.layers.into_iter().enumerate() {
            let slot = {
                let dst = stack
                    .get_mut(i)
                    .expect("the stack was sized to the import; see `validate`");
                dst.visible = layer.visible;
                dst.opacity = layer.opacity;
                dst.blend = layer.blend;
                dst.clipped = layer.clipped;
                dst.locked = layer.locked;
                dst.link = layer.link;
                dst.slot()
            };
            // Through `LayerStack::set_text`, which refuses a folder — the
            // reader never puts a record on one, and the refusal is the model's
            // to make rather than something two call sites both remember.
            //
            // **The answer is not discarded**, for the reason `set_effects`'
            // below is not. `ImportedLayer::text` is a public field, so a later
            // reader that did put a record on a folder would otherwise have it
            // dropped in silence, with no `TextDropped` beside it — a refusal
            // read as a shrug, which is the trap this codebase has already paid
            // for once. There is nowhere to raise a warning from here (the list
            // was consumed before `open` was called), so the assertion is what
            // says the reader must not produce one.
            if let Some(text) = layer.text {
                let placed = stack.set_text(i, *text);
                debug_assert!(
                    placed,
                    "a reader put a text record on an entry that cannot hold one"
                );
            }
            // Through `set_effect` rather than onto the field, because
            // `Layer::effects` is private and the invariants it carries — at
            // most one per kind, always in composite order — are the stack's to
            // maintain. It re-derives the order, so a file whose sequence was
            // written by a build that ordered them differently still comes back
            // right.
            //
            // **The whole set at once, never a loop of `set_effect`.** A loop
            // is silently wrong twice: `set_effect` *replaces* what the layer
            // held of a kind and answers `true`, so a record naming two drop
            // shadows would install one and say nothing; and it asks the budget
            // once per effect, so a set that fits could be refused half way
            // along and leave the layer holding a prefix of the file. Both are
            // the shrug CLAUDE.md's "Partial exhaustiveness" section ends on,
            // and `LayerStack::set_effects` is where they stop being possible —
            // it refuses the set whole, and a refusal moves nothing.
            //
            // The assertion is what is left. `set_effects` refuses a folder, a
            // duplicate kind and the budget; the budget was settled above for
            // every caller and not only for the ORA reader, the duplicate was
            // refused by `load_effects` asking the same rule, and `parse_stack`
            // reads the attribute off a `<layer>` alone. So a `false` here
            // means a caller hand-built something none of those cover, and it
            // says so rather than losing the effects quietly.
            if !layer.effects.is_empty() {
                let installed = stack.set_effects(i, &layer.effects);
                debug_assert!(
                    installed,
                    "an imported layer's effects were refused by the stack built for them"
                );
            }
            // A folder holds no pixels and takes no slice, so there is nothing
            // to upload and nothing to clear.
            let Some(slot) = slot else { continue };
            uploads.push(LayerUpload {
                slot,
                pixels: layer.pixels,
            });
            // A mask is another slice of the same array, so it is another
            // upload and nothing here has to know it is a mask. `add_mask`
            // cannot fail on a layer that has just been built, and the caller
            // fills the slice from this upload rather than with the opaque
            // white a *new* mask starts as.
            if let Some(mask) = layer.mask
                && let Some(mask_slot) = stack.add_mask(i)
            {
                uploads.push(LayerUpload {
                    slot: mask_slot,
                    pixels: mask,
                });
            }
        }

        // Umber's own documents remember which layer was selected; for
        // everything else the top one is what an artist expects to land on —
        // and the top *layer*, not the top entry, because a document whose
        // topmost entry is a folder would otherwise open with nowhere to paint
        // and no indication why.
        let active = self.active.filter(|i| *i < stack.len()).unwrap_or_else(|| {
            stack
                .layers()
                .iter()
                .rposition(|l| !l.is_folder())
                .unwrap_or(stack.len() - 1)
        });
        stack.set_active(active);

        // The one place a saved history's stack positions become texture slots,
        // and it is here rather than in the reader because the slots do not
        // exist until the stack above has been built. A position out of range
        // cannot arrive — `docimport::history` checked every one against the
        // layers that loaded — but the history is dropped rather than trusted if
        // one does, because replaying a patch into the wrong layer is the whole
        // failure this design exists to avoid.
        let history = saved_history
            .and_then(|saved| {
                let mut entries = Vec::with_capacity(saved.entries.len());
                for edit in saved.entries {
                    // `made_at`, never `new`: an edit read out of a file was
                    // made when the file says, and stamping it with the moment
                    // the document was opened would tell the History list that
                    // yesterday's afternoon of painting happened in one second.
                    let body = match edit.body {
                        ImportedBody::Pixels {
                            layer,
                            mask,
                            rect,
                            pieces,
                        } => {
                            // The layer's own slice, or its mask's. A mask
                            // entry naming a layer that came back without one
                            // drops the whole history rather than being
                            // replayed into the pixels, which is what the `?`
                            // does — the same rule the positions themselves
                            // answer to.
                            // A position naming a *folder* is one whose patch
                            // has nowhere to be replayed, and it drops the
                            // whole history for the reason a position out of
                            // range does: an entry replayed into the wrong
                            // layer is the failure this design exists to avoid,
                            // and a shorter history is not a safer one.
                            let dst = stack.get(layer)?;
                            let slot = if mask { dst.mask()? } else { dst.slot()? };
                            let pieces = pieces
                                .into_iter()
                                .map(|p| PatchPiece::new(p.rect, p.bytes))
                                .collect();
                            EditBody::Pixels(PixelPatch::from_pieces(rect, slot, pieces))
                        }
                        // Nothing to place: a flip belongs to the whole
                        // document, so there is no layer for it to be replayed
                        // into the wrong one of.
                        ImportedBody::Flip => EditBody::Flip,
                    };
                    entries.push(Edit::made_at(edit.kind, edit.at, body));
                }
                let mut history = History::default();
                history.restore(entries, saved.position, saved.dropped);
                Some(history)
            })
            .unwrap_or_default();

        Opened {
            document,
            stack,
            uploads,
            history,
        }
    }

    /// Largest canvas edge an import will accept.
    ///
    /// Not a GPU limit — `umber-core` cannot see the adapter — but a sanity
    /// bound: 16384 is the ceiling on current desktop hardware, and one layer
    /// that size is already a gigabyte. The caller must still check the real
    /// `max_texture_dimension_2d` before uploading.
    pub const MAX_DIMENSION: u32 = 16384;

    /// Total layer bytes an import will accept, across the whole stack.
    ///
    /// **It was 2 GiB, and at that figure the reader was stricter than the
    /// writer.** Umber will make a canvas up to [`Document::MAX_EDGE`] on a
    /// side and put [`LayerStack::MAX`] layers on it, so it could save a
    /// document it then refused to reopen: 15000×5000 is 300 MB a layer, and
    /// the eighth layer put it past 2 GiB. That is the failure the mask
    /// paragraph on [`check_bounds`] already argues against, arriving through
    /// the byte total instead.
    ///
    /// **It cannot be raised all the way, and the tension is real rather than
    /// an oversight.** The bound that would never refuse Umber's own work is
    /// `MAX_DIMENSION² × 4 × LayerStack::MAX`, which is 68.7 GB — and at that
    /// figure the check is exactly what the dimension and count checks already
    /// permit, so it stops guarding anything. What it guards is a header: a
    /// layer's buffer is allocated canvas-sized whatever the source data
    /// weighs, so a few kilobytes of hostile file can ask for tens of
    /// gigabytes before a single pixel is decoded. A finite figure is what
    /// turns that into a sentence instead of the process being killed.
    ///
    /// 16 GiB is where the two meet. It admits every document anybody has
    /// actually brought here — a 10000² canvas at 40 layers is 16.0 GB, a
    /// 15000×5000 at 50 is 15.0 GB — and still refuses the absurd. Past it the
    /// artist is told the figure and told to reduce the stack, which is
    /// something they can act on; see [`ImportError::StackTooLarge`].
    ///
    /// [`Document::MAX_EDGE`]: crate::document::Document::MAX_EDGE
    pub const MAX_TOTAL_BYTES: u64 = 16 << 30;

    /// Check what [`open`](Self::open) relies on.
    ///
    /// Every reader guards its own bounds before decoding, but a reader that
    /// grew a flattening fallback could still hand back something the stack
    /// cannot hold; this is the one place that has to be true.
    fn validate(&self) -> Result<(), ImportError> {
        // A document of nothing but folders has nothing to show and nowhere to
        // paint, so it is as empty as one with no entries at all.
        if !self.layers.iter().any(|l| !l.folder) {
            return Err(ImportError::Empty {
                format: self.format,
            });
        }
        if self.layers.len() > LayerStack::MAX {
            return Err(ImportError::TooManyLayers {
                found: self.layers.len(),
                max: LayerStack::MAX,
            });
        }
        let expected = self.size.x as usize * self.size.y as usize * 4;
        // Folders are skipped: one holds no pixels, so "canvas-sized" is not a
        // thing to be true of it. Everything else has to be exactly that,
        // because it goes straight to `write_texture`.
        for layer in self.layers.iter().filter(|l| !l.folder) {
            debug_assert_eq!(
                layer.pixels.len(),
                expected,
                "reader produced a layer that is not canvas-sized"
            );
            debug_assert!(
                layer.mask.as_ref().is_none_or(|m| m.len() == expected),
                "reader produced a mask that is not canvas-sized"
            );
        }
        Ok(())
    }
}

/// Something the import could not represent, phrased for the user.
///
/// Typed rather than a bare string so the UI can group and count them — a
/// forty-layer Photoshop file with a mask on every layer should not produce
/// forty lines of prose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportWarning {
    /// The nearest available blend mode was substituted.
    BlendApproximated {
        layer: String,
        source: String,
        used: &'static str,
    },
    /// Umber has nothing like the source mode; the layer is Normal.
    BlendDropped { layer: String, source: String },
    /// A layer group lost its grouping.
    ///
    /// Raised from three places that lose it two different ways: a `.psd` group
    /// becomes plain layers at the top level, while an ORA, `.kra` or `.clip`
    /// group nested deeper than
    /// [`LayerStack::MAX_DEPTH`](crate::LayerStack::MAX_DEPTH) is merged into
    /// the folder outside it. So the sentence names the loss —
    /// the grouping — and deliberately says nothing about where the layers
    /// ended up or that they all arrived. It used to claim "Umber has no layer
    /// groups", which folders made false; a draft after that claimed the layers
    /// were all there, which is not this warning's to promise. A child Umber
    /// cannot rasterise raises its own [`Self::LayerSkipped`] beside this one,
    /// and the two must not contradict each other.
    GroupFlattened { group: String },
    /// A group's own opacity was folded into its children, which is only the
    /// same picture when they do not overlap.
    GroupOpacityFolded { group: String },
    /// A layer mask was ignored, so the layer covers more than it should.
    MaskIgnored { layer: String },
    /// Something hanging off a layer that Umber has no place for at all — a
    /// Krita filter or selection mask, a mask that was switched off, a second
    /// mask on a layer that can hold one.
    ///
    /// Deliberately *not* [`MaskIgnored`](Self::MaskIgnored): that one says the
    /// layer now covers more than it did, which is true of a transparency mask
    /// Umber could not read and false of every case here — a disabled mask
    /// changed no pixel in the source either, and a filter mask never bounded
    /// the layer's alpha in the first place. Two losses that read identically
    /// to the artist would be one warning; these do not.
    MaskUnsupported { layer: String, what: String },
    /// The layer names an effects record that could not be read, so the layer
    /// arrives without its effects.
    ///
    /// **That layer's effects go whole and nothing else is touched**, which is
    /// deliberately *not* the saved history's "anything that does not line up
    /// exactly is dropped, whole" — and the difference is what the two things
    /// are. A history is a *sequence* in which each entry restores the pixels
    /// the next expects, so one missing from the middle is a wrong history
    /// rather than a short one. Effects are independent: one layer's say
    /// nothing about another's, and nothing downstream reads across them. So
    /// this is the mask's rule instead ([`Self::MaskIgnored`]) — keep the
    /// picture, lose the decoration, and say so. Refusing the document over an
    /// unreadable side file would cost the artist their painting to protect a
    /// shadow.
    ///
    /// "Whole" still means whole *within the layer*: the record is one RON
    /// sequence, so a single malformed effect in it takes that layer's others
    /// with it rather than being skipped past.
    EffectsIgnored { layer: String, reason: String },
    /// The document holds more enabled effects than Umber can draw, so the
    /// excess were switched off.
    ///
    /// Raised once for the document rather than once per layer, because it is
    /// a statement about the document — thirty lines each saying the same
    /// number would be the noise that stops this list being read.
    ///
    /// **Switched off, not removed.** The parameters stay in the layer and in
    /// the next save, so nothing is lost and one can be traded for another the
    /// day there is a control to trade them with; deleting them would be the
    /// silent loss, and `Effect::enabled` already means "no draw and no bake,
    /// and therefore nothing charged against the budget".
    ///
    /// The sentence therefore states what happened and does **not** tell the
    /// artist to switch one off — there is no control that does, because
    /// nothing in `umber-app` draws effects yet. A notice that instructs an
    /// action the interface cannot perform is the lying control this project
    /// refuses everywhere else, and it is easy to write months before the
    /// control exists.
    EffectsOverBudget { disabled: usize, max: usize },
    /// A layer could not be brought across at all.
    LayerSkipped { layer: String, reason: String },
    /// A layer that was not made of pixels arrived as pixels.
    ///
    /// Deliberately *not* [`LayerSkipped`](Self::LayerSkipped): the picture is
    /// all there and looks right, which is the whole difference — what is lost
    /// is that a caption cannot be retyped and a vector cannot be re-pulled.
    /// Clip Studio's own PSD export makes exactly this trade, and an artist who
    /// is told can go back and export the text separately.
    LayerRasterised { layer: String, what: String },
    /// Layer structure was lost and the flattened image was used instead.
    DocumentFlattened { reason: String },
    /// Pixels were taken to be sRGB when the file said otherwise.
    ColourProfileAssumed { detail: String },
    /// The document carried an undo history that could not be trusted against
    /// the stack that loaded, so none of it was restored.
    ///
    /// Only ever raised where the file actually claimed to have one — a
    /// document with no history says nothing, which is what keeps this list
    /// worth reading. The whole history goes or none of it does: the entries are
    /// a sequence in which each restores the pixels the next one expects, so one
    /// missing from the middle is not a shorter history but a wrong one.
    HistoryDropped { reason: String },
    /// The layer said its pixels were set as text, and the record could not be
    /// trusted, so the layer opens as ordinary paint.
    ///
    /// Only ever raised where the file actually claimed one — a layer with no
    /// [`crate::docformat::TEXT_ATTR`] says nothing, which is every layer of every
    /// document any other application wrote.
    ///
    /// The commonest reason by far is the one this warning exists for: **the
    /// pixels are no longer the ones the text made.** A build that has never
    /// heard of the attribute opens the document, the artist paints on the layer,
    /// and it is saved again with the record still beside pixels it did not
    /// make. Re-rendering then would destroy that painting, so the record goes
    /// and the picture stays. Nothing in the picture is ever lost to this.
    TextDropped { layer: String, reason: String },
}

impl fmt::Display for ImportWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BlendApproximated {
                layer,
                source,
                used,
            } => write!(
                f,
                "Layer “{layer}”: blend mode {source} is not available; the nearest, {used}, was used instead."
            ),
            Self::BlendDropped { layer, source } => write!(
                f,
                "Layer “{layer}”: blend mode {source} has no equivalent in Umber; the layer is now Normal."
            ),
            Self::GroupFlattened { group } => {
                write!(
                    f,
                    "Group “{group}” was flattened, so its layers are no longer grouped together."
                )
            }
            Self::GroupOpacityFolded { group } => write!(
                f,
                "Group “{group}” had its own opacity, which was folded into its layers; overlapping layers inside it will look slightly different."
            ),
            Self::MaskIgnored { layer } => write!(
                f,
                "Layer “{layer}” has a mask, which was ignored. The layer covers more than it did."
            ),
            Self::MaskUnsupported { layer, what } => {
                write!(f, "Layer “{layer}”: {what} was not imported.")
            }
            Self::EffectsIgnored { layer, reason } => write!(
                f,
                "Layer “{layer}” has layer effects that could not be read ({reason}), so it \
                 opens without them."
            ),
            Self::EffectsOverBudget { disabled, max } => write!(
                f,
                "This document has more layer effects than Umber can draw at once, so \
                 {disabled} of them were switched off. Umber draws up to {max}. Their \
                 settings were kept and are saved with the document."
            ),
            Self::LayerSkipped { layer, reason } => {
                write!(f, "Layer “{layer}” could not be imported: {reason}.")
            }
            Self::LayerRasterised { layer, what } => write!(
                f,
                "Layer “{layer}” was {what}, and arrived as ordinary pixels. Every pixel is \
                 there; it cannot be edited as what it was."
            ),
            Self::DocumentFlattened { reason } => write!(
                f,
                "The layers could not be read ({reason}), so the flattened image was imported as a single layer."
            ),
            Self::ColourProfileAssumed { detail } => write!(
                f,
                "Colours were read as sRGB ({detail}); they may not match the original exactly."
            ),
            Self::HistoryDropped { reason } => write!(
                f,
                "The saved undo history was not restored ({reason}), so the document opens with an empty one. Nothing in the picture was lost."
            ),
            Self::TextDropped { layer, reason } => write!(
                f,
                "Layer “{layer}” was saved as text, and {reason}, so it opens as ordinary paint. Every pixel is there; the text cannot be edited again."
            ),
        }
    }
}

/// Why an import failed.
#[derive(Debug)]
pub enum ImportError {
    Io(std::io::Error),
    /// Nothing here reads that extension.
    UnsupportedExtension(String),
    /// The file is damaged, or is not the format its name claims.
    Malformed {
        format: SourceFormat,
        detail: String,
    },
    /// A valid file using something this reader will not guess at. Distinct
    /// from `Malformed` because the fault is ours, not the file's.
    Unsupported {
        format: SourceFormat,
        detail: String,
    },
    /// Written by a later version of Umber than this one.
    ///
    /// Refused rather than read as far as it goes: a newer format revision
    /// exists precisely because it stores something this build has no idea it
    /// is dropping, and opening the file anyway would show the artist a picture
    /// that is quietly missing part of their work. The message points at the
    /// two ways out — update, or use another OpenRaster application, which can
    /// still read the file because that is all it is.
    NewerVersion {
        version: u32,
        supported: u32,
    },
    /// More layers than the texture array has slices.
    TooManyLayers {
        found: usize,
        max: usize,
    },
    /// One edge is past [`ImportedDocument::MAX_DIMENSION`].
    CanvasTooLarge {
        width: u32,
        height: u32,
    },
    /// The canvas is fine and the *stack* is not: every layer is canvas-sized
    /// and they are all held in host memory at once, so a legal canvas with
    /// enough layers on it is past [`ImportedDocument::MAX_TOTAL_BYTES`].
    ///
    /// **Separate from `CanvasTooLarge`, and that is the whole point of it.**
    /// Both refusals used to be that one variant, so a 15000×5000 Clip Studio
    /// document — an edge well inside `MAX_DIMENSION`, refused for its twenty
    /// layers — told the artist their canvas was larger than Umber can open.
    /// It is not, and no amount of shrinking it would have helped: what they
    /// had to do was reduce the *stack*, which the sentence never mentioned.
    /// A refusal that names the wrong bound is worse than a vague one, because
    /// it sends somebody to fix the thing that is not broken.
    StackTooLarge {
        width: u32,
        height: u32,
        /// Layers holding pixels. Folders are not counted — one allocates
        /// nothing, so it cannot be part of what does not fit.
        layers: usize,
        /// What those layers come to, which is the figure actually compared.
        bytes: u64,
    },
    /// A well-formed file with nothing to paint on.
    Empty {
        format: SourceFormat,
    },
}

impl From<std::io::Error> for ImportError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "The file could not be read: {e}"),
            Self::UnsupportedExtension(ext) if ext.is_empty() => {
                write!(f, "The file has no extension, so its format is unknown.")
            }
            Self::UnsupportedExtension(ext) => {
                write!(f, "Umber cannot open .{ext} files.")
            }
            Self::Malformed { format, detail } => {
                write!(
                    f,
                    "This is not a readable {} file: {detail}.",
                    format.label()
                )
            }
            Self::Unsupported { format, detail } => {
                write!(
                    f,
                    "This {} file uses {detail}, which Umber cannot read.",
                    format.label()
                )
            }
            Self::NewerVersion { version, supported } => write!(
                f,
                "This document was saved by a newer version of Umber (document format \
                 {version}; this build reads up to {supported}). Update Umber to open it, \
                 or open it in another OpenRaster application."
            ),
            Self::TooManyLayers { found, max } => write!(
                f,
                "The document has {found} layers; Umber supports at most {max}."
            ),
            Self::CanvasTooLarge { width, height } => write!(
                f,
                "The canvas is {width}×{height}, which is larger than Umber can open. \
                 Umber opens canvases up to {max} pixels on a side.",
                max = ImportedDocument::MAX_DIMENSION,
            ),
            Self::StackTooLarge {
                width,
                height,
                layers,
                bytes,
            } => write!(
                f,
                "This document has {layers} layers at {width}×{height}, which comes to \
                 {held} of pixels. Umber holds at most {max} of a document at once. \
                 Flattening or removing some layers will bring it within reach.",
                held = gigabytes(*bytes),
                max = gigabytes(ImportedDocument::MAX_TOTAL_BYTES),
            ),
            Self::Empty { format } => {
                write!(f, "The {} file contains no layers.", format.label())
            }
        }
    }
}

impl std::error::Error for ImportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Switch off whatever effects will not fit, and say how many.
///
/// **A file can be over the budget and Umber still has to open it.**
/// `effect::MAX_ENABLED` is 127 and a full stack of 64 layers carrying both
/// kinds asks for 128, so this is one bad document rather than an abstract
/// bound — and it is reachable by other routes too, since the cap governs
/// *adding* an effect and an undo, a paste and a layer leaving a folder can
/// each arrive over it (`docs/layer-effects.md` §6.1).
///
/// **Not at the install, and not only at the reader.** [`ImportedDocument::
/// open`] is where effects reach a [`LayerStack`], and `LayerStack::set_effect`
/// would refuse the excess all by itself — by returning `false`, into a caller
/// with nowhere to put a warning, which is precisely the
/// `None`-into-`.flatten()` silence CLAUDE.md's "Partial exhaustiveness"
/// section ends on. Whether a refusal is a diagnostic or a shrug is decided by
/// its *caller*.
///
/// So this is called from **both** ends, and each takes what it can use.
/// `openraster::read` has `warnings` in hand and turns the count into an
/// [`ImportWarning::EffectsOverBudget`]; `open` has nowhere to say anything and
/// calls it for the *guarantee* — that `set_effect` cannot then refuse for the
/// budget, whoever built the document. `ImportedLayer::effects` is a public
/// field on a public struct, so "only the ORA reader produces effects" is the
/// kind of thing a later change makes false in silence, which is the argument
/// `autosave::Reaper`'s containment already makes for itself.
///
/// **Calling it twice cannot double-count**, because it is idempotent by
/// construction: after it runs the enabled count is within the budget, so a
/// second pass disables nothing and returns zero. That is what lets the
/// guarantee and the diagnostic be the same function rather than two that have
/// to agree.
///
/// The order is bottom to top and, within a layer, composite order — the order
/// the stack itself is in, so it is stable across a save and a reopen rather
/// than depending on which layer happened to be read first.
fn disable_effects_over_budget(layers: &mut [ImportedLayer]) -> usize {
    let mut kept = 0usize;
    let mut disabled = 0usize;
    for layer in layers.iter_mut() {
        for e in &mut layer.effects {
            if !e.enabled {
                continue;
            }
            if effect::within_budget(kept + 1) {
                kept += 1;
            } else {
                e.enabled = false;
                disabled += 1;
            }
        }
    }
    disabled
}

/// What a reader knows about its stack before it decodes a pixel.
///
/// **Two counts that must not be interchangeable, which is why this is a type
/// and not two `usize` parameters.** [`check_bounds`] needs both — entries for
/// `LayerStack::MAX`, painted layers for the byte total — and with two bare
/// numbers in the signature every reader could pass its entry count twice.
/// Every reader did, which is how a folder came to be charged for a canvas it
/// does not hold.
///
/// Demonstrated rather than argued: with the two as parameters, putting
/// `nodes.len()` in both slots of the `.clip` reader left all 1,061 tests
/// green, because the guard that knew the difference was testing `check_bounds`
/// and could not see what its caller handed it. That is the "a guard on a model
/// is not a guard on the call site" failure CLAUDE.md records. The fix is both
/// halves: [`Self::of`] takes the folder *readings* and derives both counts
/// itself, so a caller has nothing to get the wrong way round, and
/// `a_document_filed_into_folders_is_not_charged_for_the_folders` drives the
/// reader so that writing it wrongly anyway fails the build.
#[derive(Clone, Copy, Debug)]
struct StackSize {
    /// Everything that will occupy a [`LayerStack`] entry, folders included.
    entries: usize,
    /// Everything holding a canvas-sized buffer. Folders are not in it.
    painted: usize,
}

impl StackSize {
    /// Read off one "is this entry a folder" per entry, which is the only
    /// reading that can tell the two counts apart.
    fn of(folders: impl IntoIterator<Item = bool>) -> Self {
        let mut entries = 0;
        let mut painted = 0;
        for folder in folders {
            entries += 1;
            painted += usize::from(!folder);
        }
        Self { entries, painted }
    }

    /// A reader whose every entry holds pixels.
    ///
    /// Named rather than spelled `of(repeat(false))` at the call site so that
    /// using it is a claim somebody can check: a flat picture is one layer, and
    /// the `.psd` reader makes no folders at all because a Photoshop group
    /// arrives as nothing. A reader that grows folders and keeps this call is
    /// visibly wrong, where a second `n` in an argument list was not.
    fn all_painted(n: usize) -> Self {
        Self {
            entries: n,
            painted: n,
        }
    }
}

/// Bytes as a figure an artist reads, for a refusal that has to be acted on.
///
/// Decimal GB rather than GiB, because the sentence is telling somebody how big
/// their picture is and not how a buffer was sized. One decimal place: the
/// difference between 6.0 and 6.4 GB is worth showing when the bound is 17.2,
/// and a second place is noise on a figure nobody can hit exactly.
fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

/// Reject canvases and stacks an import could never open, before decoding any
/// pixels.
///
/// Called by each reader as soon as it knows the header, which is the point of
/// it: decoding sixty 8000² layers and *then* refusing would allocate several
/// gigabytes to reach the same answer.
///
/// **The two bounds count different things**, which is what [`StackSize`] is
/// for. `LayerStack::MAX` bounds entries and a folder occupies one; the byte
/// total is buffers, and a folder holds none — `ImportedLayer::folder`
/// allocates nothing at all. One count served both, so the byte total charged
/// a folder for a canvas: a Clip Studio document filed into groups paid for its
/// own filing, and the deeper the artist's tidying the sooner the refusal came.
///
/// **Masks are deliberately not counted, and the bound is therefore what a
/// stack of layers costs rather than what a document costs.** A mask is another
/// canvas-sized slice, so a document whose every layer carries one reaches
/// roughly twice [`ImportedDocument::MAX_TOTAL_BYTES`]. Counting them looks
/// like the obvious tightening and is wrong in the one direction that matters:
/// this is the check an *Umber* document goes through on the way back in, so a
/// bound that counted masks would refuse to reopen large masked documents Umber
/// itself had written — the reader would be stricter than the writer, and the
/// artist's own file would be the casualty. The figure is a sanity bound
/// against a malformed header rather than a memory budget; the real limits are
/// the caller's `max_texture_dimension_2d` check and `LayerStack::MAX_SLOTS`,
/// which does account for a mask on every layer.
fn check_bounds(
    format: SourceFormat,
    width: u32,
    height: u32,
    stack: StackSize,
) -> Result<(), ImportError> {
    if width == 0 || height == 0 {
        return Err(ImportError::Malformed {
            format,
            detail: format!("the canvas is {width}×{height}"),
        });
    }
    if width > ImportedDocument::MAX_DIMENSION || height > ImportedDocument::MAX_DIMENSION {
        return Err(ImportError::CanvasTooLarge { width, height });
    }
    if stack.entries > LayerStack::MAX {
        return Err(ImportError::TooManyLayers {
            found: stack.entries,
            max: LayerStack::MAX,
        });
    }
    let total = width as u64 * height as u64 * 4 * stack.painted.max(1) as u64;
    if total > ImportedDocument::MAX_TOTAL_BYTES {
        return Err(ImportError::StackTooLarge {
            width,
            height,
            layers: stack.painted,
            bytes: total,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str, size: UVec2) -> ImportedLayer {
        ImportedLayer::new(
            name,
            BlendMode::Normal,
            vec![0; size.x as usize * size.y as usize * 4],
        )
    }

    #[test]
    fn the_stack_keeps_the_imported_order_and_names() {
        let size = UVec2::new(2, 2);
        let doc = ImportedDocument {
            format: SourceFormat::OpenRaster,
            size,
            layers: vec![
                layer("bottom", size),
                layer("middle", size),
                layer("top", size),
            ],
            active: None,
            background: Background::Transparent,
            dpi: None,
            history: None,
            warnings: vec![],
        };
        let Opened {
            document,
            stack,
            uploads,
            ..
        } = doc.open();

        assert_eq!(document.size, size);
        assert_eq!(stack.len(), 3);
        assert_eq!(stack.get(0).unwrap().name, "bottom");
        assert_eq!(stack.get(2).unwrap().name, "top");
        assert_eq!(stack.active_index(), 2, "the top layer should be selected");

        // Slots are the renderer's contract: every upload must name the slot of
        // the layer at the same stack position.
        for (i, upload) in uploads.iter().enumerate() {
            assert_eq!(Some(upload.slot), stack.get(i).unwrap().slot());
        }
    }

    #[test]
    fn a_remembered_selection_is_honoured_and_a_nonsensical_one_is_not() {
        let size = UVec2::new(1, 1);
        let build = |active| ImportedDocument {
            format: SourceFormat::OpenRaster,
            size,
            layers: vec![layer("a", size), layer("b", size), layer("c", size)],
            active,
            background: Background::Transparent,
            dpi: None,
            history: None,
            warnings: vec![],
        };

        assert_eq!(build(Some(0)).open().stack.active_index(), 0);
        // Out of range means the file disagrees with itself — a layer that
        // failed to load, say. The top layer is the safe answer, not a panic.
        assert_eq!(build(Some(9)).open().stack.active_index(), 2);
        assert_eq!(build(None).open().stack.active_index(), 2);
    }

    #[test]
    fn a_single_layer_import_does_not_add_a_spare() {
        // LayerStack::new() already has one layer; adding one per import layer
        // would leave an empty layer at the bottom of every import.
        let size = UVec2::new(1, 1);
        let doc = ImportedDocument {
            format: SourceFormat::Png,
            size,
            layers: vec![layer("only", size)],
            active: None,
            background: Background::Transparent,
            dpi: None,
            history: None,
            warnings: vec![],
        };
        let Opened { stack, uploads, .. } = doc.open();
        assert_eq!(stack.len(), 1);
        assert_eq!(uploads.len(), 1);
    }

    #[test]
    fn bounds_are_checked_before_any_decoding() {
        let f = SourceFormat::Photoshop;
        assert!(matches!(
            check_bounds(f, 0, 10, StackSize::all_painted(1)),
            Err(ImportError::Malformed { .. })
        ));
        assert!(matches!(
            check_bounds(f, 40000, 10, StackSize::all_painted(1)),
            Err(ImportError::CanvasTooLarge { .. })
        ));
        assert!(matches!(
            check_bounds(f, 100, 100, StackSize::all_painted(LayerStack::MAX + 1)),
            Err(ImportError::TooManyLayers { .. })
        ));
        // 16384² is a legal canvas and 64 of them is 68.7 GB, which is the
        // document the byte bound exists for: a header asking for tens of
        // gigabytes before a pixel has been decoded.
        assert!(matches!(
            check_bounds(f, 16384, 16384, StackSize::all_painted(64)),
            Err(ImportError::StackTooLarge { .. })
        ));
        assert!(check_bounds(f, 2048, 2048, StackSize::all_painted(8)).is_ok());
    }

    /// The refusal the artist actually met, and the two things wrong with it.
    ///
    /// A 15000×5000 Clip Studio document was refused with "the canvas is larger
    /// than Umber can open" — a canvas 1384 px inside `MAX_DIMENSION` on its
    /// long edge, so the sentence named a bound the file was nowhere near and
    /// the artist had nothing to act on.
    #[test]
    fn a_canvas_within_bounds_is_never_refused_for_its_size() {
        let f = SourceFormat::ClipStudio;
        // Every edge Umber will open, at the full stack. None of these may ever
        // come back as `CanvasTooLarge`: the canvas is legal in all of them.
        for (w, h) in [(15000, 5000), (5000, 5000), (16384, 16384), (10000, 10000)] {
            for layers in [1, 8, 22, LayerStack::MAX] {
                assert!(
                    !matches!(
                        check_bounds(f, w, h, StackSize::all_painted(layers)),
                        Err(ImportError::CanvasTooLarge { .. })
                    ),
                    "{w}×{h} at {layers} layers was refused for its canvas size"
                );
            }
        }
    }

    /// The figure that was wrong, driven at the sizes it was wrong at.
    ///
    /// At the old 2 GiB, 15000×5000 refused its eighth layer and 5000² its
    /// twenty-second. Both are ordinary illustration documents.
    #[test]
    fn an_ordinary_large_document_opens() {
        let f = SourceFormat::ClipStudio;
        assert!(check_bounds(f, 15000, 5000, StackSize::all_painted(8)).is_ok());
        assert!(check_bounds(f, 15000, 5000, StackSize::all_painted(50)).is_ok());
        assert!(check_bounds(f, 5000, 5000, StackSize::all_painted(22)).is_ok());
        assert!(check_bounds(f, 5000, 5000, StackSize::all_painted(LayerStack::MAX)).is_ok());
        assert!(check_bounds(f, 10000, 10000, StackSize::all_painted(40)).is_ok());
    }

    /// **A reader must never be stricter than the writer** is the rule the mask
    /// paragraph on [`check_bounds`] states, and `MAX_TOTAL_BYTES` does not
    /// fully meet it. This says exactly how far it gets, because a guard
    /// written to the rule rather than to the code would be asserting something
    /// that does not ship — and one quietly narrowed until it passed would be
    /// the bound loosened to admit the thing it exists to catch.
    ///
    /// Where it holds: every canvas up to 8192², at the full stack. That covers
    /// what Umber's own dialog offers and every document anybody has brought
    /// here.
    ///
    /// Where it does not, and it is a **known gap** rather than an oversight:
    /// a full 64-layer stack needs 16 GiB from 8192² upwards, so a 10000²
    /// document at 40 layers opens and the same canvas at 64 does not. The
    /// argument for stopping somewhere is on `MAX_TOTAL_BYTES` — every figure
    /// that admits a real 25.6 GB document also admits a malformed header
    /// asking for 25.6 GB, because the two are the same header. Raising it to
    /// `MAX_DIMENSION² × 4 × LayerStack::MAX` closes the gap and retires the
    /// check; that is a product decision, and this test is where its terms are
    /// written down so it can be taken deliberately.
    #[test]
    fn a_document_umber_could_save_can_be_reopened() {
        let f = SourceFormat::OpenRaster;
        for edge in [2048u32, 4096, 8192] {
            assert!(
                check_bounds(f, edge, edge, StackSize::all_painted(LayerStack::MAX)).is_ok(),
                "a full stack on a {edge}² canvas could be saved and not reopened"
            );
        }

        // The gap, pinned so that closing it is a change to this test and not a
        // silent one. Whichever way it moves, both halves move together.
        assert!(
            check_bounds(f, 10000, 10000, StackSize::all_painted(64)).is_err(),
            "if this now opens, MAX_TOTAL_BYTES was raised and its docs must say so"
        );
        assert!(check_bounds(f, 10000, 10000, StackSize::all_painted(40)).is_ok());
    }

    /// A folder holds no slot and no buffer, so it may not be charged a canvas.
    ///
    /// Driven through [`StackSize::of`] from folder *readings* rather than from
    /// two hand-written counts, because that constructor is what every reader
    /// calls and hand-written counts would only agree with themselves. The case
    /// is one where the two readings disagree loudly: 60 folders over 4 painted
    /// layers on a 10000² canvas is 9.6 GB counted properly and 25.6 GB counted
    /// off the entries, so a version that charged folders fails here.
    ///
    /// This is the *function's* half. Whether a reader hands it the right
    /// reading is `a_document_filed_into_folders_is_not_charged_for_the_folders`
    /// in `clipstudio`, and it has to be there: this test passes whatever the
    /// caller does.
    #[test]
    fn folders_are_not_charged_for_pixels_they_do_not_hold() {
        let f = SourceFormat::ClipStudio;
        let stack = StackSize::of((0..64).map(|i| i >= 4));
        assert_eq!((stack.entries, stack.painted), (64, 4));
        assert!(check_bounds(f, 10000, 10000, stack).is_ok());

        // And the entry bound still counts them, because a folder does occupy a
        // stack entry even though it occupies no memory. One painted layer at
        // the bottom of `MAX + 1` entries costs no more in bytes than a single
        // layer and is still a stack too tall to hold.
        let too_tall = StackSize::of((0..=LayerStack::MAX).map(|i| i > 0));
        assert!(matches!(
            check_bounds(f, 100, 100, too_tall),
            Err(ImportError::TooManyLayers { .. })
        ));
    }

    /// A refusal has to send somebody somewhere useful, so the sentence names
    /// the stack rather than the canvas and carries the figure to act on.
    #[test]
    fn a_stack_refusal_names_the_stack_and_not_the_canvas() {
        let f = SourceFormat::ClipStudio;
        let err = check_bounds(f, 16384, 16384, StackSize::all_painted(64)).unwrap_err();
        let said = err.to_string();
        assert!(said.contains("64 layers"), "{said}");
        assert!(said.contains("68.7 GB"), "{said}");
        assert!(
            said.contains("17.2 GB"),
            "the sentence must say what the bound is: {said}"
        );
        assert!(
            !said.contains("larger than Umber can open"),
            "a stack refusal must not wear the canvas refusal's words: {said}"
        );
    }

    #[test]
    fn an_unknown_extension_is_refused_by_name() {
        let err = import(Path::new("drawing.mdp")).unwrap_err();
        assert!(
            matches!(&err, ImportError::UnsupportedExtension(e) if e == "mdp"),
            "got {err:?}"
        );
        // The extension is checked before the file is opened, so the message
        // does not depend on the file existing.
        assert_eq!(err.to_string(), "Umber cannot open .mdp files.");
    }

    #[test]
    fn a_file_on_disk_imports_end_to_end() {
        // The one test that exercises the whole public entry point: extension
        // dispatch, reading, decoding, and building the engine state the UI
        // will be handed.
        // Named by process id: a fixed name is the same file in every
        // checkout, and concurrent worktrees then write over each other.
        let path = std::env::temp_dir().join(format!(
            "umber-docimport-end-to-end-{}.ora",
            std::process::id()
        ));
        std::fs::write(
            &path,
            fixtures::ora(
                2,
                2,
                &[
                    fixtures::OraLayer::new("Ink", 2, 2, &[0, 0, 0, 255]),
                    fixtures::OraLayer::new("Paper", 2, 2, &[255, 255, 255, 255]),
                ],
            ),
        )
        .unwrap();

        let doc = import(&path).unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(doc.format, SourceFormat::OpenRaster);
        let Opened {
            document,
            stack,
            uploads,
            ..
        } = doc.open();
        assert_eq!(document.size, UVec2::new(2, 2));
        assert_eq!(stack.layers()[0].name, "Paper");
        assert_eq!(uploads.len(), 2);
        assert_eq!(
            uploads[0].pixels.len() as u64,
            document.layer_bytes(),
            "an upload must be exactly one layer's worth of bytes"
        );
    }

    #[test]
    fn more_layers_than_the_stack_holds_is_refused_not_truncated() {
        let size = UVec2::new(1, 1);
        let doc = ImportedDocument {
            format: SourceFormat::Photoshop,
            size,
            layers: (0..LayerStack::MAX + 1)
                .map(|i| layer(&format!("L{i}"), size))
                .collect(),
            active: None,
            background: Background::Transparent,
            dpi: None,
            history: None,
            warnings: vec![],
        };
        assert!(matches!(
            doc.validate(),
            Err(ImportError::TooManyLayers { .. })
        ));
    }

    /// **A document over the effect budget opens, and says what it switched
    /// off.**
    ///
    /// It is one document over, not a theoretical bound:
    /// `effect::MAX_ENABLED` is 127 and 64 layers carrying both kinds ask for
    /// 128. Built at exactly that shape rather than at some round number, so
    /// the test would notice if either figure moved without the other.
    ///
    /// Two things are checked and the second is the one worth having. The
    /// excess is **disabled rather than removed**, so every parameter is still
    /// on the layer, still written at the next save, and can be switched back
    /// on the moment something else is switched off. Deleting them would be
    /// the silent loss — the document would open, look nearly right, and be
    /// saved back a shadow short.
    #[test]
    fn a_document_over_the_effect_budget_opens_with_the_excess_switched_off() {
        use crate::effect::{Effect, EffectKind};

        let size = UVec2::new(1, 1);
        let mut layers: Vec<ImportedLayer> = (0..LayerStack::MAX)
            .map(|i| {
                let mut l = layer(&format!("L{i}"), size);
                l.effects = vec![Effect::drop_shadow(), Effect::outline()];
                l
            })
            .collect();
        let asked: usize = layers.iter().map(|l| l.effects.len()).sum();
        assert_eq!(asked, effect::MAX_ENABLED + 1, "the fixture is one over");

        assert_eq!(disable_effects_over_budget(&mut layers), 1);

        // Idempotent, which is what lets `open` call it again for the
        // guarantee without double-counting — see the function's docs.
        assert_eq!(
            disable_effects_over_budget(&mut layers),
            0,
            "a second pass must find nothing left to disable"
        );

        // Nothing was thrown away: every layer still holds both, and exactly
        // one of them is off.
        assert!(layers.iter().all(|l| l.effects.len() == 2));
        let enabled: usize = layers
            .iter()
            .map(|l| l.effects.iter().filter(|e| e.enabled).count())
            .sum();
        assert_eq!(enabled, effect::MAX_ENABLED);

        // Bottom to top and in composite order, so it is the *last* effect of
        // the *top* layer that gives way — the same answer on every reopen
        // rather than one that depends on which layer was read first.
        let top = layers.last().expect("a layer");
        assert_eq!(top.effects[0].kind, EffectKind::DropShadow);
        assert!(top.effects[0].enabled);
        assert_eq!(top.effects[1].kind, EffectKind::Outline);
        assert!(!top.effects[1].enabled, "the last one is what gave way");

        // And the whole set installs into a stack without `set_effect`
        // refusing any of it, which is what `open`'s `debug_assert` claims.
        let doc = ImportedDocument {
            format: SourceFormat::OpenRaster,
            size,
            layers,
            active: None,
            background: Background::Transparent,
            dpi: None,
            history: None,
            warnings: Vec::new(),
        };
        let opened = doc.open();
        assert_eq!(
            opened.stack.enabled_effect_count(),
            effect::MAX_ENABLED,
            "the trimmed set must be exactly what the stack accepts"
        );
        assert!(
            opened.stack.layers().iter().all(|l| l.effects().len() == 2),
            "a disabled effect still lives on its layer"
        );
    }

    /// A document inside the budget has nothing switched off and says nothing.
    #[test]
    fn a_document_within_the_effect_budget_is_left_alone() {
        let size = UVec2::new(1, 1);
        let mut only = layer("Ink", size);
        only.effects = vec![crate::effect::Effect::outline()];
        let mut layers = vec![only];

        assert_eq!(disable_effects_over_budget(&mut layers), 0);
        assert!(layers[0].effects[0].enabled);
    }

    /// **`open` trims for itself, so the guarantee does not depend on which
    /// reader produced the document.**
    ///
    /// `openraster::read` is the only caller that raises the warning, and
    /// `ImportedLayer::effects` is a public field on a public struct — so a
    /// second importer, or a caller building an `ImportedDocument` by hand, can
    /// reach `open` over budget without ever passing through that reader.
    /// Without the trim inside `open`, `LayerStack::set_effect` would refuse
    /// the excess by returning `false` into a loop with nowhere to report it:
    /// the silence CLAUDE.md's "Partial exhaustiveness" section ends on, one
    /// level up from where this module already avoided it.
    ///
    /// Deliberately built by hand rather than read out of an archive, because
    /// going through the reader is exactly the path this is *not* testing.
    #[test]
    fn an_over_budget_document_is_trimmed_by_open_even_with_no_reader_between() {
        use crate::effect::Effect;

        let size = UVec2::new(1, 1);
        let layers: Vec<ImportedLayer> = (0..LayerStack::MAX)
            .map(|i| {
                let mut l = layer(&format!("L{i}"), size);
                l.effects = vec![Effect::drop_shadow(), Effect::outline()];
                l
            })
            .collect();
        let asked: usize = layers.iter().map(|l| l.effects.len()).sum();
        assert_eq!(asked, effect::MAX_ENABLED + 1, "the fixture is one over");

        let opened = ImportedDocument {
            format: SourceFormat::Krita,
            size,
            layers,
            active: None,
            background: Background::Transparent,
            dpi: None,
            history: None,
            warnings: Vec::new(),
        }
        .open();

        assert_eq!(opened.stack.enabled_effect_count(), effect::MAX_ENABLED);
        assert!(
            opened.stack.layers().iter().all(|l| l.effects().len() == 2),
            "trimming switches an effect off, it does not remove one"
        );
    }

    /// A **disabled** effect is not charged against the budget, so a document
    /// full of them is not over it.
    ///
    /// That is `Effect::enabled`'s stated meaning — no draw, no bake, nothing
    /// charged — and counting the effects rather than the enabled ones would
    /// switch off effects that were already off and report a loss that did not
    /// happen.
    #[test]
    fn a_disabled_effect_costs_the_budget_nothing_on_the_way_in() {
        use crate::effect::Effect;

        let size = UVec2::new(1, 1);
        let mut layers: Vec<ImportedLayer> = (0..LayerStack::MAX)
            .map(|i| {
                let mut l = layer(&format!("L{i}"), size);
                l.effects = vec![
                    Effect::drop_shadow(),
                    Effect {
                        enabled: false,
                        ..Effect::outline()
                    },
                ];
                l
            })
            .collect();

        assert_eq!(disable_effects_over_budget(&mut layers), 0);
        assert!(layers.iter().all(|l| l.effects[0].enabled));
    }

    #[test]
    fn every_supported_extension_dispatches() {
        // A format added to the list but not to `import` would fail here
        // rather than in the file dialog.
        for ext in supported_extensions() {
            let path = std::path::PathBuf::from(format!("no-such-file.{ext}"));
            let err = import(&path).unwrap_err();
            assert!(
                matches!(err, ImportError::Io(_)),
                ".{ext} did not reach a reader: {err:?}"
            );
        }
    }
}
