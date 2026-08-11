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
//! [`ImportedLayer::pixels`] is a sequence of [`PixelPiece`]s: rectangles of
//! RGBA8 in exactly the form a layer texture holds — sRGB-encoded, alpha
//! premultiplied in linear space. Each one can go straight to
//! `queue.write_texture`. See [`srgb`] for why that is the right form and what
//! the wrong ones look like, and [`PixelPiece`] for the three rules a piece
//! sequence lives by.
//!
//! **It used to be one canvas-sized buffer per layer, and every format Umber
//! reads stores layers sparsely.** A `.clip` keeps 256-square blocks and stores
//! only the ones the artist touched, a `.kra` keeps 64-square tiles, an `.ora`
//! keeps one PNG at its own offset, and a `.psd` keeps per-layer rectangles. All
//! four were densified on the way in, so a 54-layer 20000×5000 document that
//! holds 1.4 GB of paint was materialised as 21.6 GB of host buffers and then
//! refused for being too big. Measured over 33 real documents by
//! `examples/survey-residency.rs`: **13.5% of a dense store actually holds
//! paint.**

mod blend;
mod clipstudio;
mod container;
/// The flattened picture a document already carries, for a file manager.
pub mod preview;
/// How much of each layer a real document actually holds, without decoding it.
pub mod residency;
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
use crate::geom::PixelRect;
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

/// One rectangle of a layer's pixels, in canvas coordinates.
///
/// `bytes` is `rect.area() * 4`, tightly packed RGBA8, sRGB-encoded with alpha
/// premultiplied in linear space — the form [`ImportedLayer::pixels`] has always
/// been in, over a smaller rectangle. It goes straight to `write_layer_rect`,
/// which is byte for byte the shape `app.rs::swap_patch` already uses for an
/// undo patch's pieces.
///
/// # The three rules
///
/// 1. **Every piece lies inside the canvas.** Readers clip; a layer that hangs
///    off the page contributes the part that is on it and nothing else.
/// 2. **Pieces do not overlap.** A block or tile grid gives this for nothing,
///    and a reader that yields one piece is trivially inside it. It is not
///    *asserted* over a foreign file — a duplicate tile in somebody else's
///    malformed archive would then panic a debug build, where the writes are
///    deterministic and the last one wins exactly as a dense blit's did — so it
///    is driven per reader in tests instead. See [`overlapping_pieces`].
/// 3. **A pixel covered by no piece is the slot's empty value.** Today that is
///    transparent black for every slot, because `Graphics::install_canvas`
///    clears the whole array before the upload loop and nothing distinguishes a
///    mask slice from a layer's. **So a mask may not yet be sparse**, and no
///    reader makes one so: a mask's empty value is *white*, and
///    `srgb::mask_pixel(0)` is `[0, 0, 0, 255]` rather than four zeroes, so
///    even a fully hidden region is not what the clear leaves. When the tiled
///    store gives a slot class its own empty value this rule is what a mask
///    reader will be able to lean on; until then the rule it is actually held to
///    is the narrower "a pixel covered by no piece is transparent black".
///
/// There is deliberately **no completion signal**. The clear before the loop is
/// what makes an unwritten region already the empty value, so a reader has
/// nothing to say about the pixels it did not send.
#[derive(Clone)]
pub struct PixelPiece {
    pub rect: PixelRect,
    pub bytes: Vec<u8>,
}

impl PixelPiece {
    /// A piece over `rect`.
    ///
    /// # Panics
    ///
    /// Debug-asserts that `bytes` is exactly the rectangle's worth of RGBA8. A
    /// reader that miscounts produces a picture skewed by a row, which is the
    /// kind of thing that is obvious in a test and mystifying in a document.
    pub fn new(rect: PixelRect, bytes: Vec<u8>) -> Self {
        debug_assert_eq!(
            bytes.len() as u64,
            rect.area() * 4,
            "a piece's bytes must be exactly its own rectangle"
        );
        Self { rect, bytes }
    }

    /// One piece covering the whole canvas — what a reader that cannot do
    /// better yields, and what every mask reader yields today.
    ///
    /// Named rather than spelled out at the call site so that using it is a
    /// claim somebody can check: `.psd` takes it because the `psd` crate hands
    /// back an already-canvas-sized buffer and there is nothing better to be
    /// had, and a `.png` takes it because a flat picture *is* the canvas.
    pub fn whole(canvas: UVec2, bytes: Vec<u8>) -> Self {
        Self::new(
            PixelRect {
                x: 0,
                y: 0,
                width: canvas.x,
                height: canvas.y,
            },
            bytes,
        )
    }

    /// What this piece costs in host memory.
    fn byte_len(&self) -> u64 {
        self.bytes.len() as u64
    }
}

impl fmt::Debug for PixelPiece {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PixelPiece")
            .field("rect", &self.rect)
            .field("bytes", &format_args!("{} bytes", self.bytes.len()))
            .finish()
    }
}

/// Lay a piece sequence out over a canvas-sized buffer, zero where no piece
/// reaches.
///
/// **For tests and for nothing else.** It is the dense buffer the readers used
/// to build, which is exactly what the piece contract exists to stop anybody
/// building — so it is `pub(crate)`, and the one thing it is good for is
/// comparing a reader's pieces against the bytes that reader used to produce.
#[cfg(test)]
pub(crate) fn assemble(pieces: &[PixelPiece], canvas: UVec2) -> Vec<u8> {
    let stride = canvas.x as usize * 4;
    let mut out = vec![0u8; stride * canvas.y as usize];
    for piece in pieces {
        for row in 0..piece.rect.height as usize {
            let src = row * piece.rect.width as usize * 4;
            let dst = (piece.rect.y as usize + row) * stride + piece.rect.x as usize * 4;
            let len = piece.rect.width as usize * 4;
            out[dst..dst + len].copy_from_slice(&piece.bytes[src..src + len]);
        }
    }
    out
}

/// The first pair of pieces that overlap, if any.
///
/// A sweep rather than the `n²` comparison, because a 20000×5000 layer cut into
/// 256-squares is 1,580 pieces and a real stack is fifty of those: sorted by
/// row, only pieces whose rows actually meet are ever compared.
///
/// Rule 2's guard, and it lives here rather than inside
/// [`ImportedDocument::validate`] for the reason stated at [`PixelPiece`] — a
/// malformed foreign file must not panic a debug build over a rule whose
/// consequence today is nil.
#[cfg(test)]
pub(crate) fn overlapping_pieces(pieces: &[PixelPiece]) -> Option<(usize, usize)> {
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    order.sort_by_key(|&i| (pieces[i].rect.y, pieces[i].rect.x));
    let mut active: Vec<usize> = Vec::new();
    for &i in &order {
        let r = pieces[i].rect;
        active.retain(|&j| {
            let a = pieces[j].rect;
            u64::from(a.y) + u64::from(a.height) > u64::from(r.y)
        });
        for &j in &active {
            let a = pieces[j].rect;
            let rows = u64::from(a.y) < u64::from(r.y) + u64::from(r.height)
                && u64::from(r.y) < u64::from(a.y) + u64::from(a.height);
            let cols = u64::from(a.x) < u64::from(r.x) + u64::from(r.width)
                && u64::from(r.x) < u64::from(a.x) + u64::from(a.width);
            if rows && cols {
                return Some((j, i));
            }
        }
        active.push(i);
    }
    None
}

/// Drive [`PixelPiece`]'s rules 1 and 2 over one reader's output.
///
/// **For a reader's own tests**, because that is where the rules can be
/// enforced: `validate` cannot assert rule 2 over a foreign file without turning
/// a malformed archive into a panic, and rule 1 read off a fixture is a claim
/// about the reader rather than about the type. Every reader that yields more
/// than one piece calls this on a layer it produced.
#[cfg(test)]
pub(crate) fn check_piece_rules(pieces: &[PixelPiece], canvas: UVec2) {
    for piece in pieces {
        assert!(
            u64::from(piece.rect.x) + u64::from(piece.rect.width) <= u64::from(canvas.x)
                && u64::from(piece.rect.y) + u64::from(piece.rect.height) <= u64::from(canvas.y),
            "rule 1: {:?} reaches outside a {canvas:?} canvas",
            piece.rect
        );
        assert!(
            piece.rect.width > 0 && piece.rect.height > 0,
            "an empty piece is a write of nothing: {:?}",
            piece.rect
        );
        assert_eq!(
            piece.bytes.len() as u64,
            piece.rect.area() * 4,
            "a piece's bytes must be exactly its own rectangle: {:?}",
            piece.rect
        );
    }
    if let Some((a, b)) = overlapping_pieces(pieces) {
        panic!(
            "rule 2: {:?} and {:?} overlap",
            pieces[a].rect, pieces[b].rect
        );
    }
}

/// One imported layer, as the rectangles of it the file actually holds.
///
/// Source formats store layers as sub-rectangles with an offset; that offset is
/// resolved during import, because Umber's layers are all canvas-sized slices of
/// one texture array — but the *pixels* are not densified any more. See
/// [`PixelPiece`].
#[derive(Clone)]
pub struct ImportedLayer {
    pub name: String,
    pub visible: bool,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub blend: BlendMode,
    /// The rectangles of this layer the file holds, sRGB-encoded with
    /// premultiplied alpha — see [`PixelPiece`] and the module docs. Empty for a
    /// folder, and legitimately empty for a layer nobody painted on.
    pub pixels: Vec<PixelPiece>,
    /// The layer's mask, in the same form as `pixels` — so it goes straight to
    /// `write_texture` like everything else here.
    ///
    /// Only the **red** channel is ever read, and it holds coverage as a
    /// **linear** multiplier on the layer's alpha — the form every source format
    /// already states one in, so `srgb::mask_buffer` widens rather than
    /// converts. It used to hold the sRGB encoding of that, which cost 73 of its
    /// 256 states; see that module.
    ///
    /// **Every reader yields exactly one canvas-sized piece here**, and
    /// [`PixelPiece`]'s rule 3 says why: a mask's empty value is white, the
    /// upload's clear delivers transparent black, and `mask_pixel(0)` is not
    /// four zeroes either. A sparse mask waits for a store that gives a slot
    /// class its own empty value.
    ///
    /// Filled by ORA, by `.kra`'s transparency masks and by `.clip`'s layer
    /// masks. A `.psd` mask is still reported as lost, and that is the `psd`
    /// crate's limit rather than a decision: see `photoshop`'s module docs.
    pub mask: Option<Vec<PixelPiece>>,
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
    pub fn new(name: impl Into<String>, blend: BlendMode, pixels: Vec<PixelPiece>) -> Self {
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

    /// What this layer's pixels cost in host memory.
    ///
    /// The **pieces**, which is the whole point: it is what the file actually
    /// holds rather than `canvas × 4`, and it is the figure
    /// [`ImportedDocument::MAX_TOTAL_BYTES`] is now compared against. A mask is
    /// deliberately not in it — see [`check_resident`].
    pub fn pixel_bytes(&self) -> u64 {
        self.pixels.iter().map(PixelPiece::byte_len).sum()
    }

    /// The canvas this layer's pieces lay out over, for a test that wants to
    /// look at a pixel.
    ///
    /// **Tests only**, and not a convenience the readers may reach for: it
    /// rebuilds the dense buffer the piece contract exists to stop anybody
    /// building. What it is good for is holding a reader's pieces against the
    /// bytes that reader used to produce, which is how every "this is still
    /// pixel-identical" guard here is written.
    #[cfg(test)]
    pub(crate) fn dense(&self, canvas: UVec2) -> Vec<u8> {
        assemble(&self.pixels, canvas)
    }

    /// The same for the mask, or `None` where there is not one.
    #[cfg(test)]
    pub(crate) fn dense_mask(&self, canvas: UVec2) -> Option<Vec<u8>> {
        self.mask.as_ref().map(|m| assemble(m, canvas))
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
            .field(
                "pixels",
                &format_args!(
                    "{} piece(s), {} bytes",
                    self.pixels.len(),
                    self.pixel_bytes()
                ),
            )
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
///
/// The pieces in the order the reader found them, which the caller writes one
/// at a time. Whatever they do not cover is left as the clear that preceded
/// them — see [`PixelPiece`]'s rule 3.
pub struct LayerUpload {
    pub slot: u32,
    pub pieces: Vec<PixelPiece>,
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
                pieces: layer.pixels,
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
                    pieces: mask,
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
    /// bound. The caller must still check the real `max_texture_dimension_2d`
    /// before uploading, and does.
    ///
    /// **It tracks [`Document::MAX_EDGE`] and must**, which is the rule
    /// [`check_bounds`] states as "a reader must never be stricter than the
    /// writer": Umber can make a canvas that size, so it has to be able to open
    /// one back. They are two constants rather than one because the two crates
    /// answer different questions — what a document may be, and what a *file*
    /// may declare — but a divergence between them is a document Umber saves
    /// and refuses.
    ///
    /// [`Document::MAX_EDGE`]: crate::document::Document::MAX_EDGE
    pub const MAX_DIMENSION: u32 = crate::document::Document::MAX_EDGE;

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
    /// **What it is compared against changed, and the figure did not.** It used
    /// to be `canvas × 4 × painted` off the header — a claim — and it is now
    /// the bytes the layers' [`PixelPiece`]s actually hold. The old comparison
    /// charged a layer a whole canvas whether the artist had painted a stroke
    /// on it or covered it, so the 124 MB document that provoked
    /// `docs/perf/` was refused at 21.6 GB while holding about 1.4 GB of paint.
    ///
    /// **That retires the "not closable by tuning" argument this used to
    /// carry**, which was: every figure admitting a real 25.6 GB document also
    /// admits a malformed header asking for 25.6 GB, *because they are the same
    /// header*. The premise was "a layer's buffer is allocated canvas-sized
    /// whatever the source data weighs", and that is what stopped being true. A
    /// hostile header claiming a huge canvas now yields no pieces and costs
    /// nothing; a real document costs what its content costs. See
    /// `docs/perf/import-and-limits.md` §4.3.
    ///
    /// **It is still finite, and 16 GiB is still the figure**, because the
    /// bound now does a different job: it stops the *accumulation*. A file can
    /// genuinely hold tens of gigabytes of paint, and a reader charging as it
    /// decodes is what turns that into a sentence instead of the process being
    /// killed. Past it the artist is told the figure and given the two levers —
    /// the stack and the canvas; see [`ImportError::StackTooLarge`].
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
            // This one *has* a document, so its warnings are to hand and the
            // refusal can carry them — the case the field exists for, reached
            // from the one site that could always have said why.
            return Err(ImportError::Empty {
                format: self.format,
                because: ImportError::reasons_from(&self.warnings),
            });
        }
        if self.layers.len() > LayerStack::MAX {
            return Err(ImportError::TooManyLayers {
                found: self.layers.len(),
                max: LayerStack::MAX,
            });
        }
        // **The resident total, and it is here as well as in every reader.**
        // The readers charge as they decode, which is what bounds the *spend* —
        // a hostile header claiming sixty-four 16384² layers must not be
        // decoded sixteen layers deep before anybody objects. This is the
        // whole-document figure, checked once, so a reader whose own accounting
        // is wrong still cannot hand back a document past the bound.
        //
        // **It is not the guarantee `disable_effects_over_budget` is**, and the
        // two look alike enough that the difference has to be said. That one is
        // called from inside `open`, which is what makes it total; this one is
        // private and `open` does not call it, because `open` answers with an
        // `Opened` and has nowhere to put a refusal. So the bound belongs to
        // the `import` entry points — `import_reporting` and
        // `read_openraster` — and a caller assembling an `ImportedDocument` by
        // hand and calling `open` on it is checked by nothing. Nothing in the
        // workspace does that, and saying so is better than a sentence that
        // reads as cover.
        //
        // Folders contribute nothing because a folder holds no pieces, which is
        // the entries-versus-buffers distinction `StackSize` used to have to
        // keep by hand. There is nothing left to get the wrong way round.
        check_resident(
            self.size.x,
            self.size.y,
            self.layers.iter().filter(|l| !l.folder).count(),
            self.layers.iter().map(ImportedLayer::pixel_bytes).sum(),
            Self::MAX_TOTAL_BYTES,
        )?;
        // Rules 1 and 3 of the piece contract, per piece. Rule 2 — that pieces
        // do not overlap — is deliberately not asserted over a foreign file;
        // `overlapping_pieces` says why, and the readers' own tests drive it.
        //
        // A folder is skipped for the reason it always was: it holds no pixels,
        // so there is nothing to be true of. But it must hold *no* pieces, and
        // that is checked, because a folder takes no slot and a piece on one
        // would be a rectangle with nowhere to be written.
        for layer in &self.layers {
            debug_assert!(
                !layer.folder || (layer.pixels.is_empty() && layer.mask.is_none()),
                "reader put pixels on a folder, which holds no slot to write them to"
            );
            // **A mask is exactly one piece and it covers the canvas.** The
            // field's type permits a sequence and `PixelPiece`'s rule 3 forbids
            // using it — a rule held by prose, which is a rule that will
            // eventually be broken. Held here instead: a reader that made a
            // mask sparse would otherwise compile, pass every rule below, and
            // blank the covered layers wherever no piece reached, because the
            // clear leaves transparent black and a mask reads that as *fully
            // hidden*. That is the failure `import-and-limits.md` §7.2 names.
            debug_assert!(
                layer.mask.as_ref().is_none_or(|m| {
                    m.len() == 1
                        && m[0].rect
                            == PixelRect {
                                x: 0,
                                y: 0,
                                width: self.size.x,
                                height: self.size.y,
                            }
                }),
                "a mask must be one piece covering the canvas until a slot class \
                 carries its own empty value"
            );
            for piece in layer.pixels.iter().chain(layer.mask.iter().flatten()) {
                debug_assert!(
                    u64::from(piece.rect.x) + u64::from(piece.rect.width) <= u64::from(self.size.x)
                        && u64::from(piece.rect.y) + u64::from(piece.rect.height)
                            <= u64::from(self.size.y),
                    "reader produced a piece that reaches outside the canvas"
                );
                debug_assert_eq!(
                    piece.bytes.len() as u64,
                    piece.rect.area() * 4,
                    "reader produced a piece that is not its own rectangle"
                );
            }
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
    /// One *picture inside* the file declares an edge past
    /// [`ImportedDocument::MAX_DIMENSION`] — see [`check_image_size`].
    ///
    /// **Separate from [`ImportError::CanvasTooLarge`] for that variant's own
    /// reason.** A layer's PNG, a `mergedimage.png` and a `.psd`'s composite are
    /// not the canvas: an ORA layer is stored at its own bounding box and may
    /// legitimately be larger than the page it sits on, so a file whose canvas
    /// is 2000 × 2000 can carry an image claiming 60000 × 60000. Reporting that
    /// as the canvas would send the artist to shrink a canvas that is nowhere
    /// near the bound, which is the failure `CanvasTooLarge`'s own docs record.
    ///
    /// It is stated against the same figure, because the same reasoning applies:
    /// nothing Umber can *write* holds a picture wider than the widest canvas it
    /// will make, so this can never refuse a document Umber produced.
    ImageTooLarge {
        width: u32,
        height: u32,
    },
    /// The canvas is fine and the *stack* is not: the pixels its layers
    /// actually hold are past [`ImportedDocument::MAX_TOTAL_BYTES`].
    ///
    /// **Separate from `CanvasTooLarge`, and that is the whole point of it.**
    /// Both refusals used to be that one variant, so a 15000×5000 Clip Studio
    /// document — an edge well inside `MAX_DIMENSION`, refused for its twenty
    /// layers — told the artist their canvas was larger than Umber can open.
    /// It is not, and no amount of shrinking it would have helped: what they
    /// had to do was reduce the *stack*, which the sentence never mentioned.
    /// A refusal that names the wrong bound is worse than a vague one, because
    /// it sends somebody to fix the thing that is not broken.
    ///
    /// **`bytes` is what the file holds, not `canvas × 4 × layers`.** It used to
    /// be the second, which charged every layer a whole canvas whatever the
    /// artist had painted on it — and refused a 124 MB document holding 1.4 GB
    /// of paint as though it were 21.6 GB. See [`PixelPiece`].
    StackTooLarge {
        width: u32,
        height: u32,
        /// Layers holding pixels. Folders are not counted — one allocates
        /// nothing, so it cannot be part of what does not fit.
        layers: usize,
        /// What the layers read so far come to, which is the figure actually
        /// compared. A reader stops at the first layer that puts it over, so
        /// this is a *lower* bound on the whole document and the sentence says
        /// "at least".
        bytes: u64,
    },
    /// A well-formed file with nothing to paint on.
    Empty {
        format: SourceFormat,
        /// Why each layer was passed over, where the reader knows.
        ///
        /// **An import that refuses everything knows exactly why and used to
        /// throw all of it away.** Every reason a layer is dropped goes into
        /// [`ImportedDocument::warnings`], and warnings ride on the *document*
        /// — so on the one path where no document is built, the whole
        /// diagnosis is discarded and what reaches the artist is "contains no
        /// layers". That reads as a corrupt file, which is exactly the
        /// complaint the per-layer vector-layer sentence was written to answer;
        /// this is the same failure one level up, where it is worse, because
        /// there is no document for the warnings list to be shown beside.
        ///
        /// One entry per *distinct* reason rather than per layer, with a count,
        /// because a document of sixty-four placed images is one sentence and
        /// not sixty-four — the rule `EffectsNotPortable` already keeps.
        /// Readers with nothing to say pass an empty vector, and the sentence
        /// is then exactly what it always was.
        because: Vec<(usize, String)>,
    },
}

/// A reason as its own sentence: the first letter raised, nothing else touched.
///
/// The reasons are written to follow a colon, so they begin lower case. Lifted
/// out of that frame and stood on their own they need a capital, and only the
/// first character is looked at — a reason opening "Umber could not read…"
/// already has one, and one opening with anything that is not an ASCII letter
/// is left exactly as it is rather than being guessed at.
fn sentence_case(reason: &str) -> String {
    let mut chars = reason.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

impl ImportError {
    /// Reduce a reader's warnings to the distinct reasons layers were dropped.
    ///
    /// In `ImportError` rather than in one reader because nothing about it is
    /// Clip Studio's: any reader that can refuse every layer wants the same
    /// reduction, and a second copy of it is the drift this codebase refuses.
    /// Ordered by first appearance rather than by whatever order a map happened
    /// to hold — which for a `.clip` is **bottom of the stack first**, since
    /// that is the order `tree` walks, and is the opposite of the order the
    /// layers panel draws.
    pub fn reasons_from(warnings: &[ImportWarning]) -> Vec<(usize, String)> {
        let mut out: Vec<(usize, String)> = Vec::new();
        for warning in warnings {
            let ImportWarning::LayerSkipped { reason, .. } = warning else {
                continue;
            };
            match out.iter_mut().find(|(_, seen)| seen == reason) {
                Some((count, _)) => *count += 1,
                None => out.push((1, reason.clone())),
            }
        }
        out
    }
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
            Self::ImageTooLarge { width, height } => write!(
                f,
                "A picture inside this file is {width}×{height}, which is larger than Umber \
                 can decode. Umber reads pictures up to {max} pixels on a side.",
                max = ImportedDocument::MAX_DIMENSION,
            ),
            Self::StackTooLarge {
                width,
                height,
                layers,
                bytes,
            } => write!(
                f,
                "This document has {layers} layers at {width}×{height}, holding at least \
                 {held} of pixels. Umber reads at most {max} of layers from one file. \
                 Merging layers together in the application that made it will bring that \
                 down, and so will a smaller canvas: each halving of the width and height \
                 quarters the figure.",
                held = gigabytes(*bytes),
                max = gigabytes(ImportedDocument::MAX_TOTAL_BYTES),
            ),
            Self::Empty {
                format,
                because: reasons,
            } if reasons.is_empty() => {
                write!(f, "The {} file contains no layers.", format.label())
            }
            Self::Empty {
                format,
                because: reasons,
            } => {
                // "contains no layers" on its own is what an artist reads as a
                // damaged file. Saying that the file *was* read and naming what
                // stopped each layer is the difference between going looking
                // for a corrupt file and using one menu item.
                //
                // **It says the file was read and does not say it is
                // undamaged**, and the distinction is not pedantry: the reasons
                // below are whatever refused each layer, and some of them —
                // "Umber could not read the shape of its bitmap", "its bitmap
                // does not say how its channels are packed" — mean the file may
                // be damaged after all. A heading claiming otherwise would be
                // this module's own rule broken at the top of its own sentence.
                // The reassurance an intact document deserves is carried by its
                // reasons, which say what Clip Studio does and what to do about
                // it; a damaged one keeps the alarm its reasons raise.
                write!(
                    f,
                    "Umber read this {} file and found no layer in it that it can open.",
                    format.label()
                )?;
                // **The count goes after the reason, not in front of it.** A
                // reason is written to follow `Layer "X" could not be
                // imported:`, so every one of them opens "it is …" or "its …" —
                // and "2 layers: it is an image placed into the document" is a
                // plural subject with a singular clause hanging off it. As an
                // aside at the end the reason keeps the frame it was written
                // for and the count is what it is. One is spelled out, which is
                // the rule `editor::text_layers` already keeps.
                for (count, reason) in reasons {
                    let tally = if *count == 1 {
                        "one layer".to_string()
                    } else {
                        format!("{count} layers")
                    };
                    write!(f, "\n\n{}. ({tally}.)", sentence_case(reason))?;
                }
                Ok(())
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
/// `LayerStack::MAX`, painted layers for the sentence a byte refusal prints —
/// and with two bare numbers in the signature every reader could pass its entry
/// count twice. Every reader did, which is how a folder came to be charged for
/// a canvas it does not hold.
///
/// **The charge itself is no longer either of them.** A folder yields no
/// [`PixelPiece`], so [`PieceBudget::charge`] cannot bill one however the
/// counts are read; what `painted` decides now is only what the refusal *says*.
/// The type stays because the entry bound still needs its own reading, and
/// because a count somebody has to derive twice is a count that will eventually
/// be derived twice differently.
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
///
/// Public because `umber-app`'s graphics-memory refusal is the same sentence
/// about the same picture in a different unit's worth of memory, and two
/// spellings of "how big is your document" would eventually disagree about the
/// unit or the decimal place while both looking right on their own.
pub fn gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

/// Reject canvases and stacks an import could never open, before decoding any
/// pixels, and hand back the budget the decode is charged against.
///
/// Called by each reader as soon as it knows the header, which is the point of
/// it: a canvas past the ceiling, or a stack too tall for `LayerStack`, is
/// answerable from the header alone and decoding first would allocate several
/// gigabytes to reach the same answer.
///
/// **It no longer charges `canvas × 4 × painted`, and that is Stage 2's whole
/// point.** That product is what a densifying reader spent, and every format
/// Umber reads stores its layers sparsely: the 124 MB Clip Studio document that
/// provoked this holds about 1.4 GB of paint and was refused as 21.6 GB, a
/// figure no part of the file ever contained. The bound is now stated against
/// what the file can be *held* to rather than what it can *claim* — see
/// [`check_resident`], and `docs/perf/import-and-limits.md` §4.3, which is where
/// the "not closable by tuning" argument is retired.
///
/// **The two counts still count different things**, which is what [`StackSize`]
/// is for: `LayerStack::MAX` bounds entries and a folder occupies one, while the
/// byte figure is buffers and a folder holds none. The second half is now
/// *structural* — a folder yields no [`PixelPiece`], so there is nothing for a
/// caller to get the wrong way round — but the first still needs the reading,
/// and the painted count is what the refusal's sentence names.
///
/// Returning the budget rather than `()` is what makes the two halves one
/// thing: a reader cannot charge a decode without having checked the header,
/// and cannot check the header without being handed the means to charge.
fn check_bounds(
    format: SourceFormat,
    width: u32,
    height: u32,
    stack: StackSize,
) -> Result<PieceBudget, ImportError> {
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
    Ok(PieceBudget {
        width,
        height,
        layers: stack.painted,
        spent: 0,
        limit: ImportedDocument::MAX_TOTAL_BYTES,
    })
}

/// Refuse a picture whose *header* would otherwise choose the allocation.
///
/// **A decoder is handed a size and told to fill a buffer, and the size comes
/// out of somebody else's file.** `png`'s `output_buffer_size` is a pure
/// overflow check — it answers `None` only where `width × height × bytes` does
/// not fit an `isize`, so a 60000 × 60000 RGBA header answers
/// `Some(14_400_000_000)` from a file a few hundred bytes long. `psd` 0.3.5's
/// `generate_rgba` is worse: `vec![0; (w * h * 4) as usize]` straight off the
/// header, with the multiplication in `u32`. Neither `png::Limits::bytes` nor
/// the archive entry bound reaches either — the first bounds the *decoder's own*
/// allocations (one output row, the ICC profile, a few ancillary chunks) and the
/// second bounds the compressed entry, not what its header claims.
///
/// That matters more than an ordinary refusal for two reasons. `Vec`'s
/// allocation failure calls `handle_alloc_error`, which **aborts** — so the
/// panic hook never runs, there is no crash report and no autosave. And this
/// code runs inside Explorer's surrogate process through `umber-shellext`,
/// where an abort is somebody else's process dying.
///
/// **The bound is [`ImportedDocument::MAX_DIMENSION`] on each edge and is
/// deliberately not a figure of its own.** It is the ceiling [`check_bounds`]
/// already holds a canvas to, so nothing Umber can write can meet it: a saved
/// layer is trimmed to its content inside a canvas that is already under the
/// bound, and a `mergedimage.png` *is* the canvas. The rule that a reader must
/// never be stricter than the writer therefore holds by construction rather than
/// by measurement.
///
/// **What it does not do is make the allocation small.** At the ceiling a
/// picture is 32768² × 4 = 4 GiB, which is exactly what a canvas at
/// `MAX_DIMENSION` costs and what `MAX_TOTAL_BYTES` already admits three of. The
/// change is from a figure the file chooses without limit to the figure the rest
/// of this module is already stated in; a tighter one would refuse thumbnails of
/// documents Umber can open, and would need a survey nobody has run.
fn check_image_size(width: u32, height: u32) -> Result<(), ImportError> {
    if width > ImportedDocument::MAX_DIMENSION || height > ImportedDocument::MAX_DIMENSION {
        return Err(ImportError::ImageTooLarge { width, height });
    }
    Ok(())
}

/// What a document's layers actually hold, against
/// [`ImportedDocument::MAX_TOTAL_BYTES`].
///
/// **`layers` is the document's own painted count and `spent` is what has been
/// read so far**, which is why the refusal says "at least": a reader stops at
/// the layer that puts it over rather than decoding the rest to produce a
/// tidier number, and decoding the rest is exactly the spend the bound exists
/// to prevent.
///
/// A reader gets one out of [`check_bounds`] and nowhere else, so it cannot
/// charge a decode without having checked the header.
///
/// **`limit` is a field rather than a constant read inside**, for the reason
/// `CanvasRenderer::set_readback_limit` is one: reaching 16 GiB honestly means
/// allocating 16 GiB, which is not something to ask a CI runner for, and a test
/// that drove the comparison directly instead would be restating the rule it
/// claims to check. A test builds one of these with a small limit and charges
/// real pieces through the real function.
///
/// **What holds the fifth call site is not a behavioural test, and saying which
/// is the point.** Deleting `budget.charge(&layer)?` from a reader leaves every
/// test green: the document is still refused, by
/// [`ImportedDocument::validate`], but only *after* the memory has been spent —
/// which is a bound on the result and not on the accumulation, and the
/// accumulation is the whole reason the running charge exists. Demonstrated by
/// mutation rather than assumed. Two things stand in the way and both are
/// compiler-adjacent rather than behavioural: `#[must_use]` here, which under
/// CI's `-D warnings` makes an uncharged budget a build failure, and
/// `every_reader_that_checks_its_header_charges_what_it_decodes`, which reads
/// the readers' own source. The second is the shape `SaveLayer::text`'s guard
/// already takes — count the call sites rather than trust them to agree.
#[must_use]
struct PieceBudget {
    width: u32,
    height: u32,
    layers: usize,
    spent: u64,
    limit: u64,
}

impl PieceBudget {
    /// Charge one layer's pieces, refusing once the total is past the bound.
    ///
    /// A folder charges nothing because it holds no pieces; nothing has to
    /// remember to skip one.
    fn charge(&mut self, layer: &ImportedLayer) -> Result<(), ImportError> {
        self.spent = self.spent.saturating_add(layer.pixel_bytes());
        check_resident(self.width, self.height, self.layers, self.spent, self.limit)
    }

    /// Refuse a decode that is *about* to cost this much, before it happens.
    ///
    /// **For a reader that cannot yield pieces, which today is `.psd` alone.**
    /// The premise that retired the header comparison is that a hostile header
    /// claiming a huge canvas yields no pieces and costs nothing — and that is
    /// true of `.clip`, `.kra` and `.ora` and **false of `.psd`**, because
    /// `psd` 0.3.5's `Layer::rgba()` hands back a canvas-sized buffer per layer
    /// and there is nothing better to be had. Charging that reader only after
    /// each layer means refusing once the memory has been spent: a malformed
    /// file declaring `MAX_DIMENSION` square with a full stack is 4.29 GB a
    /// layer, so it would refuse on the fourth, which is the process being
    /// killed with a sentence written for it.
    ///
    /// **It looks ahead rather than committing**, so a reader may reserve for
    /// the worst case and still charge each layer as it lands: the accumulated
    /// figure stays what the document actually holds, and a layer the reader
    /// skipped costs nothing.
    fn reserve(&mut self, bytes: u64) -> Result<(), ImportError> {
        check_resident(
            self.width,
            self.height,
            self.layers,
            self.spent.saturating_add(bytes),
            self.limit,
        )
    }
}

/// The one comparison against [`ImportedDocument::MAX_TOTAL_BYTES`].
///
/// **Masks are deliberately not counted, and what that now costs is much more
/// than it used to.** Counting them is still wrong in the one direction that
/// matters: this is the check an *Umber* document goes through on the way back
/// in, so a bound that counted masks would refuse to reopen large masked
/// documents Umber itself had written — the reader would be stricter than the
/// writer, and the artist's own file would be the casualty.
///
/// **But the old figure for the exposure is stale and understates it by an
/// order of magnitude.** It used to read "a document whose every layer carries
/// one reaches roughly twice the figure", and that was true when a layer *was*
/// a canvas. The charge is now the pieces and a mask is still one canvas-sized
/// piece, so the ratio is the layers' occupancy: on the 20000×5000 document
/// this was built for, 53 layers hold 1.4 GB while masks on all of them would
/// be 21.2 GB — **fifteen times** the charged figure, admitted by a budget that
/// sees 1.4. The real mask ceiling is `LayerStack::MAX × canvas × 4`, which at
/// [`ImportedDocument::MAX_DIMENSION`] is 274 GB, and `.clip`'s mask path holds
/// the one-byte coverage canvas and its four-byte expansion at once. The
/// backstops are the caller's `max_texture_dimension_2d` check,
/// `LayerStack::MAX_SLOTS` and `umber-app`'s graphics-memory refusal, all three
/// of which do account for a mask on every layer — but none of them is a *host*
/// bound, and there is not one.
///
/// **The way out is to make the mask sparse, not to count it**, and what stops
/// that is stated at [`PixelPiece`]'s rule 3 rather than here: a `.clip` mask
/// has exactly the block presence a layer has, and the blocker is that the
/// upload's clear leaves transparent black, which a coverage slice reads as
/// *fully hidden*. That is a cost, not a fact about the bound.
fn check_resident(
    width: u32,
    height: u32,
    layers: usize,
    bytes: u64,
    limit: u64,
) -> Result<(), ImportError> {
    if bytes > limit {
        return Err(ImportError::StackTooLarge {
            width,
            height,
            layers,
            bytes,
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
            vec![PixelPiece::whole(
                size,
                vec![0; size.x as usize * size.y as usize * 4],
            )],
        )
    }

    /// A layer holding one rectangle of solid `value`, and nothing elsewhere.
    fn patch(name: &str, rect: PixelRect, value: u8) -> ImportedLayer {
        ImportedLayer::new(
            name,
            BlendMode::Normal,
            vec![PixelPiece::new(rect, vec![value; rect.area() as usize * 4])],
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
        // **And the byte bound is deliberately no longer among them.** 16384²
        // at 64 layers is a header claiming 68.7 GB, which this used to refuse
        // on the spot; it now passes, because a claim is not a cost. What the
        // file will actually be charged is the pieces it produces, and a header
        // claiming a canvas nobody painted on produces none.
        assert!(check_bounds(f, 16384, 16384, StackSize::all_painted(64)).is_ok());
        assert!(check_bounds(f, 2048, 2048, StackSize::all_painted(8)).is_ok());
    }

    /// The header's claim costs nothing and the file's content costs what it
    /// costs — which is the whole of Stage 2's bound.
    ///
    /// Driven through [`PieceBudget`] rather than through `check_resident`
    /// directly, because the budget is what a reader holds and the thing that
    /// could be got wrong is *what it charges*: a version that billed
    /// `canvas × 4` per layer, or that billed a folder, passes a test of the
    /// comparison and fails this one.
    #[test]
    fn a_document_is_charged_for_the_pixels_it_holds_and_not_for_its_canvas() {
        let f = SourceFormat::ClipStudio;
        // 16384² × 64 is 68.7 GB claimed. The stack is legal, so the header
        // check passes and the budget starts at nothing.
        let mut budget = check_bounds(f, 16384, 16384, StackSize::all_painted(64)).unwrap();
        assert_eq!(budget.spent, 0);

        // A layer holding one 256-square block costs a quarter of a megabyte,
        // not a quarter of a gigabyte.
        let block = PixelRect {
            x: 0,
            y: 0,
            width: 256,
            height: 256,
        };
        for i in 0..64 {
            budget
                .charge(&patch(&format!("L{i}"), block, 9))
                .expect("64 blocks is 16.8 MB and must not refuse");
        }
        assert_eq!(budget.spent, 64 * 256 * 256 * 4);

        // A folder is charged nothing, and nothing has to remember to skip one:
        // it holds no pieces.
        let before = budget.spent;
        budget
            .charge(&ImportedLayer::folder("Group", 0, true))
            .unwrap();
        assert_eq!(budget.spent, before);
    }

    /// The accumulation is what the bound stops, and it stops it partway.
    ///
    /// A reader charges as it decodes, so the layer that puts the document over
    /// is the last one read and the rest are never decoded — which is the spend
    /// the figure exists to prevent. The sentence therefore says "at least"
    /// rather than claiming to have measured the whole document.
    ///
    /// The limit is the budget's own field so this can drive the **real**
    /// `charge` over **real** pieces; see [`PieceBudget`] for why a test at
    /// 16 GiB is not one anybody can run, and note that a version reading the
    /// constant inside would have forced this test to restate the comparison
    /// instead of exercising it.
    #[test]
    fn the_budget_refuses_partway_through_and_says_so() {
        // Ten pixels a layer, and room for three of them.
        let rect = PixelRect {
            x: 0,
            y: 0,
            width: 10,
            height: 1,
        };
        let mut budget = PieceBudget {
            width: 4000,
            height: 4000,
            layers: 64,
            spent: 0,
            limit: 3 * 10 * 4,
        };

        for i in 0..3 {
            budget
                .charge(&patch(&format!("L{i}"), rect, 7))
                .unwrap_or_else(|e| panic!("layer {i} is inside the budget: {e}"));
        }
        let err = budget.charge(&patch("L3", rect, 7)).unwrap_err();

        // Ten of the sixty-four were never reached, and the sentence must not
        // pretend otherwise.
        let said = err.to_string();
        assert!(
            said.contains("at least"),
            "a refusal that stopped partway may not claim to have measured the whole \
             document: {said}"
        );
        assert!(
            said.contains("64 layers") && said.contains("4000×4000"),
            "the sentence names the stack and the canvas, which are the two levers: {said}"
        );
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

    /// **A reader must never be stricter than the writer**, and the piece
    /// contract is what finally makes that true of the byte bound.
    ///
    /// It used to be false in a stated, known way: a full 64-layer stack claims
    /// 16 GiB from 8192² upwards, so a 10000² document at 40 layers opened and
    /// the same canvas at 64 did not — a document Umber's own dialog will make
    /// and Umber's own writer will save. The claim was the whole problem, and
    /// the claim is gone.
    ///
    /// **`docformat` trims**, which is what closes it rather than a larger
    /// figure: `docformat::trim` writes each layer's PNG at its own content
    /// rectangle with an offset, and `openraster::load_layer` reads that back as
    /// one piece. So an Umber document is charged for the paint it holds, and
    /// the only stack that can still be refused is one that genuinely holds
    /// 17.2 GB of it.
    ///
    /// **The whole route, not the arithmetic.** A first draft of this asserted
    /// `check_bounds(...).is_ok()` at every canvas and then charged sixty-four
    /// tiny rectangles, and both halves were tautologies: `check_bounds` makes
    /// no byte comparison at all any more, so at a legal canvas and a legal
    /// count it cannot fail, and a megabyte against 16 GiB cannot either. It
    /// asserted the mechanism in its own prose and exercised none of it.
    ///
    /// So it saves a real full stack through `docformat` and reads it back
    /// through the reader, which is the only thing that can be wrong: if `trim`
    /// stopped cropping, or `openraster::load_layer` stopped cropping, every
    /// layer would come back a canvas.
    ///
    /// **The canvas is small and the historical figure is derived from what the
    /// round trip measured.** Reproducing the refusal in the round trip means a
    /// canvas over 8192 square — the smallest at which `canvas × 4 × MAX` clears
    /// 16 GiB — and that is 25 GB of zeroing and sixty-four PNG encodes, about
    /// seven seconds in a suite that otherwise takes one. What the file half has
    /// to establish is the *mechanism*; what the arithmetic half then does is
    /// apply it at the size that used to be refused, against a per-layer cost
    /// this test measured rather than assumed.
    #[test]
    fn a_document_umber_could_save_can_be_reopened() {
        use crate::docformat::{self, Canvas, SaveDocument, SaveError, SaveLayer};

        // The case that used to be refused, at the shape that refused it: a
        // full stack on a canvas where `canvas × 4 × MAX` is past the bound.
        // Derived, so a change to either constant moves the case rather than
        // leaving one that no longer exercises itself.
        let refused_edge = 10_000u64;
        assert!(
            refused_edge * refused_edge * 4 * LayerStack::MAX as u64
                > ImportedDocument::MAX_TOTAL_BYTES,
            "this canvas no longer makes the case: the claim is inside the bound"
        );
        let edge = 1024u32;

        // Each layer is one small mark, which is what a real stack of sixty-four
        // mostly is. **Deferred, so one canvas is live at a time** — holding all
        // sixty-four would be the 25.6 GB this test exists to say nobody has to
        // spend, which is a funny way to prove it.
        let size = UVec2::new(edge, edge);
        let mark_at = |i: usize| ((i + 1) * size.x as usize + i + 1) * 4;
        struct OneAtATime {
            size: UVec2,
            mark: usize,
        }
        impl docformat::Canvases for OneAtATime {
            fn layer(&mut self, index: usize) -> Result<std::borrow::Cow<'_, [u8]>, SaveError> {
                let mut buf = vec![0u8; self.size.x as usize * self.size.y as usize * 4];
                let px = ((index + 1) * self.size.x as usize + index + 1) * 4;
                buf[px..px + 4].copy_from_slice(&[9, 9, 9, 255]);
                self.mark = px;
                Ok(std::borrow::Cow::Owned(buf))
            }
            fn mask(&mut self, _: usize) -> Result<std::borrow::Cow<'_, [u8]>, SaveError> {
                unreachable!("no layer here carries one")
            }
            fn merged(&mut self) -> Result<std::borrow::Cow<'_, [u8]>, SaveError> {
                Ok(std::borrow::Cow::Owned(vec![
                    0u8;
                    self.size.x as usize
                        * self.size.y as usize
                        * 4
                ]))
            }
        }

        let names: Vec<String> = (0..LayerStack::MAX).map(|i| format!("L{i}")).collect();
        let layers: Vec<SaveLayer<'_>> = names
            .iter()
            .map(|name| SaveLayer::new(name.as_str(), BlendMode::Normal, Canvas::Deferred))
            .collect();
        let path = std::env::temp_dir().join(format!(
            "umber-reopen-a-saved-stack-{}.ora",
            std::process::id()
        ));
        docformat::save_from(
            &path,
            &SaveDocument {
                size,
                layers: &layers,
                active: 0,
                background: Background::Transparent,
                dpi: Document::DEFAULT_DPI,
                merged: Canvas::Deferred,
                history: None,
            },
            &mut OneAtATime { size, mark: 0 },
        )
        .expect("a document Umber can save");

        let bytes = std::fs::read(&path).expect("the archive");
        let _ = std::fs::remove_file(&path);
        let back = read_openraster(&bytes).expect("a document Umber saved must reopen");
        assert_eq!(back.layers.len(), LayerStack::MAX);

        // And it is cheap because `docformat::trim` wrote each layer at its own
        // content rectangle: sixty-four one-pixel marks, not sixty-four
        // canvases.
        let held: u64 = back.layers.iter().map(ImportedLayer::pixel_bytes).sum();
        assert!(
            held < 1 << 20,
            "a stack of one-pixel marks came back holding {held} bytes"
        );

        // **The case that used to be refused, at the per-layer cost this test
        // just measured.** A layer's cost is what it holds, not what canvas it
        // sits on, so the same stack on the 10000² canvas that met the bound
        // costs the same kilobytes — where the claim it used to be charged is
        // 25.6 GB. Both compared against the real function rather than
        // restated.
        assert!(
            check_resident(
                refused_edge as u32,
                refused_edge as u32,
                LayerStack::MAX,
                held,
                ImportedDocument::MAX_TOTAL_BYTES,
            )
            .is_ok(),
            "the stack this measured is refused on a large canvas, so trimming              bought nothing"
        );
        assert!(
            check_resident(
                refused_edge as u32,
                refused_edge as u32,
                LayerStack::MAX,
                refused_edge * refused_edge * 4 * LayerStack::MAX as u64,
                ImportedDocument::MAX_TOTAL_BYTES,
            )
            .is_err(),
            "if the old claim now passes, MAX_TOTAL_BYTES was raised and its              docs must say so"
        );

        // The picture is still there, which is what stops "cheap" being
        // "empty" — the mark of each layer, at each layer's own place. Read off
        // the piece rather than assembled, because assembling sixty-four of
        // these canvases is the spend again.
        for (i, layer) in back.layers.iter().enumerate() {
            let piece = &layer.pixels[0];
            let px = mark_at(i);
            let (x, y) = (px / 4 % size.x as usize, px / 4 / size.x as usize);
            assert_eq!(
                (piece.rect.x as usize, piece.rect.y as usize),
                (x, y),
                "layer {i}'s piece is not where its mark is"
            );
            assert_eq!(&piece.bytes[..4], &[9, 9, 9, 255], "layer {i}'s mark");
        }
    }

    /// A folder holds no slot and no buffer, so it may not be charged a canvas.
    ///
    /// **This used to be a property of the arithmetic and is now a property of
    /// the data**, which is the stronger form: `StackSize::painted` decided what
    /// the byte total charged, and a caller passing its entry count twice
    /// charged every folder a canvas — a Clip Studio document filed into groups
    /// paying for its own filing. A folder now yields no [`PixelPiece`], so
    /// there is nothing left to bill however the counts are read.
    ///
    /// What `StackSize` still decides is the entry bound and the word the
    /// refusal prints, and both halves are driven here.
    #[test]
    fn folders_are_not_charged_for_pixels_they_do_not_hold() {
        let f = SourceFormat::ClipStudio;
        let stack = StackSize::of((0..64).map(|i| i >= 4));
        assert_eq!((stack.entries, stack.painted), (64, 4));
        let mut budget = check_bounds(f, 10000, 10000, stack).unwrap();

        // Sixty folders charge nothing at all, whatever the canvas.
        for i in 0..60 {
            budget
                .charge(&ImportedLayer::folder(format!("G{i}"), 0, true))
                .unwrap();
        }
        assert_eq!(
            budget.spent, 0,
            "a folder holds no pieces to be charged for"
        );

        // And the entry bound still counts them, because a folder does occupy a
        // stack entry even though it occupies no memory. One painted layer at
        // the bottom of `MAX + 1` entries costs no bytes at all and is still a
        // stack too tall to hold.
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
        // **The case is derived from the constants that define it**, which is
        // the rule a raised ceiling has already caught this codebase on once:
        // the largest canvas Umber opens, at the full stack, holding what such
        // a stack would hold if it were solid paint.
        let edge = ImportedDocument::MAX_DIMENSION;
        let held = u64::from(edge) * u64::from(edge) * 4 * LayerStack::MAX as u64;
        let err = check_resident(
            edge,
            edge,
            LayerStack::MAX,
            held,
            ImportedDocument::MAX_TOTAL_BYTES,
        )
        .unwrap_err();
        let said = err.to_string();
        assert!(
            said.contains(&format!("{} layers", LayerStack::MAX)),
            "{said}"
        );
        assert!(said.contains(&format!("{edge}×{edge}")), "{said}");
        assert!(said.contains(&gigabytes(held)), "{said}");
        // **The bound is pinned as a literal and the document's figure is
        // not**, and the asymmetry is deliberate. The document's figure is this
        // test's own input, so deriving it says only that the sentence carries
        // what it was handed; the bound is a constant somebody may change, and
        // a literal is what makes that change fail here loudly rather than
        // agree with itself.
        assert!(
            said.contains("17.2 GB"),
            "the sentence must say what the bound is: {said}"
        );
        assert!(
            !said.contains("larger than Umber can open"),
            "a stack refusal must not wear the canvas refusal's words: {said}"
        );
        // **It offers the canvas as well as the stack.** Halving each edge
        // quarters the figure, which is the lever with the most leverage and
        // the one the sentence used to withhold — see
        // `docs/perf/import-and-limits.md` §5.2. And it no longer tells anybody
        // that Umber "holds at most" a figure, which reads as a promise about
        // their machine rather than about a constant.
        assert!(
            said.contains("smaller canvas"),
            "the canvas is a term in the arithmetic and has to be offered: {said}"
        );
        assert!(
            !said.contains("holds at most"),
            "a statement about a constant must not read as one about the machine: {said}"
        );
    }

    /// Rule 2: pieces do not overlap, and the sweep that says so actually
    /// notices when they do.
    ///
    /// Both directions, because a detector that answered `Some` for everything
    /// would pass the half of this that matters most to the readers.
    #[test]
    fn the_overlap_sweep_finds_an_overlap_and_leaves_a_grid_alone() {
        let cell = |x: u32, y: u32| PixelPiece {
            rect: PixelRect {
                x,
                y,
                width: 16,
                height: 16,
            },
            bytes: Vec::new(),
        };
        // A 4×4 grid of touching-but-not-overlapping squares, which is the
        // shape every block and tile reader produces.
        let grid: Vec<PixelPiece> = (0..4)
            .flat_map(|r| (0..4).map(move |c| cell(c * 16, r * 16)))
            .collect();
        assert_eq!(overlapping_pieces(&grid), None);

        // One pixel of overlap, in the middle of the sweep rather than at
        // either end.
        let mut broken = grid.clone();
        broken.push(cell(15, 15));
        assert!(overlapping_pieces(&broken).is_some());
    }

    /// **Every reader that checks its header charges what it decodes**, and
    /// this counts the call sites rather than trusting them.
    ///
    /// It is a source scan, which is a shape worth defending. The property is
    /// "the accumulation is bounded while the file is being read", and it is not
    /// reachable behaviourally: driving a reader over `MAX_TOTAL_BYTES` means
    /// building a 17.2 GB fixture, and the whole-document backstop in `validate`
    /// refuses the same document either way — so a reader whose running charge
    /// has been deleted passes every test there is. That was demonstrated by
    /// mutation. `#[must_use]` on `PieceBudget` catches the plain deletion under
    /// `-D warnings`; this catches the version that keeps the binding alive.
    ///
    /// **`flat.rs` is in the list deliberately**, though a one-layer PNG cannot
    /// reach the bound: a reader that opts out of the rule for a good reason
    /// today is a reader nobody re-examines when the reason stops holding.
    ///
    /// **What the sweep does not cover, said rather than left to be
    /// discovered**: the three flattened fallbacks — `openraster`'s and
    /// `krita`'s `flattened_fallback`, and `photoshop`'s `finish_flat` — each
    /// build one canvas-sized layer on a path that never returns to the loop,
    /// and none is charged. A scan of a file cannot tell them from the reader's
    /// main path. They are bounded at one canvas, which is inside the figure
    /// whatever the file says, and `validate` charges the document they
    /// produce; so it is a gap in the *sweep* and not in the bound.
    #[test]
    fn every_reader_that_checks_its_header_charges_what_it_decodes() {
        for (name, source) in [
            ("clipstudio", include_str!("clipstudio.rs")),
            ("krita", include_str!("krita.rs")),
            ("openraster", include_str!("openraster.rs")),
            ("photoshop", include_str!("photoshop.rs")),
            ("flat", include_str!("flat.rs")),
        ] {
            // Only the reader's own code, not its tests: a `charge` inside a
            // `#[cfg(test)]` block would satisfy this while bounding nothing.
            //
            // **No newline in the needle.** `include_str!` hands back the file
            // as it sits on disk, and `core.autocrlf` is the default on Windows
            // and on GitHub's Windows runners while `.gitattributes` says
            // nothing about `*.rs` — so a needle carrying `\n` matches on an LF
            // checkout and not on a CRLF one, where this would quietly start
            // scanning the very block it exists to exclude. Degrading in
            // silence is the worse shape of that bug, and this codebase has
            // already been caught by the mechanism once.
            let code = source
                .find("#[cfg(test)]")
                .map_or(source, |at| &source[..at]);
            assert!(
                code.contains("check_bounds("),
                "{name} does not check its header at all"
            );
            assert!(
                code.contains("budget.charge("),
                "{name} checks its header and never charges what it decodes, so a hostile \
                 file is bounded only after the memory has been spent"
            );
        }

        // **And the one reader that cannot yield pieces reserves as well.**
        // `psd` 0.3.5's `Layer::rgba()` hands back a canvas-sized buffer, so a
        // claim *is* a cost there and a per-layer charge arrives after the
        // gigabytes. Deleting the reserve leaves every other test green — no
        // fixture can drive 17.2 GB — which is exactly why it is named here
        // rather than trusted.
        assert!(
            include_str!("photoshop.rs").contains("budget.reserve("),
            "the .psd reader densifies, so it must be refused off its header \
             before it decodes a layer"
        );
    }

    /// `reserve` looks ahead without committing, which is what lets a reader do
    /// both: refuse the worst case up front, and still accumulate what the file
    /// turned out to hold.
    #[test]
    fn a_reservation_refuses_without_spending_the_budget() {
        let mut budget = PieceBudget {
            width: 100,
            height: 100,
            layers: 4,
            spent: 0,
            limit: 1000,
        };
        assert!(budget.reserve(1001).is_err(), "past the limit");
        assert!(budget.reserve(999).is_ok(), "inside it");
        assert_eq!(budget.spent, 0, "a reservation is not a charge");

        // It counts what has already been charged, so a reader that reserves
        // after decoding part of a document is not handed the budget twice.
        budget.spent = 900;
        assert!(budget.reserve(101).is_err());
        assert!(budget.reserve(100).is_ok());
    }

    /// A folder is not merely uncharged, it may hold no pieces at all: it takes
    /// no slot, so a rectangle on one has nowhere to be written.
    #[test]
    fn a_folder_carries_no_pieces() {
        let folder = ImportedLayer::folder("Group", 0, true);
        assert!(folder.pixels.is_empty());
        assert!(folder.mask.is_none());
        assert_eq!(folder.pixel_bytes(), 0);
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
        // **An upload is the rectangle the file holds, clipped to the page —
        // not a canvas.** This used to assert one layer's worth of bytes, and
        // that was a statement about the reader densifying rather than about
        // the document: the fixture places a 2×2 layer at (1, 1) on a 2×2
        // canvas, so one pixel of it is on the page and the other three were
        // transparent padding nobody wrote.
        assert_eq!(uploads[0].pieces.len(), 1);
        assert_eq!(
            uploads[0].pieces[0].rect,
            PixelRect {
                x: 1,
                y: 1,
                width: 1,
                height: 1
            }
        );
        assert_eq!(uploads[0].pieces[0].bytes.len(), 4);
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
