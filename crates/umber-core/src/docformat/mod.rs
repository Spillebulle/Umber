//! Writing documents — Umber's own saved-file format.
//!
//! ```no_run
//! # use std::path::Path;
//! # use glam::UVec2;
//! # let layers: Vec<umber_core::docformat::SaveLayer> = vec![];
//! # let merged: Vec<u8> = vec![];
//! let doc = umber_core::docformat::SaveDocument {
//!     size: UVec2::new(2048, 2048),
//!     layers: &layers,
//!     active: 0,
//!     background: umber_core::Background::WHITE,
//!     dpi: 72.0,
//!     merged: umber_core::docformat::Canvas::Held(&merged),
//!     history: None,
//! };
//! let warnings = umber_core::docformat::save(Path::new("sketch.ora"), &doc)?;
//! # Ok::<(), umber_core::docformat::SaveError>(())
//! ```
//!
//! # The format is OpenRaster
//!
//! Umber's document format is **`.ora`** — the same OpenRaster that
//! [`crate::docimport`] already reads. That is a deliberate refusal to invent a
//! format, and it is worth writing down why, because a painting application
//! having a container of its own is very nearly a reflex:
//!
//! * Everything Umber can hold — a canvas size, a bottom-to-top stack of
//!   layers, each with a name, an opacity, a visibility and a blend mode, and
//!   RGBA8 pixels — is *exactly* what ORA stores. A private format would be a
//!   re-spelling of a published one.
//! * The reader already exists and is under test. One format means one reader,
//!   and no second decoder to keep in step with the first.
//! * A `.ora` is a ZIP of PNGs and one small XML file. It can be opened with a
//!   file manager, and it can be read by Krita, GIMP, MyPaint, Drawpile and
//!   Pinta. Work made in Umber is therefore not hostage to Umber — which
//!   matters more for a young application than for an established one.
//!
//! # What Umber adds, and how it stays an ORA
//!
//! Several things Umber knows have nowhere to go in baseline ORA, so they are
//! written as extra attributes. XML readers ignore attributes they do not
//! recognise, so a file carrying them is still an ordinary `.ora` everywhere
//! else; the `umber-` prefix keeps them out of the way of anything the
//! specification may add later.
//!
//! * **[`VERSION_ATTR`]** on `<image>` — the revision of *these extensions*,
//!   not of ORA. A file whose number is higher than [`VERSION`] is refused on
//!   the way in, because a later revision exists precisely to store something
//!   this build would drop without knowing it had. What is written is the
//!   *lowest* revision that describes the file — see [`required_version`] —
//!   so a document using nothing new still opens in an older build.
//! * **[`MASK_ATTR`]** on a masked `<layer>`, naming an entry under `umber/`.
//!   The mask is deliberately not a layer of the ORA stack; see that constant.
//! * **[`EFFECTS_ATTR`]** on a `<layer>` carrying layer effects, naming an
//!   entry under `umber/effects/`. Same shape as the mask and for the same
//!   reason; see that constant.
//! * **[`TEXT_ATTR`]** on a `<layer>` whose pixels were *set* rather than
//!   painted, naming a record under `umber/text/`. See that constant and
//!   [`crate::textobj`]. **It is the one extension an older build can be
//!   actively *wrong* about**, and the two above are what make that precise
//!   rather than dramatic: a mask and an effect each took [`VERSION`] up, so an
//!   older build meeting one *refuses the document* and cannot be wrong about
//!   anything in it. Text declares nothing, so such a build opens the file, and
//!   the fingerprint in the record is what stands in for the version — it is
//!   why [`VERSION`] did not have to move, not a nicety beside it.
//! * **[`CLIP_ATTR`]**, **[`LOCK_ATTR`]** and **[`LINK_ATTR`]** on `<layer>`,
//!   each spelled `"true"` and each written only when set.
//! * **[`SELECTED_ATTR`]** on one `<layer>` — which layer was being painted on.
//! * **[`BLEND_ATTR`]** on a layer whose Umber mode has no exact SVG name.
//!   Only [`BlendMode::Add`] needs it: `svg:plus` is Porter-Duff addition on
//!   premultiplied colour and Umber's Add clamps straight colour, so they part
//!   company at soft edges. Without the hint, reopening a document Umber wrote
//!   would report an approximation that never happened.
//! * **[`BACKGROUND_ATTR`]** on the bottom `<layer>`, when the document has a
//!   background colour. See below — it is the *hint*, not the storage.
//! * **[`HISTORY_ATTR`]** on `<image>`, naming the entry that describes a saved
//!   undo history. See [`history`], and "The undo history" below.
//!
//! Resolution is **not** one of them. ORA's `<image>` already carries `xres`
//! and `yres`, which is what [`SaveDocument::dpi`] is written to and read from;
//! inventing `umber-dpi` beside a standard attribute would mean other
//! applications ignoring a number they already understand.
//!
//! # The background, and why it is a real layer in the file
//!
//! [`Background`] is a document property, not a layer — see
//! [`crate::document`] for why. ORA has no word for one, and the obvious
//! extension, an attribute on `<image>` naming a colour, has a cost that is
//! easy to miss: every other application would open the document on
//! transparency. A white painting arriving in Krita on a checkerboard is not a
//! dramatic failure, which is exactly what makes it a bad one — nobody notices
//! until they export.
//!
//! So the colour is written **both** ways. A full-canvas opaque `<layer>` named
//! "Background" goes in at the bottom of the stack, carrying the real pixels
//! for everyone else; and it is tagged with [`BACKGROUND_ATTR`], which is how
//! Umber's own reader knows to turn it back into the property rather than into
//! a layer the painter never made. Nothing can drift between them, because the
//! writer produces both from the same value.
//!
//! What each reader sees:
//!
//! | | |
//! |---|---|
//! | This Umber | the attribute; the layer PNG is never even decoded |
//! | An older Umber | one extra opaque layer, and **the same picture** |
//! | Krita, GIMP, MyPaint | one extra opaque layer, and the same picture |
//!
//! That last row is the whole argument, and the middle one is why [`VERSION`]
//! is *not* bumped for this: the rule is that a revision storing something an
//! older build would drop silently must be refused by it, and nothing is
//! dropped here. An older build shows every pixel, in the right place, with the
//! background merely editable. Refusing to open the file would cost more than
//! the degradation does.
//!
//! The price is one canvas-sized PNG of a solid colour — a few kilobytes after
//! deflate, since every row after the first filters to zeroes — and one
//! canvas-sized buffer built while the archive is. Beside the eight layer
//! readbacks a save already blocks on, it does not register.
//!
//! # Pixels
//!
//! [`SaveLayer::pixels`] is what a layer texture holds — sRGB-encoded with
//! alpha premultiplied in linear space — because that is what the renderer
//! reads back. ORA stores straight alpha, so this module converts, using the
//! exact inverse of the transform [`crate::docimport`] applies on the way in.
//! On the bytes a layer texture can actually contain the two round-trip
//! exactly, so saving and reopening moves nothing.
//!
//! # The undo history
//!
//! Also written, when the caller supplies one: the patches go into the archive
//! under `umber/`, named by a manifest and pointed at by [`HISTORY_ATTR`], so a
//! document reopened tomorrow can still be undone. [`history`] has the whole
//! argument — how a texture slot is turned into something that survives being
//! written down, what the file's own size budget is, and why none of it bumps
//! [`VERSION`].
//!
//! It is optional at both ends. A caller that passes `None` writes exactly the
//! file it always did, and a reader that finds no history opens the document
//! with an empty one, as every reader before this could.
//!
//! # What is still not saved
//!
//! The camera. A reopened document is framed to fit, like any other.
//!
//! # What a save costs the host, and the two rules that bound it
//!
//! A layer is four bytes a pixel, so the reference document
//! `docs/perf/formats-and-host-memory.md` argues from — 20000 × 5000, twenty
//! layers and four masks — is 400 MB a slice and 10 GB for the stack. Two rules
//! keep this module from being the thing that holds all of it at once:
//!
//! * **The archive is written as it is built, never assembled in memory.**
//!   [`save`] and [`save_from`] hand [`ZipWriter`] the temporary file itself, so
//!   a layer's PNG reaches the disk before the next layer is looked at and no
//!   entry is ever held after it has gone in. [`encode`] is the one function
//!   that still builds a `Vec<u8>`, and it exists for the round-trip tests and
//!   for callers that genuinely want the bytes.
//! * **A canvas-sized buffer may be *fetched* rather than held.** See
//!   [`Canvas`] and [`Canvases`]. A caller that can produce one layer at a time
//!   — reading it off the GPU, or taking it out of a capture — hands over a
//!   source instead of twenty borrows, and the writer asks for exactly one at a
//!   time. That the trait's methods take `&mut self` and hand back a
//!   `Cow<'_, [u8]>` is what makes "one at a time" a property the borrow checker
//!   enforces rather than one this module promises.

pub mod history;

use std::borrow::Cow;
use std::io::Write;
use std::path::Path;

use glam::UVec2;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::color::Color;
use crate::docimport::srgb;
use crate::document::Background;
use crate::effect::Effect;
use crate::geom::PixelRect;
use crate::layer::{BlendMode, LayerStack};

pub use history::{HISTORY_ATTR, SaveHistory};

/// Extension Umber saves with, without the dot.
pub const EXTENSION: &str = "ora";

/// Revision of Umber's ORA extensions that this build writes and reads.
///
/// Bumped only when a new revision stores something an older Umber would drop
/// silently — a mask, a group, a layer effect. Additions an older build can
/// safely ignore do not need it.
///
/// The document background did **not** need one, and it is worth saying why,
/// because it is the closest call so far: it is written as a real bottom layer
/// as well as an attribute, so an older build opens the file and shows every
/// pixel in the right place. Nothing is dropped, only degraded — the background
/// becomes an editable layer — and that does not meet the bar for refusing to
/// open somebody's document. Resolution did not either: it rides on ORA's own
/// `xres`/`yres`, and a build that ignores it renders an identical picture.
///
/// Neither did the **undo history**, and it is the clearest case of all: an
/// older build ignores an archive entry it has never heard of and opens the
/// document with an empty history — which is precisely what every build before
/// this one did with every file. Nothing about the picture is lost, and nothing
/// is dropped silently that could not have been. Refusing to open somebody's
/// painting because it carries a history they do not need would be a plainly
/// worse trade. [`history::VERSION`] governs that layout instead, and an
/// unreadable one is discarded rather than refused.
///
/// **2 is the first revision that earned it**, and it earned it for layer masks
/// and clipping. Both change what the picture *looks like* in a build that
/// ignores them, and change it silently: a masked layer would come back
/// covering everything the mask hid, and a clipped layer would come back
/// painting outside the layer it was bound to. Neither is a dramatic failure —
/// the file opens, every pixel is there — which is exactly what makes it the
/// case this attribute exists for. An older build now refuses the document
/// instead, and says which version it would need.
///
/// A lock and a link did **not** earn it and ride along on the same revision.
/// An older build that drops them shows the identical picture; what it loses is
/// a promise about what the artist can do to it next, which is recoverable by
/// setting the flag again and is not worth refusing somebody's painting over.
/// They are only ever written on a file that carries something else, or on one
/// declaring revision 1 — see [`required_version`].
///
/// **3 is layer effects**, and `docs/layer-effects.md` §8.2 is the argument —
/// which was put to the author as an open judgement and confirmed, because it
/// is the one call that could reasonably have gone the other way. The short
/// form: what an older build *shows* is merely plainer, a layer without its
/// shadow, which is the folder case. What it *writes* is the problem. Effects
/// are non-destructive and the parameters are the whole feature, so an older
/// build opens the document, ignores an attribute it has never heard of, and
/// the next save drops `umber/effects/` on the floor — permanently, having done
/// nothing but open and save. That is exactly the property masks and clipping
/// were refused for.
///
/// **`docs/group-compositing.md` §4.3 also proposes 3, for a folder with an
/// opacity or a blend mode of its own. Effects landed first and took 3, so
/// group compositing takes 4.** Only one feature can have a number, and the
/// decision belongs in whichever lands second rather than to
/// [`required_version`] to reconcile.
pub const VERSION: u32 = 3;

/// The lowest revision that describes `layers`.
///
/// The point is that a document with no masks and no clipping still declares
/// **1** and therefore still opens in every Umber that came before. Writing
/// [`VERSION`] unconditionally would lock every file this build touches away
/// from older ones in exchange for nothing: a revision number is a statement
/// about what a file *contains*, not about what wrote it.
///
/// **A text record is deliberately absent from this, and that is the whole of
/// why [`TEXT_ATTR`] did not move [`VERSION`].** An older build ignores the
/// attribute, decodes the ordinary layer PNG and shows the identical picture,
/// and — unlike an effect — it cannot silently *drop* anything by saving,
/// because the fingerprint in the record is what makes this build refuse a
/// record whose pixels have moved. So a document of nothing but text layers
/// still declares 1, and one carrying an effect as well declares 3 for the
/// effect alone. `text_never_raises_the_revision_a_document_declares` drives all
/// four combinations, because that is a claim about the two features *together*
/// and neither of them could have made it by itself.
fn required_version(layers: &[SaveLayer<'_>]) -> u32 {
    // Folders are skipped throughout, and not merely because they never carry
    // any of this today: `clipped` is a public field on `Layer` and nothing
    // stops it being set on a folder, where it means nothing and is written
    // nowhere. Reading it here would push a document to revision 2 — shutting
    // it out of every older Umber — for a flag with no effect on the picture at
    // all.
    //
    // The same filter governs effects, and there it is load-bearing in the
    // other direction: `encode` skips a folder's effects too, so the two agree
    // about what the file actually contains. A folder cannot hold one anyway —
    // `LayerStack::plan_set_effect` refuses it, because a folder has no
    // coverage to derive an effect from until group compositing lands
    // (`docs/layer-effects.md` §9.5) — and when it can, both halves change
    // together or a file carries effects while declaring a revision that does
    // not describe them.
    let layers = || layers.iter().filter(|l| !l.folder);

    // Highest first: a version number is a statement about what a file
    // contains, and effects are the newest thing it can contain.
    if layers().any(|l| !l.effects.is_empty()) {
        3
    } else if layers().any(|l| l.mask.is_some() || l.clipped) {
        2
    } else {
        1
    }
}

/// `<image>` attribute naming the revision the file needs — see
/// [`required_version`], which is not always [`VERSION`].
pub const VERSION_ATTR: &str = "umber-version";

/// `<layer>` attribute naming the archive entry holding the layer's mask.
///
/// The mask lives **outside** the ORA layer stack, under `umber/`, and that is
/// the whole of how a masked document stays a plain `.ora`. The alternatives
/// were both worse: a mask written as another `<layer>` would appear in every
/// other application's layers panel as a grey rectangle nobody made, and
/// Krita's convention — a nested `<stack>` whose second entry composites
/// `svg:dst-in` — is not baseline ORA and would be read by GIMP and MyPaint as
/// a layer that erases the one below it. An entry nobody else looks for is read
/// by nobody else, so what other applications see is the layer unmasked, which
/// is why this is one of the two things [`VERSION`] was raised for.
pub const MASK_ATTR: &str = "umber-mask";

/// `<layer>` attribute marking a layer clipped to the one below, spelled
/// `"true"`.
pub const CLIP_ATTR: &str = "umber-clip";

/// `<layer>` attribute marking a locked layer, spelled `"true"`.
pub const LOCK_ATTR: &str = "umber-lock";

/// `<layer>` attribute marking a linked layer, spelled `"true"`.
///
/// Still written, on every layer that belongs to any group, even though
/// [`LINK_GROUP_ATTR`] beside it says more. It is what an Umber built before
/// groups existed reads, and what it then does — treat every linked layer in
/// the document as one set — is exactly what it did before, which is the
/// behaviour that file was written by. See [`LINK_GROUP_ATTR`] for why neither
/// attribute moved [`VERSION`].
pub const LINK_ATTR: &str = "umber-link";

/// `<layer>` attribute naming which link group the layer is in, spelled as a
/// decimal number.
///
/// **This did not raise [`VERSION`]**, and the rule it is measured against is
/// the one in this module's docs: a version is raised where an older build
/// would drop something and show a picture that is *wrong*. A link changes no
/// pixel — it decides what travels with what when a layer is dragged — so a
/// build that reads only [`LINK_ATTR`] shows the same picture, and merely has
/// one set where this build has three. That is the same allowance locks and
/// links were given when [`VERSION`] went to 2.
///
/// Read in preference to [`LINK_ATTR`]; a file with the flag and no group is
/// one written before groups existed, and every linked layer in it joins group
/// zero — which is precisely the single set it was written as.
pub const LINK_GROUP_ATTR: &str = "umber-link-group";

/// Where a layer's mask goes inside the archive.
pub fn mask_src(index: usize) -> String {
    format!("umber/masks/{index:03}.png")
}

/// `<layer>` attribute naming the archive entry holding what *set* the layer.
///
/// Outside the ORA layer stack, under `umber/`, exactly as [`MASK_ATTR`] is and
/// for a reason of its own: `stack.xml` must not carry a paragraph of somebody's
/// prose. An attribute value is XML-escaped text, so a poem with `<`, `&` and a
/// newline in it would go into the one file every other OpenRaster reader parses,
/// and it would be the largest thing in it.
///
/// **This did not move [`VERSION`], and unlike the locks and the links it is not
/// obvious.** An older build ignores the attribute and decodes the ordinary
/// layer PNG, so it shows the identical picture and loses only that the text can
/// be set again — plainer, not wrong, which is the line the version is drawn on.
/// What makes that honest rather than merely convenient is that such a build can
/// also *paint* on the layer and save it: the record would then describe pixels
/// it did not make, and re-rendering would destroy the brushwork. So the record
/// carries a fingerprint of the layer image it rendered, and a mismatch on the
/// way back in discards the record and keeps the picture. See
/// [`crate::textobj`], and `docs/text-tool.md` §3.
pub const TEXT_ATTR: &str = "umber-text";

/// Where a layer's text record goes inside the archive.
///
/// Indexed exactly as [`mask_src`] and [`effects_src`] are — by the layer's
/// position in the *file*, top first.
pub fn text_src(index: usize) -> String {
    format!("umber/text/{index:03}.json")
}

/// `<layer>` attribute naming the archive entry holding the layer's effects.
///
/// **The mask's shape, deliberately** — outside the ORA layer stack, under
/// `umber/`, named by an attribute on the element. `docs/layer-effects.md` §8.1
/// has the argument, and the load-bearing half is the *attribute*: a single
/// document-wide table would have to be keyed by something, and every candidate
/// is wrong. A stack position shifts the moment a layer is reordered, a name is
/// not unique, and `Layer::id` is explicitly a within-session identity that is
/// never written down. An attribute on the element travels with the element and
/// needs no key at all.
///
/// Serialising the parameters into the attribute value itself was the
/// alternative. It works, and it puts an escaped RON blob in the middle of
/// `stack.xml` that every other reader has to skip past and every human has to
/// read around. One zip entry per effected layer is cheaper to read in both
/// senses.
///
/// Nothing here is a new extension mechanism: the `umber-` prefix *is* the
/// mechanism, and every other ORA reader ignores both the attribute and the
/// directory. What they see is the layer without its effects — which is
/// precisely why this is what took [`VERSION`] to 3, and why an effected layer
/// raises [`SaveWarning::EffectsNotPortable`].
pub const EFFECTS_ATTR: &str = "umber-effects";

/// Where a layer's effects go inside the archive.
///
/// Indexed exactly as [`mask_src`] is — by the layer's position in the *file*,
/// top first, which is the numbering `data/layer000.png` already uses. So a
/// folder leaves a gap here as it does there, and for the same reason: the
/// number is a name, and renumbering the layers consecutively would make it
/// stop matching the entry it belongs to.
pub fn effects_src(index: usize) -> String {
    format!("umber/effects/{index:03}.ron")
}

/// `<layer>` attribute marking the selected layer, spelled `"true"`.
pub const SELECTED_ATTR: &str = "umber-selected";

/// `<layer>` attribute naming an Umber blend mode outright.
pub const BLEND_ATTR: &str = "umber-blend";

/// `<layer>` attribute marking the layer that *is* the document background,
/// spelled as an sRGB hex triple — see the module docs for why the colour is
/// written twice.
///
/// On the layer rather than on `<image>` so the two travel together: a reader
/// that has the attribute has the layer to skip, and one that has neither has
/// an ordinary ORA.
pub const BACKGROUND_ATTR: &str = "umber-background";

/// `src` of the layer holding the document background.
pub const BACKGROUND_SRC: &str = "data/background.png";

/// Name the background layer carries, which is what other applications show in
/// their layers panel.
const BACKGROUND_NAME: &str = "Background";

/// The `version` every OpenRaster file declares. Not Umber's — the format's.
const ORA_VERSION: &str = "0.0.3";

/// Longest edge of the thumbnail the specification asks every writer to include.
const THUMBNAIL_MAX: u32 = 256;

/// Where a canvas-sized buffer is, on its way into the archive.
///
/// A layer is `width * height * 4` bytes — 400 MB on the reference canvas of
/// `docs/perf/formats-and-host-memory.md` — so whether the writer *borrows*
/// twenty of them or *asks for* one at a time is the difference between ten
/// gigabytes of host memory and four hundred megabytes. It is a decision the
/// caller makes and this is where it is said.
///
/// [`Self::Held`] is what every caller did before there was a choice, and it is
/// still right wherever the bytes exist anyway: the round-trip tests, and a
/// caller whose buffers came from somewhere that cannot be asked again.
///
/// [`Self::Deferred`] is refused by [`encode`] and [`save`], which have no
/// source to ask — with a named error rather than an empty layer, because a
/// document silently saved with blank pixels is the worst failure this module
/// could have.
#[derive(Clone, Copy, Debug)]
pub enum Canvas<'a> {
    /// The caller holds these bytes for the whole save.
    Held(&'a [u8]),
    /// The writer asks [`Canvases`] for these when it reaches them, and drops
    /// them before it asks for the next.
    Deferred,
}

impl<'a> From<&'a [u8]> for Canvas<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        Self::Held(bytes)
    }
}

impl<'a> From<&'a Vec<u8>> for Canvas<'a> {
    fn from(bytes: &'a Vec<u8>) -> Self {
        Self::Held(bytes)
    }
}

/// What a save asks for the canvas-sized buffers it was not handed.
///
/// **One at a time, and that is structural rather than promised.** Every method
/// takes `&mut self` and hands back a `Cow<'_, [u8]>` borrowed from it, so a
/// caller may serve every request out of one reused buffer and the writer
/// *cannot* be holding a second when it asks for the next — the borrow checker
/// refuses it. That is the whole mechanism by which a save of the reference
/// document costs one canvas rather than one per layer.
///
/// The order the writer asks in is the order the archive holds things, which is
/// **top of the stack first**, then the flattened image last. A caller that can
/// only produce buffers in some other order has to hold them, and should say
/// [`Canvas::Held`].
///
/// Indices are into [`SaveDocument::layers`], which is bottom-first — the same
/// numbering the caller built the stack with, so nothing has to be reversed on
/// the way out.
pub trait Canvases {
    /// Entry `index`'s pixels, canvas-sized and in layer-texture form.
    ///
    /// Asked once per non-folder entry. A folder holds no slice and is never
    /// asked about.
    fn layer(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError>;

    /// Entry `index`'s mask slice, canvas-sized and in the same form.
    ///
    /// Asked only where [`SaveLayer::mask`] is `Some`, so a document with no
    /// masks never reaches this.
    fn mask(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError>;

    /// The flattened composite, straight-alpha sRGB, canvas-sized.
    ///
    /// Asked once, last, and held across both entries it feeds —
    /// `mergedimage.png` and the thumbnail scaled down from the same bytes.
    fn merged(&mut self) -> Result<Cow<'_, [u8]>, SaveError>;
}

/// The source [`encode`] and [`save`] use: there isn't one.
///
/// Every method refuses, naming what was asked for. A [`Canvas::Deferred`]
/// reaching one of those two is a caller bug — they were handed no way to fetch
/// anything — and the alternative to refusing is a layer written blank.
struct NoCanvases;

impl Canvases for NoCanvases {
    fn layer(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
        Err(SaveError::NotSupplied {
            what: format!("pixels of layer {index}"),
        })
    }

    fn mask(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
        Err(SaveError::NotSupplied {
            what: format!("mask of layer {index}"),
        })
    }

    fn merged(&mut self) -> Result<Cow<'_, [u8]>, SaveError> {
        Err(SaveError::NotSupplied {
            what: "flattened image".to_string(),
        })
    }
}

/// One layer on its way to disk.
pub struct SaveLayer<'a> {
    pub name: &'a str,
    pub visible: bool,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub blend: BlendMode,
    /// `width * height * 4` bytes in layer-texture form — sRGB-encoded with
    /// alpha premultiplied in linear space. See the module docs.
    ///
    /// [`Canvas::Deferred`] where the caller would rather be asked; see that
    /// type for why a large document wants to be.
    pub pixels: Canvas<'a>,
    /// The layer's mask slice, canvas-sized and in the same form as `pixels`.
    ///
    /// Only the **red** channel is written — a mask is coverage, and the slice
    /// carries the same value in all three colour channels. See
    /// [`MASK_ATTR`].
    ///
    /// `Some` is what says there *is* a mask, whether or not its bytes are in
    /// hand: a deferred one is `Some(Canvas::Deferred)`, and `None` is the only
    /// way to say a layer has none.
    pub mask: Option<Canvas<'a>>,
    /// The layer's effects, in composite order — exactly what
    /// `Layer::effects` hands back. See [`EFFECTS_ATTR`].
    ///
    /// Written whole, enabled or not: a switched-off effect is still a set of
    /// parameters somebody dialled, and dropping it at the save would be the
    /// silent loss this whole entry exists to prevent. It is also what makes
    /// [`required_version`] read the *presence* of an effect rather than
    /// `enabled_effect_count`.
    pub effects: &'a [Effect],
    /// Bounded by the alpha of the nearest unclipped layer below.
    pub clipped: bool,
    /// Refuses edits until unlocked.
    pub locked: bool,
    /// Which link group this layer belongs to, if any. See [`LINK_GROUP_ATTR`].
    pub link: Option<u8>,
    /// What set this layer's pixels, where they were set rather than painted.
    ///
    /// Written as its own archive entry under `umber/text/` and pointed at by
    /// [`TEXT_ATTR`], with a fingerprint of the layer image *this save* is
    /// writing — never one the caller supplies, because a fingerprint the model
    /// carried could be stale and this one cannot. See [`crate::textobj`].
    pub text: Option<&'a crate::textobj::TextObject>,
    /// How deeply nested, 0 at the top level. See [`crate::layer`]'s docs.
    pub depth: u8,
    /// This entry is a folder: it becomes a nested `<stack>` and has no `src`,
    /// no PNG and no pixels.
    ///
    /// `pixels` is empty for one, and the canvas-size check every layer is held
    /// to is skipped — a folder has nothing to be the wrong size.
    pub folder: bool,
}

impl<'a> SaveLayer<'a> {
    /// A layer with none of the flags set and no mask.
    ///
    /// A constructor rather than `Default`, because the three fields it does
    /// take have no sensible default and a half-built layer is not a thing to
    /// hand round. Its purpose is that adding a flag here does not mean
    /// touching every test that builds one.
    pub fn new(name: &'a str, blend: BlendMode, pixels: impl Into<Canvas<'a>>) -> Self {
        Self {
            name,
            visible: true,
            opacity: 1.0,
            blend,
            pixels: pixels.into(),
            mask: None,
            effects: &[],
            clipped: false,
            locked: false,
            link: None,
            text: None,
            depth: 0,
            folder: false,
        }
    }

    /// A folder: a nested `<stack>` with a name, an eye and nothing else.
    ///
    /// No opacity and no blend mode, because a *pass-through* folder has
    /// neither — see [`crate::layer`]'s docs. Writing an opacity here would be
    /// the one thing that made this file open showing a different picture in an
    /// older Umber, which is what [`required_version`] would then have to
    /// answer for.
    pub fn folder(name: &'a str, depth: u8, visible: bool) -> Self {
        Self {
            visible,
            depth,
            folder: true,
            ..Self::new(name, BlendMode::Normal, &[][..])
        }
    }
}

/// A document on its way to disk.
pub struct SaveDocument<'a> {
    pub size: UVec2,
    /// Bottom to top, matching [`LayerStack`]'s own order.
    pub layers: &'a [SaveLayer<'a>],
    /// Index into `layers` of the one being painted on.
    pub active: usize,
    /// What lies under the stack. A colour becomes an extra bottom layer in the
    /// file — see the module docs.
    pub background: Background,
    /// Pixels per inch, written to ORA's own `xres` and `yres`.
    ///
    /// Rounded to a whole number on the way out, which is what those attributes
    /// mean and what every other writer puts there.
    pub dpi: f32,
    /// The flattened composite, straight-alpha sRGB, canvas-sized.
    ///
    /// Required by the specification, and supplied by the caller rather than
    /// computed here **on purpose**: flattening means blend modes, and the
    /// blend maths lives in one place — the composite shader. A software copy
    /// of it in this module would be a second implementation to keep in step,
    /// and a saved file whose preview disagreed with the screen is exactly the
    /// bug that arrangement produces.
    ///
    /// [`Canvas::Deferred`] is asked for **last**, after every layer has gone
    /// in, which is what lets a caller that composites on demand pay for one
    /// canvas rather than carrying it beside the whole stack.
    pub merged: Canvas<'a>,
    /// The undo history, resolved against the stack by
    /// [`SaveHistory::new`] — `None` writes the file exactly as it was written
    /// before histories were saved at all.
    ///
    /// `None` is also what a history that could not be resolved becomes, and
    /// what the preference for not saving one produces. See [`history`].
    pub history: Option<SaveHistory<'a>>,
}

/// Something a save could not carry across in full.
///
/// Typed for the same reason [`crate::docimport::ImportWarning`] is: so the UI
/// can count and group them rather than print a paragraph per layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveWarning {
    /// The layer's mode has no exact OpenRaster name, so other applications
    /// will composite it a little differently. Umber's own reader will not —
    /// see [`BLEND_ATTR`].
    BlendApproximated {
        layer: String,
        mode: &'static str,
        used: &'static str,
    },
    /// The layer's text record could not be written, so the layer is paint in the
    /// file. Every pixel is there; what is lost is that the text can be set
    /// again.
    ///
    /// `why` carries the reason rather than the sentence assuming one, because
    /// there are two and the artist can act on the difference: a record over
    /// [`crate::textobj::MAX_RECORD_BYTES`], or a figure that is not a figure.
    /// Saying "too much text" for the second is a notice that lies.
    ///
    /// Named rather than passed over for the reason every import warning is: a
    /// text layer that had quietly stopped being editable by the time somebody
    /// reopened the document is a loss discovered instead of reported. **An
    /// autosave drops this**, along with every other save warning, and that is
    /// right — a notice nobody asked for, over a copy nobody asked for, is the
    /// dialog that reappears every five minutes.
    TextNotRecorded {
        layer: String,
        why: crate::textobj::NotRecorded,
    },
    /// Some layers carry effects, which no other OpenRaster reader can see.
    ///
    /// Named in the same breath as an approximated blend mode because it is
    /// the same kind of loss — the file is still a plain `.ora` and still holds
    /// every pixel, and what another application shows is the layer unadorned.
    ///
    /// **Once for the document, with a count, and not once per layer.**
    /// `docs/layer-effects.md` §8.3 says "told once at the save", and the
    /// reasoning is the one [`crate::docimport::ImportWarning::
    /// EffectsOverBudget`] already gives for itself: thirty effected layers
    /// would be thirty lines of the same sentence with a different name in
    /// them, which is the noise that stops the list being read at all.
    /// [`Self::BlendApproximated`] is per layer and is not a precedent — it
    /// names a *different mode* each time, where this says one thing however
    /// many layers it is true of.
    ///
    /// **Counted on layers whose effects are switched *on*.** A layer whose
    /// every effect is disabled draws plain in Umber too, so a warning about it
    /// would name a loss that did not happen — the trap `export::losses`
    /// already avoids by asking whether *this* document has transparency. The
    /// record is still written and [`required_version`] still reads presence
    /// rather than enablement, because the parameters are what an older build
    /// would drop; only the sentence is about what is visible.
    ///
    /// It is emphatically **not** a loss for Umber. The parameters are in the
    /// file, [`EFFECTS_ATTR`] points at them, and [`required_version`] declares
    /// the revision that says so, which is why an older Umber refuses the
    /// document rather than quietly dropping them.
    EffectsNotPortable { layers: usize },
}

impl std::fmt::Display for SaveWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BlendApproximated { layer, mode, used } => write!(
                f,
                "Layer “{layer}”: OpenRaster has no exact equivalent of {mode}, so it is \
                 written as {used}. Umber reopens it as {mode}; other applications will \
                 composite it slightly differently where the layer is partly transparent."
            ),
            Self::TextNotRecorded { layer, why } => write!(
                f,
                "Layer “{layer}”: {}, so it was saved as paint. The picture is complete, but \
                 the text cannot be edited again after the document is reopened.",
                why.reason()
            ),
            Self::EffectsNotPortable { layers } => write!(
                f,
                "{layers} {} layer effects. Umber saves them and reopens them, but no other \
                 OpenRaster application can read them, so {} look plain everywhere else.",
                if *layers == 1 {
                    "layer has"
                } else {
                    "layers have"
                },
                if *layers == 1 { "it will" } else { "they will" },
            ),
        }
    }
}

/// Why a save failed.
#[derive(Debug)]
pub enum SaveError {
    Io(std::io::Error),
    /// A document with nothing in it. Cannot happen from the editor, which
    /// never lets the last layer go, but the format would be invalid.
    Empty,
    /// More layers than [`LayerStack`] could ever open again. Refused while the
    /// file on disk is still the old one, rather than written and then found
    /// unopenable.
    TooManyLayers {
        found: usize,
        max: usize,
    },
    /// A buffer that is not the canvas size. A caller bug, but one worth
    /// naming: the alternative is a file whose layers are silently sheared.
    WrongSize {
        what: String,
        found: usize,
        expected: usize,
    },
    /// A buffer the document said would be fetched that could not be.
    ///
    /// Named rather than passed over for the reason [`Self::WrongSize`] is: the
    /// alternative to refusing is a layer written blank, which is a document
    /// silently damaged by its own save.
    ///
    /// **Two unlike failures share it, and both belong here.** One is a caller
    /// bug — a [`Canvas::Deferred`] reaching [`encode`] or [`save`], which are
    /// handed no [`Canvases`] at all. The other is a source that genuinely
    /// could not produce a buffer it was asked for: the autosave's capture
    /// coming home short of a slice, which is a runtime failure of the
    /// readback and not a bug anywhere. What makes one variant right for both
    /// is that the *consequence* is identical and is the only thing the artist
    /// can act on — this document was not written, and none of it was.
    ///
    /// `what` is filled in by `resolve` rather than by the source, so it
    /// names the **layer** and not the index the source was asked in: a source
    /// knows which slice it failed to read and does not know what the artist
    /// called it.
    NotSupplied {
        what: String,
    },
}

impl From<std::io::Error> for SaveError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<zip::result::ZipError> for SaveError {
    fn from(e: zip::result::ZipError) -> Self {
        // Every variant that can arise while *writing* an archive we control is
        // an I/O failure underneath — a full disk, a disconnected drive.
        Self::Io(std::io::Error::other(e))
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "The file could not be written: {e}"),
            Self::Empty => write!(
                f,
                "The document has no layers, so there is nothing to save."
            ),
            Self::TooManyLayers { found, max } => write!(
                f,
                "The document has {found} layers; Umber can only reopen {max}, so it was \
                 not saved."
            ),
            Self::WrongSize {
                what,
                found,
                expected,
            } => write!(
                f,
                "The {what} came back as {found} bytes where {expected} were expected, so \
                 the document was not saved."
            ),
            // One sentence for all three of `Wanted`'s spellings, which is why
            // it is built round "could not read the …" rather than round a
            // plural: "the mask of layer “Ink”" and "the flattened image" are
            // both singular and "the pixels of layer “Ink”" is not.
            Self::NotSupplied { what } => write!(
                f,
                "Umber could not read the {what}, so the document was not saved."
            ),
        }
    }
}

impl std::error::Error for SaveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Write `doc` to `path`.
///
/// Returns whatever the format could not carry exactly; an empty list means the
/// file holds everything the document did.
///
/// The file is written to a temporary neighbour and renamed into place. A save
/// that fails halfway — a full disk, a pulled USB stick — would otherwise leave
/// a truncated archive where the artist's last good version used to be, which
/// is the one failure a save must not have. The archive is **streamed** into
/// that temporary rather than assembled in memory first: see the module docs,
/// and note that it makes no difference to the guarantee, because what is
/// renamed is still only ever a file that was written to the end.
///
/// Every buffer has to be [`Canvas::Held`]; [`save_from`] is the one that can
/// fetch.
pub fn save(path: &Path, doc: &SaveDocument<'_>) -> Result<Vec<SaveWarning>, SaveError> {
    save_from(path, doc, &mut NoCanvases)
}

/// [`save`], asking `canvases` for every [`Canvas::Deferred`] buffer as the
/// archive reaches it.
///
/// This is what a document too large to hold in host memory is written by: the
/// writer asks for one canvas-sized buffer at a time, in archive order, and the
/// source may serve every one of them out of a single reused allocation. See
/// [`Canvases`].
///
/// The same one temporary and the same one rename. A source that fails partway
/// through — a readback that could not map — leaves the temporary discarded and
/// the artist's file exactly as it was, which is the same guarantee a full disk
/// already had.
pub fn save_from(
    path: &Path,
    doc: &SaveDocument<'_>,
    canvases: &mut dyn Canvases,
) -> Result<Vec<SaveWarning>, SaveError> {
    let mut warnings = Vec::new();
    write_with(path, |file| {
        // Buffered, because the writer now makes many small writes where it
        // used to make one large one: a `File` has no buffer of its own, so a
        // PNG streamed straight at it is a syscall per chunk.
        let sink = std::io::BufWriter::with_capacity(64 * 1024, file);
        warnings = stream_archive(sink, doc, canvases)?.1;
        Ok(())
    })?;
    Ok(warnings)
}

/// Build the archive into `sink`, and hand `sink` back with it.
///
/// The composition [`save_from`] uses, factored out so a test can drive it over
/// a sink that fails — which is the failure streaming introduces and is not
/// reachable through a path.
fn stream_archive<W: Write + std::io::Seek>(
    sink: W,
    doc: &SaveDocument<'_>,
    canvases: &mut dyn Canvases,
) -> Result<(W, Vec<SaveWarning>), SaveError> {
    let mut watched = Watched::new(sink);
    let mut zip = ZipWriter::new(&mut watched);
    let warnings = write_archive(&mut zip, doc, canvases)?;
    // `finish` writes the central directory; the sink may still be holding the
    // tail of it, and a `BufWriter`'s own `Drop` would swallow that error.
    // Neither can fail here — `Watched` absorbs — so the reading that matters
    // is the one after them.
    zip.finish()?.flush()?;
    match watched.give_up() {
        (_, Some(e)) => Err(SaveError::Io(e)),
        (sink, None) => Ok((sink, warnings)),
    }
}

/// A sink that stops writing at its first failure, remembers it, and goes on
/// *reporting* success.
///
/// **`ZipWriter` may not be shown an I/O error, and that is a real defect
/// rather than a preference.** Handed one part-way through an entry, zip 8.6.0
/// finalises the entry it was in the middle of; `finish_file` then reads a
/// stream position behind where the entry started and trips
/// `debug_assert!(file_end >= self.stats.start)`. So a full disk during a save
/// is a **panic** in any build with debug assertions on, and in a release build
/// the same subtraction merely wraps into a nonsense entry size. Neither is
/// reachable from the shape this replaced, where the archive was built in a
/// `Vec<u8>` that could not fail.
///
/// **It reaches that assertion from `finish()` as well as from `Drop`**, which
/// is the part that decides the remedy rather than merely describing it.
/// `finalize` calls `finish_file` directly, so a quarter of the failing
/// positions panic on the ordinary path — and `mem::forget` on the error path,
/// which is the obvious alternative and costs a leak on every failed save,
/// would not have helped with any of them. Absorbing is not the tidier of two
/// options; it is the one that works.
///
/// Nothing is lost by it: the bytes after a failure are going into a temporary
/// that is about to be deleted, and the error itself is kept and returned.
/// Positions are tracked here rather than asked of the inner sink, because zip
/// seeks back to patch each local header and those seeks have to keep answering
/// after the writes have stopped landing.
struct Watched<W> {
    inner: W,
    /// The first failure, which is the one worth reporting: everything after
    /// it is a consequence.
    failed: Option<std::io::Error>,
    /// Where the stream would be if every write had landed.
    at: u64,
}

impl<W: Write + std::io::Seek> Watched<W> {
    fn new(mut inner: W) -> Self {
        // Asked rather than assumed to be zero. It always is from `save_from`,
        // which hands over a file it has just created — but the count only has
        // to be right *relative* to the inner sink, and starting it at zero
        // over a sink that was already somewhere would hand the ZIP writer
        // offsets short by that much for as long as it went on writing
        // successfully.
        //
        // A sink that cannot say where it is is one that cannot be written to
        // either, so the position is *also* an error worth keeping rather than
        // an assumption worth making: it goes into `failed` like any other, and
        // the save is refused rather than proceeding from a guess. That is what
        // the rest of this type does with a failure, and this is the one place
        // it could have been written not to.
        let (at, failed) = match inner.stream_position() {
            Ok(at) => (at, None),
            Err(e) => (0, Some(e)),
        };
        Self { inner, failed, at }
    }

    fn give_up(self) -> (W, Option<std::io::Error>) {
        (self.inner, self.failed)
    }

    fn note(&mut self, e: std::io::Error) {
        if self.failed.is_none() {
            self.failed = Some(e);
        }
    }
}

impl<W: Write + std::io::Seek> Write for Watched<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.failed.is_none() {
            match self.inner.write(buf) {
                Ok(n) => {
                    self.at += n as u64;
                    return Ok(n);
                }
                Err(e) => self.note(e),
            }
        }
        self.at += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.failed.is_none()
            && let Err(e) = self.inner.flush()
        {
            self.note(e);
        }
        Ok(())
    }
}

impl<W: Write + std::io::Seek> std::io::Seek for Watched<W> {
    fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
        if self.failed.is_none() {
            match self.inner.seek(to) {
                Ok(at) => {
                    self.at = at;
                    return Ok(at);
                }
                Err(e) => self.note(e),
            }
        }
        // Answered from the count this kept, so the writer's own bookkeeping
        // stays consistent after the bytes have stopped landing. `End` is the
        // one it cannot answer for — the inner sink's length is exactly what is
        // no longer known — and standing still is **not** an arbitrary choice
        // there: `zip::write::finalize` compares the footer's end against the
        // archive's, and an answer *larger* than the truth takes a branch whose
        // own `debug_assert!(stream_position()? == archive_end)` this would then
        // trip. `u64::MAX` makes `Watched` panic on its own account. Standing
        // still and delegating are both safe; anything past the end is not.
        self.at = match to {
            std::io::SeekFrom::Start(n) => n,
            std::io::SeekFrom::Current(d) => self.at.saturating_add_signed(d),
            std::io::SeekFrom::End(_) => self.at,
        };
        Ok(self.at)
    }
}

/// Put bytes somebody already has at `path`, whole or not at all.
///
/// [`write_with`] with the simplest possible `fill`, which is all it is now.
/// The guarantee is that one's: written to a temporary neighbour and renamed
/// into place, so a write that fails halfway — a full disk, a pulled USB
/// stick — cannot leave a truncated file where the artist's last good version
/// used to be.
///
/// **This paragraph used to say it was separate from [`save`] because the
/// autosave writes one archive to two places, and that is no longer why.** The
/// autosave streams its archive into the internal copy and *copies the finished
/// file* to the painter's own, so nothing holds a whole archive as a `&[u8]`
/// any more. What is left for this are the callers that genuinely have one:
/// [`crate::export`], which encodes a picture in one piece, and the keymap
/// writer. It is also why the temporary takes the *target's* extension rather
/// than `.ora` — an exported `sketch.png` becomes `sketch.png.saving` rather
/// than a name that would collide with the save of a document beside it.
pub fn write_encoded(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    write_with(path, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

/// Put whatever `fill` writes at `path`, whole or not at all.
///
/// **The one temp-and-rename a *document* goes through**, and the reason this
/// is the function rather than [`write_encoded`]: a streamed archive has no
/// `&[u8]` to hand over, and a second copy of "write beside it, then rename"
/// would be a second thing to get right. [`write_encoded`] is now one line of
/// this; [`save_from`] is the other caller, and [`crate::export`] and the
/// autosave reach it through the first.
///
/// Not the only one in Umber, and a bolder sentence here said it was.
/// `themelib` and `crate::palette` each keep their own, because both report a
/// different error type and neither writes anything a painter would lose a
/// day over. Both still push a fixed `.saving`, which is right for them: what
/// forced a unique name here is the *window*, and theirs is one `fs::write`.
///
/// If `fill` fails, or the rename does, the temporary is removed and the file
/// at `path` is untouched — which is the whole point, and is why `fill` writing
/// half an archive before it fails is a failure and not a corruption.
///
/// # The temporary's name has to be unique, and it did not used to
///
/// It was `<path>.saving`, one name for every writer of that file, and that
/// was safe for as long as the window between creating it and renaming it was
/// a single `std::fs::write`. It is not any more: `save_from` opens the
/// temporary and then performs every readback and every PNG encode inside it,
/// which on a large document is seconds to a minute rather than milliseconds.
///
/// Two writers of one document is not hypothetical. An explicit Save and an
/// autosave of the same document can overlap — `App::stop_autosave_of` can
/// only cancel a capture, and a `Task` already dispatched to the writer thread
/// is past that — and with one name the second `File::create` **truncates the
/// first's live temporary**, after which one rename moves a half-written
/// archive into place as the artist's document and the other fails. Silent
/// damage to the file the temp-and-rename exists to protect.
///
/// So the name carries the process and a counter. `.saving` stays *in* the
/// name so anything matching on it still does — `autosave::is_autosave_name`
/// is what that means in practice.
///
/// **The cost is worth stating precisely, because the shared name was
/// self-cleaning and this is not.** A temporary abandoned by a hard kill used
/// to be overwritten by the next save of that document and renamed away; now
/// the pid and the counter differ, so a second kill leaves a second file and a
/// third a third — one per interrupted save, indefinitely. Inside the autosave
/// folder `autosave::Reaper` clears them. Beside the artist's own document
/// nothing does, deliberately: `Reaper` is the only thing in Umber that deletes
/// a file on the user's behalf and its containment is careful for a reason, so
/// widening it to sweep somebody's documents folder is exactly the loosening
/// that rule refuses. A visible stray beside a good file beats a truncated file
/// over a good one, and that is the whole of the trade.
pub fn write_with(
    path: &Path,
    fill: impl FnOnce(&mut std::fs::File) -> Result<(), SaveError>,
) -> Result<(), SaveError> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let token = NEXT.fetch_add(1, Ordering::Relaxed);

    let mut temporary = path.to_path_buf().into_os_string();
    temporary.push(format!(".saving-{}-{token}", std::process::id()));
    let temporary = std::path::PathBuf::from(temporary);

    let written = (|| -> Result<(), SaveError> {
        let mut file = std::fs::File::create(&temporary)?;
        fill(&mut file)?;
        // A `std::fs::File` buffers nothing, so this is cheap; it is here so
        // that "everything `fill` wrote reached the operating system" is stated
        // rather than inferred from that fact.
        file.flush()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(e);
    }

    if let Err(e) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(SaveError::Io(e));
    }
    Ok(())
}

/// Build the archive in memory.
///
/// Separate from [`save`] so the round-trip tests can write and read without
/// touching a disk. **Not what a large document is saved by** — the whole
/// archive lands in one `Vec<u8>`, which for the reference canvas is every
/// layer's PNG at once and a doubling transient on top. [`save`] streams
/// instead and this is written in terms of the same body, so the two cannot
/// produce different bytes.
///
/// Every buffer has to be [`Canvas::Held`], for the reason [`save`]'s do.
pub fn encode(doc: &SaveDocument<'_>) -> Result<(Vec<u8>, Vec<SaveWarning>), SaveError> {
    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let warnings = write_archive(&mut zip, doc, &mut NoCanvases)?;
    Ok((zip.finish()?.into_inner(), warnings))
}

/// Which buffer a [`Canvas::Deferred`] stands for, so one resolver serves all
/// three.
#[derive(Clone, Copy)]
enum Wanted {
    Layer(usize),
    Mask(usize),
    Merged,
}

impl Wanted {
    /// What the buffer is called in a [`SaveError::WrongSize`], which has to
    /// read the same whether the bytes were held or fetched.
    fn describe(self, layers: &[SaveLayer<'_>]) -> String {
        match self {
            Self::Layer(i) => format!("pixels of layer “{}”", layers[i].name),
            Self::Mask(i) => format!("mask of layer “{}”", layers[i].name),
            Self::Merged => "flattened image".to_string(),
        }
    }
}

/// The bytes behind one [`Canvas`], borrowed where they were held and fetched
/// where they were not.
///
/// Checked against the canvas size **here**, at the point of use, so a fetched
/// buffer is held to exactly the bound a held one is — the alternative is a
/// source that hands back a short read and a file whose layers are silently
/// sheared.
///
/// A [`SaveError::NotSupplied`] coming *out* of a source is renamed on the way
/// through, because only this side knows what the layer is called: a source is
/// asked in indices and would otherwise refuse a document by a number the
/// artist has never seen. It is the same `what` a [`SaveError::WrongSize`] from
/// the line below carries, so the two adjacent failures cannot report the same
/// layer two different ways.
fn resolve<'c>(
    canvases: &'c mut dyn Canvases,
    canvas: Canvas<'c>,
    wanted: Wanted,
    expected: usize,
    layers: &[SaveLayer<'_>],
) -> Result<Cow<'c, [u8]>, SaveError> {
    let bytes = match canvas {
        Canvas::Held(bytes) => Cow::Borrowed(bytes),
        Canvas::Deferred => match wanted {
            Wanted::Layer(i) => canvases.layer(i),
            Wanted::Mask(i) => canvases.mask(i),
            Wanted::Merged => canvases.merged(),
        }
        .map_err(|e| match e {
            SaveError::NotSupplied { .. } => SaveError::NotSupplied {
                what: wanted.describe(layers),
            },
            other => other,
        })?,
    };
    if bytes.len() != expected {
        return Err(SaveError::WrongSize {
            what: wanted.describe(layers),
            found: bytes.len(),
            expected,
        });
    }
    Ok(bytes)
}

/// Write the whole archive into `zip`, asking `canvases` for anything the
/// document deferred.
///
/// The one body [`encode`], [`save`] and [`save_from`] share, which is what
/// makes "the streamed file is byte for byte the encoded one" structural rather
/// than a thing to keep true.
fn write_archive<W: Write + std::io::Seek>(
    zip: &mut ZipWriter<W>,
    doc: &SaveDocument<'_>,
    canvases: &mut dyn Canvases,
) -> Result<Vec<SaveWarning>, SaveError> {
    if doc.layers.is_empty() {
        return Err(SaveError::Empty);
    }
    if doc.layers.len() > LayerStack::MAX {
        return Err(SaveError::TooManyLayers {
            found: doc.layers.len(),
            max: LayerStack::MAX,
        });
    }
    if doc.layers.iter().all(|l| l.folder) {
        return Err(SaveError::Empty);
    }
    let expected = doc.size.x as usize * doc.size.y as usize * 4;

    // Every buffer the caller is *holding*, measured before a byte is written.
    // A deferred one cannot be measured here without fetching it, which would
    // be the whole stack in memory again — so it is measured in `resolve`, at
    // the point of use, and the refusal reads identically either way. What this
    // loop keeps is that a caller who handed over the bytes still learns their
    // shape is wrong before anything has been written at all.
    for layer in doc.layers.iter().filter(|l| !l.folder) {
        if let Canvas::Held(pixels) = layer.pixels
            && pixels.len() != expected
        {
            return Err(SaveError::WrongSize {
                what: format!("pixels of layer “{}”", layer.name),
                found: pixels.len(),
                expected,
            });
        }
        if let Some(Canvas::Held(mask)) = layer.mask
            && mask.len() != expected
        {
            return Err(SaveError::WrongSize {
                what: format!("mask of layer “{}”", layer.name),
                found: mask.len(),
                expected,
            });
        }
    }
    if let Canvas::Held(merged) = doc.merged
        && merged.len() != expected
    {
        return Err(SaveError::WrongSize {
            what: "flattened image".to_string(),
            found: merged.len(),
            expected,
        });
    }

    // The specification requires `mimetype` first and uncompressed, and real
    // readers — Umber's own included — check it.
    zip.start_file("mimetype", stored())?;
    zip.write_all(b"image/openraster")?;

    let mut warnings = Vec::new();
    let mut entries = Vec::with_capacity(doc.layers.len());
    // Layers carrying an effect somebody can see, counted for the one warning
    // raised about all of them — see `SaveWarning::EffectsNotPortable`.
    let mut effected = 0usize;

    // Top first, which is the order `stack.xml` wants. The `src` numbering
    // follows the same order so a file listing reads the way the layers panel
    // does.
    //
    // A folder writes no PNG at all, so the numbering has gaps where one sits.
    // That is deliberate: `src` is a name, and numbering the *layers*
    // consecutively instead would make the number stop matching the entry it
    // belongs to, which is the one thing the numbering is for.
    for (i, layer) in doc.layers.iter().rev().enumerate() {
        // `doc.active` indexes the bottom-first stack; this loop runs top first.
        // `at` is that same bottom-first index, which is what a `Canvases` is
        // asked in — the caller numbered the stack, so nothing is reversed on
        // the way out.
        let at = doc.layers.len() - 1 - i;
        let selected = at == doc.active;
        if layer.folder {
            entries.push(Entry {
                depth: layer.depth,
                folder: true,
                xml: folder_xml(layer, selected),
            });
            continue;
        }
        let src = format!("data/layer{i:03}.png");
        // Scoped so the canvas-sized buffer is gone before the PNG is written:
        // `trim` owns what it produces, so past this brace the only thing alive
        // is the layer's own content rectangle. For a fetched buffer that is
        // the difference between one canvas resident and one per layer.
        let placed = {
            let pixels = resolve(
                canvases,
                layer.pixels,
                Wanted::Layer(at),
                expected,
                doc.layers,
            )?;
            trim(&pixels, doc.size)
        };
        zip.start_file(&src, stored())?;
        write_png(zip, placed.size, &placed.pixels)?;

        // The mask, as a greyscale PNG of the slice's red channel, under
        // `umber/` where no other reader will look. Never trimmed: a mask is
        // canvas-sized by definition and its *transparent* region is a region
        // that hides, so a bounding box of the non-zero pixels would come back
        // as a mask that revealed everything outside it.
        //
        // The bytes go in raw, exactly as the history's patches do: they are
        // coverage, not colour, and the straight-alpha conversion the layer
        // images get exists so other applications can read them. Nothing else
        // reads these.
        let mask_src = match layer.mask {
            Some(mask) => {
                let src = mask_src(i);
                // Scoped like the layer's own, and for the same reason: the
                // canvas-sized slice is gone by the closing brace and only the
                // one-byte-a-pixel coverage survives into the encoder.
                let grey: Vec<u8> = {
                    let mask = resolve(canvases, mask, Wanted::Mask(at), expected, doc.layers)?;
                    mask.chunks_exact(4).map(|px| px[0]).collect()
                };
                zip.start_file(&src, stored())?;
                write_png_grey(zip, doc.size, &grey)?;
                Some(src)
            }
            None => None,
        };

        // The text record, fingerprinted against the layer image **this save is
        // writing** rather than against anything the caller holds. That is what
        // makes the fingerprint unable to be stale, and it is why the record's
        // own type carries none: see `textobj`'s module docs.
        //
        // A record too large for `textobj::MAX_RECORD_BYTES` is not written and
        // is *named*, because a text layer that silently stopped being editable
        // at the next open would be a loss nobody was told about. The picture is
        // whole either way — the pixels are the layer's ordinary PNG.
        let text_entry = match layer.text {
            Some(text) => {
                let print = crate::textobj::Fingerprint::of(
                    PixelRect {
                        x: placed.at.0,
                        y: placed.at.1,
                        width: placed.size.x,
                        height: placed.size.y,
                    },
                    &placed.pixels,
                );
                match text.to_json(&print) {
                    Ok(json) => {
                        let src = text_src(i);
                        // Deflated: this is JSON, which is text, and the one
                        // entry in the archive that compresses well.
                        zip.start_file(&src, deflated())?;
                        zip.write_all(&json)?;
                        Some(src)
                    }
                    Err(why) => {
                        warnings.push(SaveWarning::TextNotRecorded {
                            layer: layer.name.to_string(),
                            why,
                        });
                        None
                    }
                }
            }
            None => None,
        };

        // The effects, as RON under `umber/effects/`, where no other reader
        // will look. `docs/layer-effects.md` §8.1 and [`EFFECTS_ATTR`].
        //
        // Written for a layer and never for a folder, which is the same
        // `!l.folder` filter `required_version` applies — so the entries in the
        // archive and the revision the file declares cannot disagree about what
        // it holds. A folder can carry none anyway: `plan_set_effect` refuses
        // one, because there is no coverage to derive it from until group
        // compositing lands.
        let effects_src = match layer.effects.is_empty() {
            true => None,
            false => {
                let src = effects_src(i);
                zip.start_file(&src, deflated())?;
                zip.write_all(encode_effects(layer.effects)?.as_bytes())?;
                // Counted here and reported once below. Only where something
                // is switched *on*: a layer whose effects are all disabled
                // draws plain in Umber as well, so naming it would report a
                // loss that did not happen.
                if layer.effects.iter().any(|e| e.enabled) {
                    effected += 1;
                }
                Some(src)
            }
        };

        let (op, exact) = composite_op(layer.blend);
        if !exact {
            warnings.push(SaveWarning::BlendApproximated {
                layer: layer.name.to_string(),
                mode: layer.blend.label(),
                used: op,
            });
        }
        entries.push(Entry {
            depth: layer.depth,
            folder: false,
            xml: layer_xml(
                layer,
                &src,
                placed.at,
                op,
                exact,
                selected,
                mask_src.as_deref(),
                text_entry.as_deref(),
                effects_src.as_deref(),
            ),
        });
    }

    // One sentence for the document, however many layers carry an effect. See
    // `SaveWarning::EffectsNotPortable` for why this is not per layer.
    if effected > 0 {
        warnings.push(SaveWarning::EffectsNotPortable { layers: effected });
    }

    // The background goes in last, so it is the bottom of the stack — and it is
    // a real layer with real pixels, because every application but this one
    // would otherwise open the document on transparency. See the module docs.
    if let Some(colour) = doc.background.colour() {
        zip.start_file(BACKGROUND_SRC, stored())?;
        write_png(zip, doc.size, &solid(doc.size, colour))?;
        entries.push(Entry {
            // At the top level whatever the stack above it does: the background
            // is under everything, so it can be inside nothing.
            depth: 0,
            folder: false,
            xml: background_xml(colour),
        });
    }

    // The undo history, if the caller kept one. Under `umber/`, which every
    // other OpenRaster reader walks straight past — and written before
    // `stack.xml`, because the attribute that points at it must only appear
    // when something actually went in.
    // The names as `stack.xml` will hold them, not as the editor holds them —
    // the manifest fingerprints the stack the reader will rebuild.
    let names: Vec<String> = doc.layers.iter().map(|l| clean_name(l.name)).collect();
    let saved_history = match &doc.history {
        Some(h) if !h.is_empty() => history::write(zip, doc.size, &names, h)?,
        _ => false,
    };

    zip.start_file("stack.xml", deflated())?;
    zip.write_all(
        stack_xml(
            doc.size,
            doc.dpi,
            required_version(doc.layers),
            saved_history,
            &entries,
        )
        .as_bytes(),
    )?;

    // Both are required of a conforming writer, and both are what gives a file
    // manager something to show — and both come off the *same* flattened image,
    // which is why it is fetched once here and held across the pair rather than
    // asked for twice.
    let merged = resolve(canvases, doc.merged, Wanted::Merged, expected, doc.layers)?;
    zip.start_file("mergedimage.png", stored())?;
    write_png(zip, doc.size, &merged)?;

    let (thumb_size, thumb) = thumbnail(&merged, doc.size);
    drop(merged);
    zip.start_file("Thumbnails/thumbnail.png", stored())?;
    write_png(zip, thumb_size, &thumb)?;

    Ok(warnings)
}

/// Umber's mode as an OpenRaster `composite-op`, and whether it is exact.
///
/// See [`BLEND_ATTR`] for why Add is the one that is not.
pub fn composite_op(mode: BlendMode) -> (&'static str, bool) {
    match mode {
        BlendMode::Normal => ("svg:src-over", true),
        BlendMode::Multiply => ("svg:multiply", true),
        BlendMode::Screen => ("svg:screen", true),
        BlendMode::Overlay => ("svg:overlay", true),
        BlendMode::Add => ("svg:plus", false),
        // **The one mode with no SVG *name* that is nonetheless exact.**
        // `svg:plus` is Porter-Duff addition on premultiplied colour, which is
        // precisely what Add (Glow) is — so unlike every other entry below, no
        // fidelity is lost by writing it and another reader draws it correctly.
        // Add above shares the spelling and is *not* exact, which is why both
        // still carry `umber-blend` to tell them apart on the way back in.
        BlendMode::AddGlow => ("svg:plus", true),
        // The rest of the W3C separable set, which OpenRaster names exactly.
        BlendMode::Darken => ("svg:darken", true),
        BlendMode::Lighten => ("svg:lighten", true),
        BlendMode::ColorDodge => ("svg:color-dodge", true),
        BlendMode::ColorBurn => ("svg:color-burn", true),
        BlendMode::HardLight => ("svg:hard-light", true),
        BlendMode::SoftLight => ("svg:soft-light", true),
        BlendMode::Difference => ("svg:difference", true),
        BlendMode::Exclusion => ("svg:exclusion", true),
        // And the non-separable four, which it also names.
        BlendMode::Hue => ("svg:hue", true),
        BlendMode::Saturation => ("svg:saturation", true),
        BlendMode::Color => ("svg:color", true),
        BlendMode::Luminosity => ("svg:luminosity", true),

        // **Photoshop's, not SVG's.** OpenRaster has no name for any of these,
        // so each is written as the nearest thing another reader can draw and
        // is marked inexact — which is what makes the writer add `umber-blend`
        // beside it, so Umber's own round trip is still exact and only a
        // foreign reader sees the approximation. Exactly the arrangement Add
        // has had since `svg:plus`.
        BlendMode::LinearBurn => ("svg:multiply", false),
        BlendMode::VividLight => ("svg:hard-light", false),
        BlendMode::LinearLight => ("svg:hard-light", false),
        BlendMode::PinLight => ("svg:hard-light", false),
        BlendMode::Subtract => ("svg:difference", false),
        BlendMode::Divide => ("svg:color-dodge", false),
    }
}

/// Stable name for [`BLEND_ATTR`]. The debug spelling, so it cannot drift out
/// of step with the enum the way a second hand-written table would.
///
/// What "stable" rests on, since the derive cannot enforce it: the variant's
/// **name in the source is the identifier on disk**. Adding a mode is safe and
/// is what the derive was chosen for; *renaming* one silently changes the file
/// format. The damage is milder here than for `history::kind_id` and is worth
/// stating exactly rather than dramatically. The attribute is written only
/// where [`composite_op`] reports `!exact`, which is Add alone, so only Add
/// layers carry one; on a rename [`blend_from_id`] answers `None` and the
/// reader falls back to `composite-op`, which for such a layer is
/// `svg:plus` — and `blend::nearest` reads that back as Add. **The mode and
/// the pixels are unchanged.** What is lost is the fidelity: the import raises
/// a spurious `BlendApproximated` warning, which is precisely the regression
/// [`BLEND_ATTR`] exists to prevent, reintroduced on every Add layer in every
/// document already on disk.
///
/// **The ORA file is not the only thing this spelling reaches, and the other
/// one is worse** — which is worth saying, having claimed to state the cost
/// exactly. [`BlendMode`] derives `Serialize`, and `Brush::blend` carries one
/// into `brushes.ron`, where serde's variant name is that same source
/// identifier. `preset::parse` is a single `ron::from_str(…)?` over the whole
/// file, so an unrecognised variant is not a per-brush fallback but a hard
/// error: a user who has ever set a brush to Add loses their entire saved
/// library rather than gaining a warning. Two consumers of one identifier,
/// and only one of them is this module's.
///
/// `the_names_written_into_the_blend_attribute_are_
/// these_exact_strings` is what catches it; the round trip against
/// `blend_from_id` cannot, because both sides move together.
pub fn blend_id(mode: BlendMode) -> String {
    format!("{mode:?}")
}

/// Inverse of [`blend_id`]. An unrecognised name comes from a version that has
/// modes this one does not, and yields `None` so the reader falls back to the
/// `composite-op` every ORA also carries.
pub fn blend_from_id(id: &str) -> Option<BlendMode> {
    BlendMode::ALL.into_iter().find(|m| blend_id(*m) == id)
}

/// A layer's effects as the archive holds them: a RON sequence, in composite
/// order.
///
/// RON rather than JSON, unlike the history's manifest, and the difference is
/// not a preference. [`Effect`] already derives serde and its serialised
/// spelling is *pinned as literal text* in `effect`'s own tests — the field
/// names, the variant names and the colour's shape — because a derived spelling
/// that reaches a file is a format rather than a name. `brushes.ron` is where
/// that spelling is already read and written, so writing it the same way here
/// means one form to keep pinned instead of two. It is also the only form that
/// lets `Effect`'s per-field `#[serde(default)]`s do their job: a parameter
/// added in a later build has to load out of a file written before it existed.
///
/// Pretty rather than compact, and `struct_names(false)` exactly as
/// `preset::write` uses: the whole point of a ZIP of readable parts is that
/// somebody can unzip one and look. It costs a few hundred bytes in a deflated
/// entry.
///
/// **`new_line("\n")` is where this parts company with `preset::write`, and it
/// is not a preference.** `PrettyConfig::new()` takes the *platform's* line
/// ending, so the same document saved on Windows and on Linux differed byte for
/// byte inside `umber/effects/` — a document travels and `brushes.ron` does
/// not, which is the whole of why the precedent does not carry. Nothing reads
/// the bytes back for comparison today, so this was invisible; it would have
/// been found by whoever first diffed two `.ora`s or checked one into version
/// control.
fn encode_effects(effects: &[Effect]) -> Result<String, SaveError> {
    let config = ron::ser::PrettyConfig::new()
        .struct_names(false)
        .new_line("\n");
    ron::ser::to_string_pretty(&effects, config)
        .map_err(|e| SaveError::Io(std::io::Error::other(e)))
}

/// The background colour as [`BACKGROUND_ATTR`] spells it: `#rrggbb`, sRGB.
///
/// sRGB bytes rather than linear floats because that is the only form in which
/// the value is exactly what a colour picker showed, and because it is the
/// spelling anybody opening `stack.xml` in a text editor can read.
pub fn background_id(colour: Color) -> String {
    let [r, g, b, _] = colour.to_srgb_u8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Inverse of [`background_id`].
///
/// `None` for anything unrecognised, which leaves the reader treating the layer
/// as an ordinary layer — the picture is still right, and a colour guessed from
/// a malformed attribute would not be.
pub fn background_from_id(id: &str) -> Option<Color> {
    let hex = id.strip_prefix('#')?;
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(Color::from_srgb_u8(byte(0)?, byte(2)?, byte(4)?, 255))
}

// --- the XML ---------------------------------------------------------------

/// One line of `stack.xml` on its way out, with what it takes to nest it.
///
/// The depth travels beside the text rather than being read back out of it,
/// because closing a `<stack>` is a decision about the *next* entry and a
/// string has nowhere to put that.
struct Entry {
    depth: u8,
    /// This entry opens a `<stack>` that later entries go inside.
    folder: bool,
    xml: String,
}

fn stack_xml(size: UVec2, dpi: f32, version: u32, saved_history: bool, layers: &[Entry]) -> String {
    // `xres`/`yres` are OpenRaster's own, in whole pixels per inch, and every
    // reader that cares about print already looks for them — which is exactly
    // why there is no `umber-dpi` beside them. Umber has one resolution rather
    // than one per axis, so both get the same number.
    let res = crate::document::sane_dpi(dpi).round() as u32;
    // Named rather than spelled as a flag, so a later revision can move the
    // manifest without every existing file becoming ambiguous.
    let history = if saved_history {
        format!(" {HISTORY_ATTR}=\"{}\"", history::MANIFEST)
    } else {
        String::new()
    };
    let mut out = String::from("<?xml version='1.0' encoding='UTF-8'?>\n");
    out.push_str(&format!(
        "<image w=\"{}\" h=\"{}\" xres=\"{res}\" yres=\"{res}\" \
         version=\"{ORA_VERSION}\" {VERSION_ATTR}=\"{version}\"{history}>\n \
         <stack>\n",
        size.x, size.y
    ));
    // A folder is a nested `<stack>`, which is the only thing in this file that
    // is not one self-closing line. The entries arrive top first with a depth
    // each, and a folder's contents are exactly the entries after it that are
    // deeper — so the close tags are emitted whenever the depth comes back
    // down, and whatever is still open is closed at the end.
    //
    // This is baseline OpenRaster, not an `umber-` extension: it is the nesting
    // GIMP, Krita and MyPaint all write, and the reason folders did **not**
    // move `VERSION_ATTR`. A reader that does not keep the nesting — an older
    // Umber, or another application — folds the group's visibility into the
    // layers inside it and draws the identical picture, because a pass-through
    // folder *is* its contents composited in place.
    let mut open: Vec<u8> = Vec::new();
    for entry in layers {
        while open.last().is_some_and(|d| *d >= entry.depth) {
            open.pop();
            // At the indent the opening tag was written at, which is one level
            // in from however many folders remain open around it.
            out.push_str(&indent(open.len() + 1));
            out.push_str("</stack>\n");
        }
        out.push_str(&indent(open.len() + 1));
        out.push_str(&entry.xml);
        out.push('\n');
        if entry.folder {
            open.push(entry.depth);
        }
    }
    while open.pop().is_some() {
        out.push_str(&indent(open.len() + 1));
        out.push_str("</stack>\n");
    }
    out.push_str(" </stack>\n</image>\n");
    out
}

/// Indentation for a `stack.xml` line, so a nested file is readable in an
/// editor — which is half of why the format was chosen.
fn indent(levels: usize) -> String {
    " ".repeat(levels + 1)
}

#[allow(clippy::too_many_arguments)]
fn layer_xml(
    layer: &SaveLayer<'_>,
    src: &str,
    at: (u32, u32),
    op: &str,
    exact: bool,
    selected: bool,
    mask: Option<&str>,
    text: Option<&str>,
    effects: Option<&str>,
) -> String {
    let mut out = format!(
        "<layer name=\"{}\" src=\"{src}\" x=\"{}\" y=\"{}\" opacity=\"{:.4}\" \
         visibility=\"{}\" composite-op=\"{op}\"",
        attribute(layer.name),
        at.0,
        at.1,
        layer.opacity.clamp(0.0, 1.0),
        if layer.visible { "visible" } else { "hidden" },
    );
    if !exact {
        out.push_str(&format!(" {BLEND_ATTR}=\"{}\"", blend_id(layer.blend)));
    }
    if selected {
        out.push_str(&format!(" {SELECTED_ATTR}=\"true\""));
    }
    if let Some(mask) = mask {
        out.push_str(&format!(" {MASK_ATTR}=\"{mask}\""));
    }
    // The entry's *name*, never the record: see [`TEXT_ATTR`]. A path Umber
    // built, so there is nothing in it to escape. The same is true of the
    // effects entry beside it.
    if let Some(text) = text {
        out.push_str(&format!(" {TEXT_ATTR}=\"{text}\""));
    }
    if let Some(effects) = effects {
        out.push_str(&format!(" {EFFECTS_ATTR}=\"{effects}\""));
    }
    // Written only when set, so a file from a document nobody has flagged
    // anything on is byte for byte the file this module always wrote.
    for (attr, on) in [
        (CLIP_ATTR, layer.clipped),
        (LOCK_ATTR, layer.locked),
        (LINK_ATTR, layer.link.is_some()),
    ] {
        if on {
            out.push_str(&format!(" {attr}=\"true\""));
        }
    }
    // Beside the old flag rather than instead of it — see [`LINK_GROUP_ATTR`].
    if let Some(group) = layer.link {
        out.push_str(&format!(" {LINK_GROUP_ATTR}=\"{group}\""));
    }
    out.push_str("/>");
    out
}

/// The opening tag of the nested `<stack>` a folder becomes.
///
/// **No `opacity` and no `composite-op`.** Both are legal on an ORA `<stack>`
/// and both are deliberately absent, because a folder in this build is
/// pass-through: it has no opacity of its own to write. Writing `opacity="1"`
/// would be harmless and writing anything else would not — a group opacity is
/// the one thing a reader that flattens the nesting away *cannot* reproduce,
/// since a folder at 50% over two overlapping children is not the same picture
/// as two children at 50% each. That is the whole reason folders did not move
/// [`VERSION`], and it is why [`required_version`] has nothing to say about
/// them.
///
/// **`isolation="auto"` is written, and it is not optional.** ORA's `isolation`
/// defaults to `isolate`, so a `<stack>` that says nothing declares itself an
/// isolated group — the opposite of every folder in this build. The two agree
/// for as long as every child composites `svg:src-over`, which is why this went
/// unnoticed; they diverge the moment one does not, because a child inside an
/// isolated group blends against its siblings alone while a pass-through child
/// blends against the whole backdrop beneath the folder. So a document with a
/// Multiply layer inside a folder came out of Umber looking one way and opened
/// in a conforming reader looking another. Saying `auto` costs nothing and
/// needs no [`VERSION`] bump — it makes the file mean what Umber always
/// rendered — and it is what the group-compositing work will vary when a folder
/// finally has an opacity of its own to isolate for.
///
/// `umber-selected` can land here: a folder is selectable, and a document
/// saved with one in hand must reopen with it in hand.
fn folder_xml(layer: &SaveLayer<'_>, selected: bool) -> String {
    let mut out = format!(
        "<stack name=\"{}\" visibility=\"{}\" isolation=\"auto\"",
        attribute(layer.name),
        if layer.visible { "visible" } else { "hidden" },
    );
    if selected {
        out.push_str(&format!(" {SELECTED_ATTR}=\"true\""));
    }
    // A lock reaches what is inside the folder, so it is worth keeping; it
    // changes no pixel, which is why — like a layer's — it did not move
    // `VERSION` either.
    if layer.locked {
        out.push_str(&format!(" {LOCK_ATTR}=\"true\""));
    }
    // **A folder can be linked, so its group has to be written.** `targets`
    // includes a ticked folder and `LayerStack::moving_with` is built for
    // exactly that, so a set of "these travel together" can name one. Dropping
    // it here lost the folder out of the group silently — and worse, brought
    // the *other* member back alone, which `dissolve_lone_groups` exists to
    // make impossible: a chain in one colour meaning "moves together with
    // nothing", holding a number `free_group` could then never hand back.
    if layer.link.is_some() {
        out.push_str(&format!(" {LINK_ATTR}=\"true\""));
    }
    if let Some(group) = layer.link {
        out.push_str(&format!(" {LINK_GROUP_ATTR}=\"{group}\""));
    }
    out.push('>');
    out
}

/// The `<layer>` that carries the document background.
///
/// An ordinary, fully opaque, canvas-sized layer as far as anything else is
/// concerned — which is the point — plus [`BACKGROUND_ATTR`], which is how
/// Umber's own reader knows to turn it back into a document property instead of
/// handing the painter a layer they never made.
///
/// Never selected: the attribute says it is not a layer, so `umber-selected`
/// on it would contradict that, and a reader that ignored the attribute would
/// open the document with the background layer active.
fn background_xml(colour: Color) -> String {
    format!(
        "<layer name=\"{BACKGROUND_NAME}\" src=\"{BACKGROUND_SRC}\" x=\"0\" y=\"0\" \
         opacity=\"1.0000\" visibility=\"visible\" composite-op=\"svg:src-over\" \
         {BACKGROUND_ATTR}=\"{}\"/>",
        background_id(colour)
    )
}

/// Escape a layer name for an attribute value.
fn attribute(raw: &str) -> String {
    quick_xml::escape::escape(clean_name(raw)).into_owned()
}

/// A layer name as the file will hold it.
///
/// Control characters are dropped rather than escaped: they are not legal in
/// XML 1.0 at all, so a name carrying one — pasted from somewhere odd — would
/// make the file unreadable by every parser including Umber's.
///
/// Public within the module because the history manifest fingerprints the stack
/// by name, and it has to record the names the *file* will come back with. Left
/// to `attribute` alone, a document with an odd character in a layer name would
/// write a fingerprint that could never match on the way in, and silently lose
/// its history every time it was saved.
pub(crate) fn clean_name(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect()
}

// --- the pixels ------------------------------------------------------------

/// A layer's pixels reduced to the rectangle that actually holds anything.
struct Placed {
    size: UVec2,
    at: (u32, u32),
    /// Straight-alpha sRGB, `size.x * size.y * 4` bytes.
    pixels: Vec<u8>,
}

/// A canvas of one opaque colour, straight-alpha sRGB.
///
/// Not put through [`trim`]: the background covers the whole canvas by
/// definition, so scanning it for a bounding box would be sixteen megabytes of
/// reading to conclude "all of it". The bytes are already straight alpha —
/// there is no premultiply to undo when alpha is 1 everywhere — so the sRGB
/// conversion `trim` performs is skipped too.
fn solid(size: UVec2, colour: Color) -> Vec<u8> {
    let px = colour.with_alpha(1.0).to_srgb_u8();
    px.iter()
        .copied()
        .cycle()
        .take(size.x as usize * size.y as usize * 4)
        .collect()
}

/// Crop a canvas-sized layer to its non-transparent bounding box.
///
/// ORA stores each layer at its own rectangle with an offset, and Umber's
/// reader already places them back. It is worth doing rather than writing the
/// whole canvas every time: a sketch is mostly empty, and the difference on a
/// 2048² document with eight layers is a file of a few hundred kilobytes
/// instead of a few megabytes — plus seven-eighths less PNG encoding, which is
/// what the save actually spends its time on.
fn trim(pixels: &[u8], canvas: UVec2) -> Placed {
    let (w, h) = (canvas.x as usize, canvas.y as usize);
    let mut min = (w, h);
    let mut max = (0usize, 0usize);
    for y in 0..h {
        let row = &pixels[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            if row[x * 4 + 3] != 0 {
                min = (min.0.min(x), min.1.min(y));
                max = (max.0.max(x + 1), max.1.max(y + 1));
            }
        }
    }

    // A layer nobody has painted on yet. A zero-sized PNG is not a PNG, so it
    // becomes one transparent pixel — which is also what makes an empty layer
    // survive a round trip rather than vanishing from the stack.
    if min.0 >= max.0 {
        return Placed {
            size: UVec2::ONE,
            at: (0, 0),
            pixels: vec![0; 4],
        };
    }

    let (tw, th) = (max.0 - min.0, max.1 - min.1);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in min.1..max.1 {
        let start = (y * w + min.0) * 4;
        out.extend_from_slice(&pixels[start..start + tw * 4]);
    }
    srgb::decode_buffer(&mut out);

    Placed {
        size: UVec2::new(tw as u32, th as u32),
        at: (min.0 as u32, min.1 as u32),
        pixels: out,
    }
}

/// Box-average the flattened image down to something a file browser can show.
///
/// Averaging is done on the sRGB bytes deliberately. A thumbnail is not a
/// colour operation the engine will ever composite with; it is a preview, and
/// the specification's readers show it beside PNGs from every other
/// application, all of which do the same thing.
fn thumbnail(merged: &[u8], canvas: UVec2) -> (UVec2, Vec<u8>) {
    let scale = (canvas.x.max(canvas.y) as f32 / THUMBNAIL_MAX as f32).max(1.0);
    let tw = ((canvas.x as f32 / scale).round() as u32).max(1);
    let th = ((canvas.y as f32 / scale).round() as u32).max(1);
    if tw == canvas.x && th == canvas.y {
        return (canvas, merged.to_vec());
    }

    let mut out = vec![0u8; tw as usize * th as usize * 4];
    for ty in 0..th as usize {
        // Source band for this output row, at least one pixel tall.
        let y0 = ty * canvas.y as usize / th as usize;
        let y1 = (((ty + 1) * canvas.y as usize) / th as usize).max(y0 + 1);
        for tx in 0..tw as usize {
            let x0 = tx * canvas.x as usize / tw as usize;
            let x1 = (((tx + 1) * canvas.x as usize) / tw as usize).max(x0 + 1);

            let mut sum = [0u32; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let px = (y * canvas.x as usize + x) * 4;
                    for c in 0..4 {
                        sum[c] += merged[px + c] as u32;
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u32;
            let dst = (ty * tw as usize + tx) * 4;
            for c in 0..4 {
                out[dst + c] = (sum[c] / n) as u8;
            }
        }
    }
    (UVec2::new(tw, th), out)
}

/// One byte per pixel, for a layer mask.
///
/// Greyscale rather than RGBA because a mask is one channel and writing four
/// would quadruple the entry for three copies of the same byte. `decode_png`
/// widens it back to `(g, g, g, 255)` on the way in, which is exactly the form
/// a mask slice holds — so the round trip is byte for byte and needs no
/// conversion at either end.
/// A PNG in memory.
///
/// The one caller left is [`history::write`], and it is not an oversight: the
/// file's own byte budget is stated over *encoded* patches, so it has to know
/// how large one came out before it can decide whether to keep it — and it
/// walks the timeline newest-first to make that decision while writing the
/// entries oldest-first. What it accumulates is bounded by
/// [`history::BUDGET_BYTES`], where a layer's PNG is bounded only by the canvas.
fn encode_png(size: UVec2, rgba: &[u8]) -> Result<Vec<u8>, SaveError> {
    let mut out = Vec::new();
    write_png(&mut out, size, rgba)?;
    Ok(out)
}

/// A greyscale PNG, written straight into `sink`.
///
/// `sink` rather than a returned `Vec<u8>` so a mask's PNG never exists beside
/// the archive it is going into. The bytes are identical either way — the
/// encoder makes the same calls in the same order — which is what lets
/// `a_streamed_save_is_byte_for_byte_the_archive_encode_builds` hold.
fn write_png_grey(sink: &mut impl Write, size: UVec2, grey: &[u8]) -> Result<(), SaveError> {
    let mut encoder = png::Encoder::new(sink, size.x, size.y);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    finish_png(encoder, grey)
}

fn write_png(sink: &mut impl Write, size: UVec2, rgba: &[u8]) -> Result<(), SaveError> {
    let mut encoder = png::Encoder::new(sink, size.x, size.y);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    // ORA specifies sRGB, and so does Umber's engine at its edges.
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    // `Fast` is fdeflate, which is tuned for PNG and beats libpng's default
    // ratio anyway. A save already blocks on eight GPU readbacks; spending
    // seconds more squeezing the last few per cent out of a working file — one
    // that will be written again in a minute — is the wrong trade. Exports are
    // a different question, and are a different function.
    encoder.set_compression(png::Compression::Fast);
    finish_png(encoder, rgba)
}

/// Write the header, the image and the trailer.
///
/// `png::Writer` writes `IEND` from its `Drop` and throws away whatever that
/// says; this calls `finish` so the trailer's own failure is a value rather
/// than a silence.
///
/// **It is not what stands between a full disk and a truncated archive, and an
/// earlier version of this comment said it was.** Two things make that claim
/// false. A sink that refuses the trailer refuses the next entry's local
/// header too, and the last PNG is followed by the ZIP's own central
/// directory — so the save is refused either way. And on the streaming path
/// [`Watched`] absorbs an **I/O** failure before any of this sees one. The
/// call is kept for what it costs, which is nothing; it is not load-bearing
/// today and no test can make it look as though it is.
///
/// "Cannot fail at all" would be the over-correction, and it is wrong: the sink
/// here is a `ZipWriter`, whose own `Write` refuses an entry that crosses
/// `ZIP64_BYTES_THR` because `stored()` and `deflated()` never set
/// `large_file`. That error is raised *above* `Watched` and reaches this. It is
/// unreachable for any canvas anybody has, and not for any canvas
/// `Document::MAX_EDGE` permits — a 32768² layer is 4.3 GB raw, and noisy paint
/// can store past `u32::MAX` — at which point a save fails with zip's own
/// wording wrapped as [`SaveError::Io`]. That is the second thing to build on
/// the very large canvas, after refusing one the card cannot hold.
fn finish_png<W: Write>(encoder: png::Encoder<'_, W>, data: &[u8]) -> Result<(), SaveError> {
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(data).and(w.finish()))
        .map_err(|e| SaveError::Io(std::io::Error::other(e)))
}

// --- the container ---------------------------------------------------------

/// Entries that are already compressed.
///
/// PNG data is deflated by definition, so deflating it again in the ZIP costs
/// time and gains nothing. `mimetype` must be stored, by the specification.
fn stored() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
}

fn deflated() -> SimpleFileOptions {
    SimpleFileOptions::default().compression_method(CompressionMethod::Deflated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docimport::{self, ImportError};
    use crate::document::Document;
    use crate::effect::OutlinePosition;

    /// Layer-texture bytes: a solid colour over the whole canvas.
    fn solid(size: UVec2, px: [u8; 4]) -> Vec<u8> {
        px.iter()
            .copied()
            .cycle()
            .take(size.x as usize * size.y as usize * 4)
            .collect()
    }

    fn empty(size: UVec2) -> Vec<u8> {
        vec![0; size.x as usize * size.y as usize * 4]
    }

    fn layer<'a>(name: &'a str, pixels: &'a [u8]) -> SaveLayer<'a> {
        SaveLayer::new(name, BlendMode::Normal, pixels)
    }

    // --- text layers --------------------------------------------------------

    fn text_object() -> crate::textobj::TextObject {
        use crate::text::{Align, TextBlock};
        use crate::textobj::{Placement, TextFace, TextObject};
        TextObject::new(
            TextBlock {
                text: "A caption <&> a second line".into(),
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
            Color::from_srgb_u8(10, 20, 30, 255),
            Placement::identity(PixelRect {
                x: 0,
                y: 0,
                width: 4,
                height: 4,
            }),
        )
    }

    /// A one-text-layer document, and the text record inside it.
    fn text_document(size: UVec2, px: &[u8]) -> Vec<u8> {
        let text = text_object();
        let layers = vec![SaveLayer {
            text: Some(&text),
            ..layer("Caption", px)
        }];
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(px),
            history: None,
        })
        .expect("encode");
        assert!(warnings.is_empty(), "{warnings:?}");
        bytes
    }

    /// What set a layer comes back, and the string comes back **out of its own
    /// archive entry** rather than out of `stack.xml`.
    ///
    /// The prose in the fixture carries `<`, `&` and `>` deliberately: an
    /// attribute would have had to escape all three, in the one file every other
    /// OpenRaster reader parses, and it would have been the largest thing in it.
    #[test]
    fn a_text_layer_comes_back_as_text() {
        let size = UVec2::new(4, 4);
        let px = solid(size, [10, 20, 30, 255]);
        let bytes = text_document(size, &px);

        let xml = read_stack_xml(&bytes);
        assert!(
            xml.contains(&format!("{TEXT_ATTR}=\"{}\"", text_src(0))),
            "{xml}"
        );
        assert!(
            !xml.contains("A caption"),
            "the prose must not be in stack.xml:\n{xml}"
        );

        let doc = docimport::read_openraster(&bytes).expect("read back");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        let text = doc.layers[0].text.as_deref().expect("the record came back");
        assert_eq!(*text, text_object());

        // And the stack it opens as holds it, with painting refused on it.
        let opened = doc.open();
        assert!(opened.stack.text_at(0).is_some());
        assert_eq!(
            opened.stack.refusal_at(0, crate::layer::EditTarget::Layer),
            Some(crate::layer::EditRefusal::Text)
        );
    }

    /// **`umber-version` did not move for text**, and this is the test the claim
    /// rests on.
    ///
    /// A record is an attribute an older build ignores beside a layer PNG it
    /// decodes, so it opens the document and shows the identical picture. That is
    /// "plainer", not "wrong", which is the line `VERSION` is drawn on — and the
    /// fingerprint is what keeps it true in the case that argument nearly missed,
    /// which `a_text_layer_painted_on_by_an_older_build_opens_as_paint` is.
    #[test]
    fn a_document_of_text_layers_still_declares_the_revision_it_needs() {
        let size = UVec2::new(4, 4);
        let px = solid(size, [10, 20, 30, 255]);
        let xml = read_stack_xml(&text_document(size, &px));
        assert!(
            xml.contains(&format!("{VERSION_ATTR}=\"1\"")),
            "a text layer is not a reason to shut older builds out:\n{xml}"
        );
    }

    /// **All four combinations of a text record and an effect**, because the
    /// claim "text raises nothing" is about the two features *together* and
    /// neither of them could have made it alone.
    ///
    /// The trap this is written against is a test that asserts 3 and passes for
    /// the wrong reason: effects take a document to 3 on their own, so a text
    /// test whose fixture happened to carry one would confirm nothing. Here the
    /// text-only row is the load-bearing one and the effect rows are what say
    /// the figure still moves when it should.
    #[test]
    fn text_never_raises_the_revision_a_document_declares() {
        let size = UVec2::new(2, 2);
        let px = solid(size, [1, 2, 3, 255]);
        let text = text_object();
        let outline = crate::effect::Effect::outline();

        for (with_text, with_effect, want) in [
            (false, false, 1),
            (true, false, 1),
            (false, true, 3),
            (true, true, 3),
        ] {
            let mut one = layer("Caption", &px);
            if with_text {
                one.text = Some(&text);
            }
            if with_effect {
                one.effects = std::slice::from_ref(&outline);
            }
            let layers = vec![one];
            let (bytes, _) = encode(&SaveDocument {
                size,
                layers: &layers,
                active: 0,
                background: Background::Transparent,
                dpi: Document::DEFAULT_DPI,
                merged: Canvas::Held(&px),
                history: None,
            })
            .expect("encode");
            let xml = read_stack_xml(&bytes);
            assert!(
                xml.contains(&format!("{VERSION_ATTR}=\"{want}\"")),
                "text {with_text}, effect {with_effect} should declare {want}:\n{xml}"
            );
        }
    }

    /// **The guard `docs/text-tool.md` §3 names.** An older build opens the
    /// document, the artist paints on the text layer, it is saved again with the
    /// record still beside pixels it did not make — and this build must open it as
    /// paint rather than re-render over that painting.
    ///
    /// The simulation is exactly that: the same archive with a different layer
    /// PNG in it. Nothing else about the file changes, which is the point — the
    /// record is untouched and still says what it always said.
    #[test]
    fn a_text_layer_painted_on_by_an_older_build_opens_as_paint() {
        let size = UVec2::new(4, 4);
        let px = solid(size, [10, 20, 30, 255]);
        let bytes = text_document(size, &px);

        // One pixel changed, in a PNG of the same size at the same offset — so
        // the fingerprint's rectangle still matches and only the hash does not.
        // The weaker of the two halves, deliberately.
        let painted = {
            let mut over = solid(size, [10, 20, 30, 255]);
            over[0] = 255;
            encode_png(size, &over).unwrap()
        };
        let doc = docimport::read_openraster(&with_entry(&bytes, "data/layer000.png", painted))
            .expect("the picture still opens");

        assert!(
            doc.layers[0].text.is_none(),
            "a record that does not fingerprint the pixels must be dropped"
        );
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, crate::docimport::ImportWarning::TextDropped { .. })),
            "and it must say so: {:?}",
            doc.warnings
        );
        // The painting is what is kept, which is the whole of the trade.
        assert_eq!(doc.layers[0].pixels[0], 255);
    }

    /// A record from a newer revision, and one that is simply rubbish, are both
    /// discarded rather than refusing the document. `docformat::VERSION` is what
    /// refuses a *document*; a record is discarded, for the reason a history
    /// manifest is.
    #[test]
    fn an_unreadable_text_record_drops_the_text_and_keeps_the_picture() {
        let size = UVec2::new(4, 4);
        let px = solid(size, [10, 20, 30, 255]);
        let bytes = text_document(size, &px);

        for body in [
            b"not json at all".to_vec(),
            {
                let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
                let mut held = Vec::new();
                std::io::Read::read_to_end(&mut zip.by_name(&text_src(0)).unwrap(), &mut held)
                    .unwrap();
                String::from_utf8(held)
                    .unwrap()
                    .replace("\"version\":1", "\"version\":99")
                    .into_bytes()
            },
            // **The record's own size bound, from the reading end.** A layer, a
            // mask and the merged image are all sized by the canvas, which
            // `MAX_TOTAL_BYTES` bounds; this one is sized by how much somebody
            // typed, so a small archive can claim a large record and the
            // document-wide figure does not reach it. An effects record is the
            // other entry of that shape and has its own figure — see
            // `textobj::MAX_RECORD_BYTES` for why the two are separate numbers.
            //
            // What this drives is `TextObject::from_json`'s length check, and
            // **it does not reach `read_optional_entry_bounded`'s limit** —
            // measured, by taking the limit out: the record is still dropped and
            // this test still passes, because the parse refuses it a moment
            // later. That limit is an allocation bound and its only observable
            // effect is the megabytes not spent decompressing a record that was
            // going to be refused, so nothing here can hold it in place. Said
            // rather than implied, because the comment claiming otherwise is
            // easier to write than the guard.
            format!(
                "{{\"version\":1,\"text\":\"{}\"}}",
                "x".repeat(crate::textobj::MAX_RECORD_BYTES + 16)
            )
            .into_bytes(),
        ] {
            let doc = docimport::read_openraster(&with_entry(&bytes, &text_src(0), body))
                .expect("the picture still opens");
            assert!(doc.layers[0].text.is_none());
            assert!(
                doc.warnings
                    .iter()
                    .any(|w| matches!(w, crate::docimport::ImportWarning::TextDropped { .. }))
            );
        }
    }

    /// A record too long to write is **named**, and the layer is saved as paint.
    ///
    /// Written rather than truncated, and named rather than passed over: a text
    /// layer that had quietly stopped being editable by the time somebody
    /// reopened the document is the discovered loss the warnings exist for.
    #[test]
    fn text_that_will_not_fit_the_record_is_saved_as_paint_and_said_so() {
        let size = UVec2::new(4, 4);
        let px = solid(size, [10, 20, 30, 255]);
        let mut text = text_object();
        text.block.text = "x".repeat(crate::textobj::MAX_RECORD_BYTES + 1);
        let layers = vec![SaveLayer {
            text: Some(&text),
            ..layer("Caption", &px)
        }];
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&px),
            history: None,
        })
        .expect("encode");
        assert_eq!(
            warnings,
            vec![SaveWarning::TextNotRecorded {
                layer: "Caption".into(),
                why: crate::textobj::NotRecorded::TooLarge,
            }]
        );
        // And the sentence names the length rather than blaming a figure.
        assert!(warnings[0].to_string().contains("too much text"));
        let xml = read_stack_xml(&bytes);
        assert!(
            !xml.contains(TEXT_ATTR),
            "no attribute may point at a record that was not written:\n{xml}"
        );
        let doc = docimport::read_openraster(&bytes).expect("read back");
        assert!(
            doc.layers[0].text.is_none() && doc.warnings.is_empty(),
            "a layer with no attribute says nothing at all: {:?}",
            doc.warnings
        );
    }

    // --- folders ------------------------------------------------------------

    /// A document with a folder in it, written and read straight back.
    ///
    /// The order is the thing to watch: `SaveDocument::layers` is bottom first,
    /// so the folder is the entry **above** its contents, and `stack.xml` is
    /// top first, so the `<stack>` element comes out before the layers inside
    /// it. Get that backwards and the group swallows the wrong layers.
    #[test]
    fn a_folder_survives_a_round_trip() {
        let size = UVec2::new(4, 4);
        let px = solid(size, [200, 30, 30, 255]);
        // Bottom first, and the two above "Paper" are inside the folder.
        let layers = vec![
            layer("Paper", &px),
            SaveLayer {
                depth: 1,
                ..layer("Sketch", &px)
            },
            SaveLayer {
                depth: 1,
                ..layer("Ink", &px)
            },
            SaveLayer::folder("Line art", 0, true),
        ];

        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 1,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&px),
            history: None,
        });
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        let shape: Vec<(&str, u8, bool)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), l.depth, l.folder))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Paper", 0, false),
                ("Sketch", 1, false),
                ("Ink", 1, false),
                ("Line art", 0, true),
            ]
        );
        assert_eq!(doc.active, Some(1), "the selected layer came back");

        // And the stack the document opens as agrees.
        let opened = doc.open();
        assert_eq!(opened.stack.subtree(3), 1..4);
        assert_eq!(opened.uploads.len(), 3, "a folder has no pixels to upload");
    }

    /// **Folders do not raise `umber-version`**, and this is the test that has
    /// to hold for that claim to be worth anything.
    ///
    /// A *pass-through* folder is exactly its contents composited in place, so
    /// an older Umber — or GIMP, or MyPaint — that flattens the nesting away
    /// shows the identical picture and loses only the grouping. That is
    /// "plainer", not "wrong", which is the line `VERSION` is drawn on. A
    /// folder with an opacity of its own would be the other case, and is
    /// exactly why there is nowhere to put one.
    #[test]
    fn a_document_of_folders_still_declares_the_revision_it_needs() {
        let size = UVec2::new(2, 2);
        let px = solid(size, [1, 2, 3, 255]);
        let layers = vec![
            SaveLayer {
                depth: 1,
                ..layer("Inside", &px)
            },
            SaveLayer::folder("Group", 0, false),
        ];
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&px),
            history: None,
        })
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let xml = read_stack_xml(&bytes);
        assert!(
            xml.contains(&format!("{VERSION_ATTR}=\"1\"")),
            "a folder is not a reason to shut older builds out:\n{xml}"
        );
        // Baseline OpenRaster nesting, not an `umber-` attribute of our own.
        assert!(xml.contains("<stack name=\"Group\""), "{xml}");
        assert!(xml.contains("</stack>"), "{xml}");
        // And emphatically no group opacity or blend mode, because there is no
        // such thing here and a reader that honoured one would draw a different
        // picture from the one Umber showed.
        let folder_line = xml
            .lines()
            .find(|l| l.contains("<stack name=\"Group\""))
            .unwrap();
        assert!(!folder_line.contains("opacity="), "{folder_line}");
        assert!(!folder_line.contains("composite-op="), "{folder_line}");
        assert!(
            folder_line.contains("visibility=\"hidden\""),
            "{folder_line}"
        );
        // Said out loud, because ORA's default is the opposite of what Umber
        // draws. Leaving it off makes every folder an isolated group in a
        // conforming reader, which changes the picture as soon as any child
        // blends with something other than src-over.
        assert!(
            folder_line.contains("isolation=\"auto\""),
            "a pass-through folder must say so, or a reader takes ORA's \
             `isolate` default and blends a folder's children against each \
             other instead of against the backdrop:\n{folder_line}"
        );
    }

    /// **A folder can belong to a link group, so the file has to carry it.**
    ///
    /// `targets` includes a ticked folder and `moving_with` is built for one,
    /// so a set of "these travel together" can name a group. Writing the
    /// attribute on the `<layer>` and not on the `<stack>` lost the folder out
    /// of the set silently — and brought the other member back *alone*, which
    /// `dissolve_lone_groups` exists to make unreachable: a chain in one colour
    /// meaning "moves together with nothing", holding a number `free_group`
    /// could never hand back.
    #[test]
    fn a_folder_in_a_link_group_comes_back_in_it() {
        let size = UVec2::new(2, 2);
        let px = solid(size, [7, 7, 7, 255]);
        let layers = vec![
            SaveLayer {
                link: Some(2),
                ..layer("Loose", &px)
            },
            SaveLayer {
                depth: 1,
                ..layer("Inside", &px)
            },
            SaveLayer {
                link: Some(2),
                ..SaveLayer::folder("Group", 0, true)
            },
        ];
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&px),
            history: None,
        })
        .unwrap();
        let xml = read_stack_xml(&bytes);
        assert!(
            xml.contains(&format!("{LINK_GROUP_ATTR}=\"2\"")),
            "the group number is not on the stack tag:\n{xml}"
        );

        let doc = docimport::read_openraster(&bytes).expect("read back");
        let links: Vec<Option<u8>> = doc.layers.iter().map(|l| l.link).collect();
        assert_eq!(links, vec![Some(2), None, Some(2)]);

        // And the stack it opens as agrees, so nothing dissolves the pair.
        let opened = doc.open();
        assert_eq!(
            opened.stack.group_indices(2),
            vec![0, 2],
            "the folder and the loose layer still travel together"
        );
    }

    /// A saved undo history resolves against **stack positions that count
    /// folders**, because that is what `SaveHistory::new` maps a slot to and
    /// what the manifest's name fingerprint is built from. If the two ever
    /// disagreed about what "layer 2" means, an undo would be replayed into the
    /// wrong layer — the worst failure in that module.
    #[test]
    fn a_history_survives_a_document_that_has_folders_in_it() {
        use crate::history::{Edit, EditBody, EditKind, History, PixelPatch};

        let size = UVec2::new(4, 4);
        let px = solid(size, [9, 9, 9, 255]);

        // A stack whose folder sits *below* the layer the patch belongs to, so
        // a mapping that ignored folders would be off by one.
        let mut stack = LayerStack::empty();
        stack.push_imported(false, 1, "Inside".into());
        stack.push_imported(true, 0, "Group".into());
        stack.push_imported(false, 0, "Above".into());
        let target = stack.get(2).unwrap().slot().unwrap();

        let rect = crate::geom::PixelRect {
            x: 0,
            y: 0,
            width: 2,
            height: 2,
        };
        let mut history = History::default();
        history.record(Edit::new(
            EditKind::Paint,
            EditBody::Pixels(PixelPatch::new(rect, target, vec![7; 2 * 2 * 4])),
        ));
        let saved = super::history::SaveHistory::new(&history, &stack)
            .expect("every patch names a live slice");

        let layers = vec![
            SaveLayer {
                depth: 1,
                ..layer("Inside", &px)
            },
            SaveLayer::folder("Group", 0, true),
            layer("Above", &px),
        ];
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 2,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&px),
            history: Some(saved),
        });
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        let opened = doc.open();
        assert_eq!(opened.history.len(), 1, "the history came back");
        let slot = opened.history.entry_at(0).unwrap().patches()[0].slot;
        assert_eq!(
            Some(slot),
            opened.stack.get(2).unwrap().slot(),
            "the patch came back on “Above”, not on the layer inside the group"
        );
    }

    /// Write a document and read it straight back through the importer, which
    /// is the only round trip that matters: the file is only worth anything if
    /// Umber's own reader gets the document back out of it.
    fn round_trip(doc: &SaveDocument<'_>) -> docimport::ImportedDocument {
        let (bytes, _) = encode(doc).expect("encode");
        docimport::read_openraster(&bytes).expect("read back")
    }

    #[test]
    fn a_stack_survives_a_round_trip() {
        let size = UVec2::new(4, 4);
        let bottom = solid(size, [200, 30, 30, 255]);
        let top = solid(size, [10, 10, 240, 255]);
        let merged = solid(size, [10, 10, 240, 255]);

        let layers = vec![
            SaveLayer {
                visible: true,
                opacity: 0.5,
                ..SaveLayer::new("Paper", BlendMode::Multiply, &bottom)
            },
            SaveLayer {
                visible: false,
                opacity: 1.0,
                ..SaveLayer::new("Ink", BlendMode::Screen, &top)
            },
        ];
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&merged),
            history: None,
        });

        assert_eq!(doc.size, size);
        assert_eq!(doc.layers.len(), 2);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        // Bottom first on both sides of the trip.
        assert_eq!(doc.layers[0].name, "Paper");
        assert_eq!(doc.layers[0].opacity, 0.5);
        assert_eq!(doc.layers[0].blend, BlendMode::Multiply);
        assert!(doc.layers[0].visible);
        assert_eq!(doc.layers[1].name, "Ink");
        assert_eq!(doc.layers[1].blend, BlendMode::Screen);
        assert!(!doc.layers[1].visible, "visibility was lost");

        assert_eq!(doc.active, Some(0), "the selected layer was lost");
        assert_eq!(doc.layers[0].pixels, bottom);
        assert_eq!(doc.layers[1].pixels, top);
    }

    /// A mask, a clip, a lock and a link all the way out and back.
    ///
    /// The mask is the one with pixels in it, so it is checked byte for byte:
    /// it goes into the archive as the red channel of a slice and has to come
    /// back as a slice whose red channel is those bytes again. Anything lossy
    /// in that path would show up as a mask that drifted a level every time the
    /// document was saved — the same failure
    /// `saving_and_reopening_does_not_move_a_pixel` guards for the picture.
    #[test]
    fn masks_and_the_layer_flags_survive_a_round_trip() {
        let size = UVec2::new(4, 4);
        let pixels = solid(size, [10, 20, 30, 255]);
        // A gradient rather than a flat fill, so a mask written back
        // transposed, trimmed or half-decoded could not pass.
        let mask: Vec<u8> = (0..16u8)
            .flat_map(|i| {
                let v = i * 17;
                [v, v, v, 255]
            })
            .collect();

        let layers = vec![
            SaveLayer {
                locked: true,
                ..SaveLayer::new("Paper", BlendMode::Normal, &pixels)
            },
            SaveLayer {
                mask: Some(Canvas::Held(&mask)),
                clipped: true,
                link: Some(3),
                ..SaveLayer::new("Ink", BlendMode::Normal, &pixels)
            },
        ];
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .expect("encode");
        assert!(warnings.is_empty(), "{warnings:?}");

        // A document that carries a mask needs the revision that was raised
        // for one, so an older build refuses it rather than opening a picture
        // with the mask silently gone.
        //
        // **2 as a literal, not `VERSION`.** It read `VERSION` until effects
        // took that to 3, at which point this assertion said "the newest
        // revision this build knows about" rather than "the revision a mask
        // needs" — and it would then have passed for a writer that declared
        // every file at the newest number, which is exactly what
        // `required_version` exists to prevent.
        assert!(
            read_stack_xml(&bytes).contains(&format!("{VERSION_ATTR}=\"2\"")),
            "a masked document must declare revision 2 and no higher"
        );
        // And the mask lives outside the ORA stack, so no other reader shows it
        // as a layer nobody made.
        let names: Vec<String> = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..]))
            .unwrap()
            .file_names()
            .map(str::to_string)
            .collect();
        assert!(names.iter().any(|n| n == &mask_src(0)), "{names:?}");
        assert_eq!(
            names.iter().filter(|n| n.starts_with("data/")).count(),
            2,
            "the mask must not be one of the layers"
        );

        let doc = docimport::read_openraster(&bytes).expect("read back");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(doc.layers.len(), 2);
        assert!(doc.layers[0].locked, "the lock was lost");
        assert!(!doc.layers[0].clipped);
        assert_eq!(doc.layers[0].mask, None);
        assert!(doc.layers[1].clipped, "the clip was lost");
        assert_eq!(doc.layers[1].link, Some(3), "the link group was lost");
        assert_eq!(
            doc.layers[1].mask.as_deref(),
            Some(&mask[..]),
            "the mask did not come back byte for byte"
        );

        // And the mask reaches the stack as a slice of its own, ready to be
        // uploaded like any other.
        let opened = doc.open();
        let mask_slot = opened.stack.mask_at(1).expect("the layer kept its mask");
        assert!(opened.stack.mask_at(0).is_none());
        assert!(
            opened
                .uploads
                .iter()
                .any(|u| u.slot == mask_slot && u.pixels == mask),
            "the mask's pixels were not handed over for upload"
        );
    }

    // --- layer effects --------------------------------------------------

    /// An effect that reads back as the *same effect*, parameter for
    /// parameter, and the colour bit for bit.
    ///
    /// `saving_and_reopening_does_not_move_a_pixel` is the pattern and the
    /// reason is the same one: an effect's colour is four linear `f32`s, RON
    /// writes an `f32` as text, and a shortened one is a colour that drifts a
    /// little every time a document is saved and reopened. So the components
    /// are awkward rather than round, and they are compared **as bits** —
    /// `PartialEq` on `f32` would let a `-0.0` for a `0.0` past, and equality
    /// on the whole struct would not say which field moved.
    ///
    /// It goes all the way to a `LayerStack`, because that is the round trip
    /// somebody actually makes: `Layer::effects` in, `Layer::effects` out.
    #[test]
    fn an_effect_survives_a_save_and_a_reopen_bit_for_bit() {
        let size = UVec2::new(4, 4);
        let pixels = solid(size, [10, 20, 30, 255]);

        let mut shadow = Effect::drop_shadow();
        shadow.color = Color::new(0.123_456_79, 0.007_812_5, 0.999_999_94, 0.333_333_34);
        shadow.opacity = 0.618_034;
        shadow.angle = 37.5;
        shadow.distance = 11.25;
        shadow.softness = 2.5;
        shadow.spread = 0.125;
        shadow.blend = BlendMode::Screen;
        // Carried by every effect and read by an outline alone, which is
        // exactly why a shadow is the one to check it on: a writer that only
        // wrote the fields a kind reads would drop it here.
        shadow.position = OutlinePosition::Inside;

        let mut outline = Effect::outline();
        outline.position = OutlinePosition::Centre;
        // Disabled, and still written: a switched-off effect is a set of
        // parameters somebody dialled.
        outline.enabled = false;

        let effects = [shadow, outline];
        let layers = vec![SaveLayer {
            effects: &effects,
            ..layer("Ink", &pixels)
        }];
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .expect("encode");
        assert_eq!(
            warnings,
            vec![SaveWarning::EffectsNotPortable { layers: 1 }],
            "an effected layer is a loss for every other reader and must say so"
        );

        // Outside the ORA layer stack, exactly as a mask is, so no other
        // reader shows anything it did not before.
        let names: Vec<String> = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..]))
            .unwrap()
            .file_names()
            .map(str::to_string)
            .collect();
        assert!(
            names.iter().any(|n| n == "umber/effects/000.ron"),
            "{names:?}"
        );
        assert_eq!(
            names.iter().filter(|n| n.starts_with("data/")).count(),
            1,
            "the effects must not be a layer"
        );
        let xml = read_stack_xml(&bytes);
        assert!(
            xml.contains(&format!("{EFFECTS_ATTR}=\"umber/effects/000.ron\"")),
            "{xml}"
        );

        let doc = docimport::read_openraster(&bytes).expect("read back");
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        assert_eq!(
            doc.layers[0].effects, effects,
            "{:?}",
            doc.layers[0].effects
        );

        // And through the stack, which is where the invariants live.
        let opened = doc.open();
        let back = opened.stack.get(0).expect("the layer").effects();
        assert_eq!(back.len(), 2);
        for (before, after) in effects.iter().zip(back) {
            assert_eq!(before.kind, after.kind);
            assert_eq!(before.enabled, after.enabled);
            assert_eq!(before.blend, after.blend);
            assert_eq!(before.position, after.position);
            for (a, b) in before.color.to_array().iter().zip(after.color.to_array()) {
                assert_eq!(a.to_bits(), b.to_bits(), "the colour moved");
            }
            for (a, b) in [
                (before.opacity, after.opacity),
                (before.spread, after.spread),
                (before.softness, after.softness),
                (before.angle, after.angle),
                (before.distance, after.distance),
            ] {
                assert_eq!(
                    a.to_bits(),
                    b.to_bits(),
                    "{before:?} came back as {after:?}"
                );
            }
        }
    }

    /// **`required_version` still emits the lowest revision the file needs.**
    ///
    /// The sibling of `a_document_of_folders_still_declares_the_revision_it_
    /// needs`, and the guard that has to hold for every one of these numbers
    /// to be worth writing. The temptation whenever a revision is added is to
    /// declare the newest one and be done; that shuts every document this
    /// build touches out of every older Umber in exchange for nothing.
    ///
    /// Written as a sweep over the whole ladder rather than as three tests, so
    /// a fourth revision has one place to be added and cannot be added by
    /// relaxing an existing assertion.
    #[test]
    fn a_document_declares_the_lowest_revision_that_describes_it() {
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [7, 8, 9, 255]);
        let mask = solid(size, [128, 128, 128, 255]);
        let effects = [Effect::drop_shadow()];

        let plain = || layer("Ink", &pixels);
        let masked = || SaveLayer {
            mask: Some(Canvas::Held(&mask)),
            ..plain()
        };
        let clipped = || SaveLayer {
            clipped: true,
            ..plain()
        };
        let effected = || SaveLayer {
            effects: &effects,
            ..plain()
        };
        // An effect *and* a mask: the highest of the two, not the sum and not
        // whichever was tested last.
        let both = || SaveLayer {
            mask: Some(Canvas::Held(&mask)),
            ..effected()
        };

        for (expected, layers) in [
            (1, vec![plain()]),
            (2, vec![masked()]),
            (2, vec![clipped()]),
            (3, vec![effected()]),
            (3, vec![both()]),
            (3, vec![plain(), effected()]),
        ] {
            assert_eq!(
                required_version(&layers),
                expected,
                "{} layer(s), flags {:?}",
                layers.len(),
                layers
                    .iter()
                    .map(|l| (l.mask.is_some(), l.clipped, l.effects.len()))
                    .collect::<Vec<_>>()
            );
        }

        // A disabled effect is still an effect in the file, so it still needs
        // the revision that describes one. Reading `enabled_effect_count` here
        // would let an older build open the document and drop the parameters,
        // which is the whole failure the bump exists for.
        let off = [Effect {
            enabled: false,
            ..Effect::outline()
        }];
        let layers = vec![SaveLayer {
            effects: &off,
            ..plain()
        }];
        assert_eq!(required_version(&layers), 3);

        // And the number reaches the file rather than only the function.
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .expect("encode");
        let xml = read_stack_xml(&bytes);
        assert!(xml.contains(&format!("{VERSION_ATTR}=\"3\"")), "{xml}");
    }

    /// **Every finite `f32` an effect can hold survives the record**, which is
    /// the claim `an_effect_survives_a_save_and_a_reopen_bit_for_bit` makes on
    /// six values and this one makes on the axis.
    ///
    /// Six hand-picked numbers cannot answer "does RON's `f32` round-trip",
    /// because the failure mode of a text encoder is a *class* of values —
    /// subnormals, the extremes, whatever needs the ninth significant digit —
    /// and a test author picks the ones they thought of. So this sweeps bit
    /// patterns rather than numbers, over `opacity` and `angle` together
    /// because a shared serialiser could still be given per-field attributes.
    ///
    /// **Measured before it was written**, over 398,459 finite patterns
    /// including both infinities, both zeroes, `MIN_POSITIVE`, the smallest
    /// subnormal of each sign, `MAX`, `MIN` and `EPSILON`: not one moved. The
    /// sweep here is cut to five thousand, which is 0.04 s against twenty
    /// thousand's 0.31 — the point is to catch a *class* of value, and every
    /// class is reachable at either size. The specials are listed explicitly
    /// so they are in it whatever the sample does.
    ///
    /// **NaN is excluded and that is a real limit, stated rather than hidden.**
    /// RON writes `NaN` and reads back the *canonical* quiet NaN of that sign —
    /// so `0x7fc0_0000` and `0xffc0_0000` do survive whole, and only a
    /// non-canonical *payload* is lost. Nothing in Umber can produce one and
    /// nothing can see the difference, but a later reader of this test should
    /// not conclude that every bit pattern survives, because one class does
    /// not.
    #[test]
    fn every_finite_parameter_an_effect_can_hold_survives_the_record() {
        let mut values: Vec<f32> = vec![
            0.0,
            -0.0,
            f32::MIN_POSITIVE,
            f32::from_bits(1),
            f32::from_bits(0x807F_FFFF),
            f32::MAX,
            f32::MIN,
            f32::EPSILON,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.0 / 3.0,
            core::f32::consts::PI,
        ];
        // A plain linear congruential walk, so the sample is the same on every
        // machine and every run — a random one would make this flaky in the
        // one way a format test must never be.
        let mut state: u32 = 0x9E37_79B9;
        while values.len() < 5_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let v = f32::from_bits(state);
            if !v.is_nan() {
                values.push(v);
            }
        }

        for v in values {
            let effect = Effect {
                opacity: v,
                angle: v,
                ..Effect::outline()
            };
            let text = encode_effects(&[effect]).expect("encode");
            let back: Vec<Effect> = ron::from_str(&text).expect("read back");
            assert_eq!(back.len(), 1);
            assert_eq!(
                back[0].opacity.to_bits(),
                v.to_bits(),
                "{v:?} ({:#x}) moved, written as `{text}`",
                v.to_bits()
            );
            assert_eq!(back[0].angle.to_bits(), v.to_bits(), "{v:?}");
        }
    }

    /// The record is a plain RON sequence at a path this module names, and
    /// both halves are format rather than decoration.
    ///
    /// What it deliberately does **not** do is pin the whole text: the field
    /// names, the variant names and the colour's shape are already pinned as
    /// literals in `effect`'s own tests, against the compact spelling, which
    /// is where a rename has to be caught. Pinning the pretty-printed form
    /// here as well would fail on a `ron` release that changed its
    /// indentation, which is a false alarm about a real guard.
    #[test]
    fn the_effects_record_is_a_ron_sequence_where_this_module_says() {
        assert_eq!(effects_src(0), "umber/effects/000.ron");
        assert_eq!(effects_src(12), "umber/effects/012.ron");

        let text = encode_effects(&[Effect::outline(), Effect::drop_shadow()]).expect("encode");
        assert!(text.trim_start().starts_with('['), "{text}");
        assert!(text.trim_end().ends_with(']'), "{text}");
        let back: Vec<Effect> = ron::from_str(&text).expect("read back");
        assert_eq!(back, vec![Effect::outline(), Effect::drop_shadow()]);
    }

    /// **The portability warning names a loss somebody can see, once for the
    /// document.**
    ///
    /// Two rules and each was wrong in the first draft.
    ///
    /// *Once.* `docs/layer-effects.md` §8.3 says "told once at the save", and
    /// per layer it was thirty lines of one sentence with a different name in
    /// them — the noise `ImportWarning::EffectsOverBudget` is explicitly
    /// written to avoid, in the same feature, arguing against itself.
    ///
    /// *Seen.* A layer whose every effect is switched off draws plain **in
    /// Umber too**, so telling the artist it will look plain elsewhere reports
    /// a loss that did not happen. That is the trap `export::losses` already
    /// avoids by asking whether *this* document has transparency. The record is
    /// still written and the revision is still 3, because the parameters are
    /// what an older build would drop; only the sentence is about what shows.
    #[test]
    fn only_a_visible_effect_is_named_as_a_loss_and_only_once() {
        let size = UVec2::new(2, 2);
        let px = solid(size, [1, 2, 3, 255]);
        let on = [Effect::drop_shadow()];
        let off = [Effect {
            enabled: false,
            ..Effect::outline()
        }];

        let warnings_for = |layers: Vec<SaveLayer<'_>>| {
            encode(&SaveDocument {
                size,
                layers: &layers,
                active: 0,
                background: Background::Transparent,
                dpi: Document::DEFAULT_DPI,
                merged: Canvas::Held(&px),
                history: None,
            })
            .expect("encode")
            .1
        };

        // Nothing switched on: the record is written, and nothing is claimed.
        let quiet = warnings_for(vec![SaveLayer {
            effects: &off,
            ..layer("Off", &px)
        }]);
        assert!(quiet.is_empty(), "{quiet:?}");

        // Three effected layers, one sentence, and the count is of layers
        // carrying something visible rather than of layers or of effects.
        let loud = warnings_for(vec![
            SaveLayer {
                effects: &on,
                ..layer("A", &px)
            },
            SaveLayer {
                effects: &off,
                ..layer("B", &px)
            },
            SaveLayer {
                effects: &on,
                ..layer("C", &px)
            },
        ]);
        assert_eq!(loud, vec![SaveWarning::EffectsNotPortable { layers: 2 }]);

        // And it reads as a sentence at both counts, because a count of one is
        // the common case and "1 layers have" is how that goes wrong.
        assert!(
            SaveWarning::EffectsNotPortable { layers: 1 }
                .to_string()
                .starts_with("1 layer has layer effects"),
            "{}",
            SaveWarning::EffectsNotPortable { layers: 1 }
        );
        assert!(
            SaveWarning::EffectsNotPortable { layers: 2 }
                .to_string()
                .starts_with("2 layers have layer effects")
        );
    }

    /// **A document does not depend on the machine that wrote it**, and
    /// `PrettyConfig`'s default line ending would have made it.
    ///
    /// `ron::ser::PrettyConfig::new()` takes the platform's newline, so the
    /// same document saved on Windows and on Linux held different bytes inside
    /// `umber/effects/`. Nothing in Umber reads those bytes back for
    /// comparison, so it would not have failed a test; it would have been found
    /// by whoever first diffed two `.ora`s or put one in version control.
    ///
    /// The other half is `preset::write`, which shares the config and keeps the
    /// platform ending. That is *right* there and wrong here for one reason: a
    /// `brushes.ron` stays on the machine that wrote it and a document travels.
    #[test]
    fn the_effects_record_carries_no_platform_line_ending() {
        let text = encode_effects(&[Effect::drop_shadow(), Effect::outline()]).expect("encode");
        assert!(text.contains('\n'), "it is meant to be pretty: {text:?}");
        assert!(
            !text.contains('\r'),
            "a document must not differ by the machine that wrote it: {text:?}"
        );
    }

    /// A record whose sequence is **not** in composite order comes back in it.
    ///
    /// `ImportedDocument::open`'s comment claims this — "it re-derives the
    /// order, so a file whose sequence was written by a build that ordered them
    /// differently still comes back right" — and nothing exercised it, because
    /// every other test's fixture happens to be in rank order already and the
    /// sort is then the identity. A guard whose fixture cannot distinguish the
    /// mutation is a guard for a claim nobody is making.
    ///
    /// It matters because the order is not the writer's to promise: an inside
    /// outline ranks *above* the layer and a drop shadow below it, and which
    /// way round they land decides whether the shadow draws over the outline.
    #[test]
    fn a_record_out_of_composite_order_comes_back_in_it() {
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [4, 5, 6, 255]);
        let inside = Effect {
            position: OutlinePosition::Inside,
            ..Effect::outline()
        };
        let shadow = Effect::drop_shadow();
        assert!(shadow.rank() < inside.rank(), "the fixture is backwards");

        // Written the wrong way round on purpose.
        let effects = [inside, shadow];
        let layers = vec![SaveLayer {
            effects: &effects,
            ..layer("Ink", &pixels)
        }];
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .expect("encode");

        // The reader hands back what the file said, in the file's order: it is
        // not the reader's business to sort, and a reader that did would hide
        // the stack failing to.
        let doc = docimport::read_openraster(&bytes).expect("read back");
        assert_eq!(doc.layers[0].effects, [inside, shadow]);

        // The stack is where the invariant lives, and it puts them right.
        let opened = doc.open();
        assert_eq!(opened.stack.get(0).unwrap().effects(), &[shadow, inside]);
    }

    /// **A folder writes no effects, and `required_version` counts none**, so
    /// the archive and the number the file declares cannot disagree.
    ///
    /// Unreachable through the model — `LayerStack::plan_set_effect` refuses a
    /// folder, because a folder holds no slot and there is no coverage to
    /// derive an effect from until group compositing lands
    /// (`docs/layer-effects.md` §9.5) — and `SaveLayer::effects` is a public
    /// field, so the case has to have an answer anyway. The answer is that
    /// **both halves skip a folder together**: writing the entry while the
    /// version clause skipped it would produce a file carrying effects and
    /// declaring revision 1, which every older Umber would open and then drop.
    ///
    /// This is what has to change when a folder can carry one, and changing
    /// only one half is what this test exists to catch.
    #[test]
    fn a_folder_writes_no_effects_and_declares_no_revision_for_them() {
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [7, 8, 9, 255]);
        let effects = [Effect::drop_shadow()];
        let layers = vec![
            SaveLayer {
                depth: 1,
                ..layer("Inside", &pixels)
            },
            SaveLayer {
                effects: &effects,
                ..SaveLayer::folder("Group", 0, true)
            },
        ];
        assert_eq!(required_version(&layers), 1);

        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .expect("encode");
        assert!(warnings.is_empty(), "{warnings:?}");

        let names: Vec<String> = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..]))
            .unwrap()
            .file_names()
            .map(str::to_string)
            .collect();
        assert!(
            !names.iter().any(|n| n.starts_with("umber/effects/")),
            "{names:?}"
        );
        let xml = read_stack_xml(&bytes);
        assert!(xml.contains(&format!("{VERSION_ATTR}=\"1\"")), "{xml}");
        assert!(!xml.contains(EFFECTS_ATTR), "{xml}");
    }

    /// Two independent link groups out and back, and the old spelling still
    /// read.
    ///
    /// The second half is why neither attribute moved [`VERSION`]. The file
    /// keeps writing `umber-link="true"` beside the group, so a build from
    /// before groups existed reads a linked layer and treats every one of them
    /// as one set — which changes no pixel and is exactly what that build did
    /// with the file it wrote. This is the same allowance locks and links were
    /// given when the version went to 2.
    #[test]
    fn link_groups_survive_a_round_trip_and_the_old_spelling_still_reads() {
        let size = UVec2::new(4, 4);
        let pixels = solid(size, [10, 20, 30, 255]);
        let layers: Vec<SaveLayer<'_>> = [Some(0u8), Some(2), None, Some(0)]
            .into_iter()
            .enumerate()
            .map(|(i, link)| SaveLayer {
                link,
                ..SaveLayer::new(
                    match i {
                        0 => "a",
                        1 => "b",
                        2 => "c",
                        _ => "d",
                    },
                    BlendMode::Normal,
                    &pixels,
                )
            })
            .collect();
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: 72.0,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .expect("encode");
        assert!(warnings.is_empty(), "{warnings:?}");

        let doc = docimport::read_openraster(&bytes).expect("read back");
        assert_eq!(
            doc.layers.iter().map(|l| l.link).collect::<Vec<_>>(),
            vec![Some(0), Some(2), None, Some(0)],
            "the groups did not come back as they went in"
        );

        // Every layer in a group still carries the pre-groups flag, so an older
        // build reads three linked layers rather than none.
        let xml = stack_xml(&bytes);
        assert_eq!(xml.matches(&format!("{LINK_ATTR}=\"true\"")).count(), 3);

        // And a file written *by* such a build — the flag with no group — reads
        // as the one set it was.
        let older = xml.replace(&format!(" {LINK_GROUP_ATTR}=\"0\""), "");
        let older = older.replace(&format!(" {LINK_GROUP_ATTR}=\"2\""), "");
        let rebuilt = rewrite_stack(&bytes, &older);
        let doc = docimport::read_openraster(&rebuilt).expect("read back");
        assert_eq!(
            doc.layers.iter().map(|l| l.link).collect::<Vec<_>>(),
            vec![Some(0), Some(0), None, Some(0)],
            "a pre-groups file is one group, which is what it always was"
        );
    }

    /// `stack.xml` out of an archive, as text.
    fn stack_xml(bytes: &[u8]) -> String {
        use std::io::{Cursor, Read};
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let mut entry = zip.by_name("stack.xml").expect("stack.xml");
        let mut out = String::new();
        entry.read_to_string(&mut out).expect("utf-8");
        out
    }

    /// Replace `stack.xml` inside an archive, leaving every other entry alone.
    ///
    /// Only a test needs this: it is how a file written by an *older* Umber is
    /// produced without keeping a binary fixture that would have to be rebuilt
    /// every time the writer changed anything else.
    fn rewrite_stack(bytes: &[u8], stack: &str) -> Vec<u8> {
        use std::io::{Cursor, Read, Write};
        let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).expect("zip");
        let mut out = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).expect("entry");
            let name = entry.name().to_owned();
            let options: zip::write::FileOptions<'_, ()> =
                zip::write::FileOptions::default().compression_method(entry.compression());
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf).expect("read");
            out.start_file(&name, options).expect("start");
            if name == "stack.xml" {
                out.write_all(stack.as_bytes()).expect("write");
            } else {
                out.write_all(&buf).expect("write");
            }
        }
        out.finish().expect("finish").into_inner()
    }

    #[test]
    fn the_canvas_settings_survive_a_round_trip() {
        // The same idea as `saving_and_reopening_does_not_move_a_pixel`, for
        // the two things that are not pixels: a document reopened has to be the
        // document that was saved, background and resolution included.
        let size = UVec2::new(4, 4);
        let pixels = empty(size);
        let paper = Color::from_srgb_u8(247, 243, 233, 255);
        let layers = vec![layer("Ink", &pixels)];

        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::opaque(paper),
            dpi: 300.0,
            merged: Canvas::Held(&pixels),
            history: None,
        });

        assert_eq!(doc.background, Background::opaque(paper));
        assert_eq!(doc.dpi, Some(300.0));
        assert_eq!(
            doc.layers.len(),
            1,
            "the background must come back as the background, not as a layer"
        );
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        // And the same document rebuilt, which is what the editor installs.
        let reopened = doc.document();
        assert_eq!(
            reopened,
            Document::new(4, 4)
                .with_background(Background::opaque(paper))
                .with_dpi(300.0)
        );
    }

    #[test]
    fn a_transparent_document_writes_no_background_layer_at_all() {
        // The identity case. A document with nothing behind its stack has to
        // produce exactly the file it always did — an extra layer of nothing
        // would show up in every other application's layers panel.
        let size = UVec2::new(4, 4);
        let pixels = empty(size);
        let layers = vec![layer("Ink", &pixels)];
        let (bytes, warnings) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");

        let zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..])).unwrap();
        assert!(
            !zip.file_names().any(|n| n == BACKGROUND_SRC),
            "a transparent document must not carry a background image"
        );

        let back = docimport::read_openraster(&bytes).unwrap();
        assert_eq!(back.background, Background::Transparent);
        assert_eq!(back.layers.len(), 1);
    }

    #[test]
    fn every_other_application_gets_the_background_as_real_pixels() {
        // The whole reason the colour is written twice. Reading the file the
        // way an application that has never heard of `umber-background` would
        // must produce an opaque bottom layer of the right colour — otherwise a
        // white painting opens in Krita on a checkerboard.
        let size = UVec2::new(3, 2);
        let pixels = empty(size);
        let layers = vec![layer("Ink", &pixels)];
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::opaque(Color::from_srgb_u8(20, 120, 200, 255)),
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .unwrap();

        // Strip the attribute and read it back: that is exactly what a foreign
        // reader does, since an XML reader ignores what it does not know.
        let foreign = with_stack_xml(&bytes, |xml| {
            let start = xml.find(BACKGROUND_ATTR).expect("the attribute");
            let end = xml[start..].find("/>").expect("the element") + start;
            format!("{}{}", &xml[..start], &xml[end..])
        });
        let back = docimport::read_openraster(&foreign).unwrap();

        assert_eq!(back.background, Background::Transparent, "no attribute");
        assert_eq!(back.layers.len(), 2, "the background is a layer to them");
        assert_eq!(back.layers[0].name, "Background", "and it is at the bottom");
        assert!(
            back.layers[0].pixels.chunks_exact(4).all(|p| p[3] == 255),
            "the background layer must be opaque everywhere"
        );
        assert_eq!(&back.layers[0].pixels[..4], &[20, 120, 200, 255]);
    }

    #[test]
    fn the_resolution_is_openrasters_own_attribute() {
        // `xres`/`yres` rather than an invented `umber-dpi`: every reader that
        // cares about print already looks for them.
        let size = UVec2::new(2, 2);
        let pixels = empty(size);
        let layers = vec![layer("Ink", &pixels)];
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: 150.0,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .unwrap();

        let xml = read_stack_xml(&bytes);
        assert!(xml.contains("xres=\"150\""), "{xml}");
        assert!(xml.contains("yres=\"150\""), "{xml}");
        assert!(
            !xml.contains("umber-dpi"),
            "a standard attribute must not grow a private twin: {xml}"
        );
    }

    #[test]
    fn a_document_with_a_full_stack_and_a_background_still_reopens() {
        // The background is an extra layer in the file, so a document already
        // at `LayerStack::MAX` writes MAX + 1 of them. The reader takes the
        // background out before it counts, which is what stops Umber writing a
        // file it then refuses to open.
        let size = UVec2::new(1, 1);
        let pixels = empty(size);
        let layers: Vec<SaveLayer> = (0..LayerStack::MAX).map(|_| layer("L", &pixels)).collect();
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::WHITE,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        });
        assert_eq!(doc.layers.len(), LayerStack::MAX);
        assert_eq!(doc.background, Background::WHITE);
    }

    #[test]
    fn a_background_colour_survives_its_own_round_trip() {
        for bytes in [[0, 0, 0], [255, 255, 255], [1, 128, 254], [247, 243, 233]] {
            let colour = Color::from_srgb_u8(bytes[0], bytes[1], bytes[2], 255);
            let id = background_id(colour);
            assert_eq!(id.len(), 7, "{id}");
            assert_eq!(
                background_from_id(&id).map(Color::to_srgb_u8),
                Some([bytes[0], bytes[1], bytes[2], 255]),
            );
        }
        // Anything a hand-edited file might hold. `None` leaves the layer as a
        // layer, which is still the right picture.
        for bad in ["", "#", "ffffff", "#fffff", "#gggggg", "#ffffffff"] {
            assert!(background_from_id(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn partly_transparent_pixels_come_back_byte_for_byte() {
        // The one thing a document format has to get right. These are stored
        // premultiplied and ORA is straight alpha, so every one of them makes
        // the trip through a conversion and back.
        let size = UVec2::new(4, 1);
        let mut pixels = Vec::new();
        for a in [0u8, 1, 40, 128, 254] {
            pixels.extend_from_slice(&srgb::encode_pixel([220, 90, 15, a]));
        }
        pixels.truncate(size.x as usize * 4);

        let layers = vec![layer("Wash", &pixels)];
        let merged = empty(size);
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&merged),
            history: None,
        });
        assert_eq!(doc.layers[0].pixels, pixels);
    }

    #[test]
    fn a_layer_painted_in_one_corner_lands_back_in_that_corner() {
        // Layers are written cropped to their content, so the offset has to be
        // right or the whole layer moves.
        let size = UVec2::new(8, 8);
        let mut pixels = empty(size);
        let at = (5 * 8 + 6) * 4;
        pixels[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);

        let layers = vec![layer("Dot", &pixels)];
        let merged = empty(size);
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&merged),
            history: None,
        });
        assert_eq!(doc.layers[0].pixels, pixels);
    }

    #[test]
    fn an_untouched_layer_survives_rather_than_vanishing() {
        let size = UVec2::new(4, 4);
        let blank = empty(size);
        let painted = solid(size, [1, 2, 3, 255]);
        let layers = vec![layer("Blank", &blank), layer("Painted", &painted)];
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 1,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&painted),
            history: None,
        });

        assert_eq!(doc.layers.len(), 2, "the empty layer was dropped");
        assert_eq!(doc.layers[0].name, "Blank");
        assert!(doc.layers[0].pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn add_is_written_as_plus_and_read_back_as_add() {
        // `svg:plus` is the nearest ORA has, and it is not exact — so the save
        // says so, and writes the hint that stops the *reload* claiming a loss
        // that did not happen.
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [90, 90, 90, 255]);
        let layers = vec![SaveLayer {
            visible: true,
            opacity: 1.0,
            ..SaveLayer::new("Glow", BlendMode::Add, &pixels)
        }];
        let doc = SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        };

        let (bytes, warnings) = encode(&doc).unwrap();
        assert_eq!(
            warnings,
            vec![SaveWarning::BlendApproximated {
                layer: "Glow".into(),
                mode: "Add",
                used: "svg:plus",
            }]
        );

        let back = docimport::read_openraster(&bytes).unwrap();
        assert_eq!(back.layers[0].blend, BlendMode::Add);
        assert!(
            back.warnings.is_empty(),
            "reopening Umber's own file must not report a loss: {:?}",
            back.warnings
        );
    }

    #[test]
    fn a_name_with_xml_in_it_does_not_break_the_file() {
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [5, 5, 5, 255]);
        let layers = vec![layer("<ink> & \"paper\"", &pixels)];
        let doc = round_trip(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        });
        assert_eq!(doc.layers[0].name, "<ink> & \"paper\"");
    }

    #[test]
    fn a_newer_document_is_refused_rather_than_half_read() {
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [5, 5, 5, 255]);
        let layers = vec![layer("Ink", &pixels)];
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .unwrap();

        // A document with no mask and no clipping declares revision 1, so it
        // still opens in every build that came before — see `required_version`.
        assert!(
            read_stack_xml(&bytes).contains(&format!("{VERSION_ATTR}=\"1\"")),
            "a plain document must not claim a revision it does not use"
        );

        // Rewrite the archive with the version bumped past this build's.
        let doctored = with_stack_xml(&bytes, |xml| {
            xml.replace(
                &format!("{VERSION_ATTR}=\"1\""),
                &format!("{VERSION_ATTR}=\"{}\"", VERSION + 1),
            )
        });
        let err = docimport::read_openraster(&doctored).unwrap_err();
        assert!(
            matches!(err, ImportError::NewerVersion { .. }),
            "got {err:?}"
        );
        assert!(err.to_string().contains("newer version of Umber"));
    }

    #[test]
    fn the_file_is_a_plain_openraster() {
        // Everything the specification requires of a writer. If this stops
        // holding, work saved from Umber stops opening in Krita and GIMP —
        // which is the whole reason the format is this one.
        let size = UVec2::new(300, 200);
        let pixels = solid(size, [40, 60, 80, 255]);
        let layers = vec![layer("Ink", &pixels)];
        let (bytes, _) = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .unwrap();

        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&bytes[..])).unwrap();
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        for required in [
            "mimetype",
            "stack.xml",
            "mergedimage.png",
            "Thumbnails/thumbnail.png",
        ] {
            assert!(names.contains(&required.to_string()), "missing {required}");
        }

        let first = zip.by_index(0).unwrap();
        assert_eq!(first.name(), "mimetype", "mimetype must come first");
        assert_eq!(
            first.compression(),
            CompressionMethod::Stored,
            "mimetype must be stored uncompressed"
        );
        drop(first);

        // The thumbnail is bounded on its long edge and keeps its shape.
        let mut thumb = Vec::new();
        std::io::Read::read_to_end(
            &mut zip.by_name("Thumbnails/thumbnail.png").unwrap(),
            &mut thumb,
        )
        .unwrap();
        let decoded = png::Decoder::new(std::io::Cursor::new(thumb))
            .read_info()
            .unwrap();
        let info = decoded.info();
        assert_eq!((info.width, info.height), (256, 171));
    }

    #[test]
    fn a_document_too_tall_for_the_stack_is_refused_before_anything_is_written() {
        let size = UVec2::new(1, 1);
        let pixels = solid(size, [0, 0, 0, 255]);
        let layers: Vec<SaveLayer> = (0..LayerStack::MAX + 1)
            .map(|_| layer("L", &pixels))
            .collect();
        let err = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        })
        .unwrap_err();
        assert!(matches!(err, SaveError::TooManyLayers { .. }), "{err:?}");
    }

    #[test]
    fn a_mismatched_buffer_is_refused_rather_than_sheared() {
        let size = UVec2::new(4, 4);
        let short = vec![0u8; 8];
        let full = empty(size);
        let layers = vec![layer("Ink", &short)];
        let err = encode(&SaveDocument {
            size,
            layers: &layers,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&full),
            history: None,
        })
        .unwrap_err();
        assert!(matches!(err, SaveError::WrongSize { .. }), "{err:?}");
    }

    #[test]
    fn blend_names_survive_their_own_round_trip() {
        for mode in BlendMode::ALL {
            assert_eq!(blend_from_id(&blend_id(mode)), Some(mode));
        }
        assert_eq!(blend_from_id("Dissolve"), None);
    }

    /// **These strings are a file format, and what this catches is a rename.**
    ///
    /// [`blend_id`] is the derived `Debug` spelling, so what goes into
    /// [`BLEND_ATTR`] is the variant's name in the source. See that function
    /// for what a rename costs — briefly, a spurious "blend approximated"
    /// warning on every Add layer already written, and, through the same
    /// identifier reaching serde, a `brushes.ron` that no longer parses at
    /// all. The round trip above moves with the rename and cannot see it;
    /// text written out here does not move.
    ///
    /// As with the history's twin: a mode *added* fails this too, and there
    /// appending the name is the right fix. A mode *renamed* is the case this
    /// exists for, and the literal is not the thing to edit first.
    #[test]
    fn the_names_written_into_the_blend_attribute_are_these_exact_strings() {
        let spelled: Vec<String> = BlendMode::ALL.into_iter().map(blend_id).collect();
        assert_eq!(
            spelled,
            [
                "Normal",
                "Darken",
                "Multiply",
                "ColorBurn",
                "LinearBurn",
                "Lighten",
                "Screen",
                "ColorDodge",
                "Add",
                "AddGlow",
                "Overlay",
                "SoftLight",
                "HardLight",
                "VividLight",
                "LinearLight",
                "PinLight",
                "Difference",
                "Exclusion",
                "Subtract",
                "Divide",
                "Hue",
                "Saturation",
                "Color",
                "Luminosity"
            ]
        );
    }

    /// [`BlendMode::ALL`] is a hand-written array, and a mode missing from it
    /// is a mode that does not exist as far as three readers are concerned.
    /// The one this module cares about is [`blend_from_id`], which searches it
    /// and would answer `None` and fall through to whatever `composite-op`
    /// says. **It is not the worst of the three**, and saying otherwise would
    /// be the overclaim this guard exists to prevent: the other two are the
    /// layer blend dropdown in `panels.rs` and the brush blend dropdown in
    /// `ui.rs`, both of which build themselves by iterating `ALL`, so a mode
    /// left out of it is one nobody can select in the first place. That is the
    /// failure `CLAUDE.md` already names for the brush editor — "adding one
    /// means adding a control, or the library can use a brush nobody can
    /// make".
    ///
    /// The guard is the exhaustive `match`, which fails the **build** when a
    /// mode is added. That has to be a compile error rather than an assertion,
    /// because a test that iterates `ALL` can only check the entries that are
    /// in it and so agrees with itself however short the array is.
    ///
    /// It sits here rather than beside the enum because this is where the test
    /// module that already pins the mode names is, and the two are one
    /// subject. That is a weaker reason than the twin guard in `history.rs`
    /// has for sitting beside `EditKind` — if these are ever made consistent,
    /// beside the enum is the better home for both, since the dropdowns above
    /// are the bigger consumer and neither lives here.
    ///
    /// The arms index `ALL`, so an arm added for a sixth mode the obvious way
    /// — `BlendMode::ALL[5]` — is an out-of-bounds index into a fixed-size
    /// array and fails the build a second time when `ALL` was not extended.
    /// Any arm that does not index its *own* position still slips through,
    /// for the reason set out at `history::tests::listed_in_all`, which is
    /// where the hole was measured.
    #[test]
    fn all_lists_every_blend_mode() {
        // The positions are `ALL`'s, which is grouped for the menu rather than
        // ordered by discriminant — so these are deliberately not 0..n in
        // variant order, and an arm that simply counted upwards would fail.
        const fn listed_in_all(mode: BlendMode) -> BlendMode {
            match mode {
                BlendMode::Normal => BlendMode::ALL[0],
                BlendMode::Darken => BlendMode::ALL[1],
                BlendMode::Multiply => BlendMode::ALL[2],
                BlendMode::ColorBurn => BlendMode::ALL[3],
                BlendMode::LinearBurn => BlendMode::ALL[4],
                BlendMode::Lighten => BlendMode::ALL[5],
                BlendMode::Screen => BlendMode::ALL[6],
                BlendMode::ColorDodge => BlendMode::ALL[7],
                BlendMode::Add => BlendMode::ALL[8],
                BlendMode::AddGlow => BlendMode::ALL[9],
                BlendMode::Overlay => BlendMode::ALL[10],
                BlendMode::SoftLight => BlendMode::ALL[11],
                BlendMode::HardLight => BlendMode::ALL[12],
                BlendMode::VividLight => BlendMode::ALL[13],
                BlendMode::LinearLight => BlendMode::ALL[14],
                BlendMode::PinLight => BlendMode::ALL[15],
                BlendMode::Difference => BlendMode::ALL[16],
                BlendMode::Exclusion => BlendMode::ALL[17],
                BlendMode::Subtract => BlendMode::ALL[18],
                BlendMode::Divide => BlendMode::ALL[19],
                BlendMode::Hue => BlendMode::ALL[20],
                BlendMode::Saturation => BlendMode::ALL[21],
                BlendMode::Color => BlendMode::ALL[22],
                BlendMode::Luminosity => BlendMode::ALL[23],
            }
        }

        // Each arm has to hand back the mode it was reached by, and no
        // position may be listed twice — the first catches a *listed* mode's
        // arm pointing at the wrong entry, which is what a reordered `ALL`
        // looks like, and the second catches a mode being *replaced* in the
        // array rather than added to it, which the first cannot see on its own
        // because the mode that fell out is then never iterated.
        for mode in BlendMode::ALL {
            assert_eq!(listed_in_all(mode), mode, "{mode:?} is listed wrongly");
        }
        for (i, mode) in BlendMode::ALL.iter().enumerate() {
            assert!(
                !BlendMode::ALL[..i].contains(mode),
                "`BlendMode::ALL` lists {mode:?} twice, so a mode is missing"
            );
        }
    }

    #[test]
    fn save_replaces_the_file_only_once_it_has_written_all_of_it() {
        // `temp_dir`'s, which carries the process id. This asks for the
        // *directory* to be free of leftovers now rather than for one named
        // file to be absent, so a second `cargo test` running beside this one
        // in the same fixed directory would have made it fail on somebody
        // else's temporary — which is a flake that looks like a real defect.
        let dir = temp_dir("save");
        let path = dir.join("sketch.ora");

        let size = UVec2::new(2, 2);
        let pixels = solid(size, [7, 7, 7, 255]);
        let layers = vec![layer("Ink", &pixels)];
        save(
            &path,
            &SaveDocument {
                size,
                layers: &layers,
                active: 0,
                background: Background::Transparent,
                dpi: Document::DEFAULT_DPI,
                merged: Canvas::Held(&pixels),
                history: None,
            },
        )
        .unwrap();

        assert!(docimport::import(&path).is_ok());
        assert!(
            leftovers(&dir).is_empty(),
            "the temporary file was left behind: {:?}",
            leftovers(&dir)
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Names left in `dir` that say they are one of [`write_with`]'s
    /// temporaries.
    ///
    /// A **substring** test, deliberately. The temporary carries a process id
    /// and a counter now — see [`write_with`] for why it has to — so an
    /// assertion naming `"<stem>.saving"` exactly would pass whatever was left
    /// behind, which is a guard that stops guarding without ever failing.
    fn leftovers(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.contains(".saving"))
                    .collect()
            })
            .unwrap_or_default()
    }

    // --- streaming and deferred buffers ------------------------------------

    /// A document with one of everything this module writes, in both forms.
    ///
    /// Both halves are built from the *same* buffers and describe the same
    /// document; the only difference is whether each canvas-sized buffer is
    /// [`Canvas::Held`] or [`Canvas::Deferred`]. That is what makes a byte
    /// comparison between them a comparison of the two code paths and not of
    /// two documents.
    struct BothWays {
        size: UVec2,
        /// One canvas per stack entry, and **no two alike**. Sharing one buffer
        /// across the layers is the shape this fixture started with and it left
        /// a hole: with identical bytes on every layer, a resolver asked for the
        /// wrong *index* writes a byte-identical archive, so the comparison
        /// could not see a layer mix-up at all. Index 1 is the folder's and is
        /// never read.
        pixels: Vec<Vec<u8>>,
        mask: Vec<u8>,
        merged: Vec<u8>,
        effects: Vec<Effect>,
        text: crate::textobj::TextObject,
    }

    impl BothWays {
        fn new() -> Self {
            let size = UVec2::new(6, 5);
            // Not flat: `trim` crops to the non-transparent box and a PNG
            // filters each row against the one above, so a document of one
            // colour would compare equal under almost any bug in either.
            let canvas = |step: u32, hole: bool| -> Vec<u8> {
                (0..size.x * size.y)
                    .flat_map(|i| {
                        let v = (i * step % 251) as u8;
                        [v, v / 2, 255 - v, if hole && i == 0 { 0 } else { 255 }]
                    })
                    .collect()
            };
            Self {
                size,
                pixels: vec![
                    canvas(37, true),
                    Vec::new(),
                    canvas(53, false),
                    canvas(71, true),
                ],
                mask: (0..size.x * size.y)
                    .flat_map(|i| {
                        let v = (i * 11 % 253) as u8;
                        [v, v, v, 255]
                    })
                    .collect(),
                merged: (0..size.x * size.y)
                    .flat_map(|i| {
                        let v = (i * 61 % 249) as u8;
                        [255 - v, v, v / 3, 255]
                    })
                    .collect(),
                effects: vec![Effect::drop_shadow()],
                text: text_object(),
            }
        }

        /// Folder, masked layer, text layer, effected layer, background, a
        /// blend mode that warns — everything with a branch in `write_archive`.
        fn layers(&self, deferred: bool) -> Vec<SaveLayer<'_>> {
            let px = |at: usize| match deferred {
                true => Canvas::Deferred,
                false => Canvas::Held(&self.pixels[at]),
            };
            let mask = || match deferred {
                true => Canvas::Deferred,
                false => Canvas::Held(&self.mask),
            };
            vec![
                SaveLayer {
                    mask: Some(mask()),
                    locked: true,
                    ..SaveLayer::new("Wash", BlendMode::Add, px(0))
                },
                SaveLayer::folder("Group", 0, true),
                SaveLayer {
                    depth: 1,
                    clipped: true,
                    link: Some(2),
                    effects: &self.effects,
                    ..SaveLayer::new("Shadowed", BlendMode::Multiply, px(2))
                },
                SaveLayer {
                    depth: 1,
                    text: Some(&self.text),
                    ..SaveLayer::new("Caption", BlendMode::Normal, px(3))
                },
            ]
        }

        fn document<'a>(&'a self, layers: &'a [SaveLayer<'a>], deferred: bool) -> SaveDocument<'a> {
            SaveDocument {
                size: self.size,
                layers,
                active: 2,
                background: Background::WHITE,
                dpi: 300.0,
                merged: match deferred {
                    true => Canvas::Deferred,
                    false => Canvas::Held(&self.merged),
                },
                history: None,
            }
        }
    }

    /// A source that serves every buffer out of one reused allocation, and
    /// records what it was asked for.
    ///
    /// **"One canvas at a time" is the borrow checker's guarantee and not this
    /// type's**, and saying otherwise was an overclaim worth retracting:
    /// `Canvases` hands back a `Cow<'_, [u8]>` borrowed from `&mut self`, so a
    /// writer holding one while asking for the next does not compile. Reusing
    /// the scratch demonstrates that a caller *may* work that way — it is what
    /// a source reading into a fixed buffer would do — and demonstrates nothing
    /// about the writer.
    ///
    /// What this does carry is `asked`, which pins the *order*, and that is not
    /// decoration: fetching a buffer early and holding it across the archive
    /// produces byte-identical output, so the sequence is the only reading that
    /// can tell "one at a time, in archive order" from "all of them up front".
    struct OneAtATime<'a> {
        fixture: &'a BothWays,
        scratch: Vec<u8>,
        /// What was asked for, in order.
        asked: Vec<String>,
    }

    impl OneAtATime<'_> {
        fn serve(&mut self, from: Which, what: String) -> Result<Cow<'_, [u8]>, SaveError> {
            self.asked.push(what);
            // The fixture is behind a shared reference, so copying the handle
            // out ends the borrow conflict with `&mut self` — no clone of a
            // canvas, which would have made the "one buffer" above false in
            // the fixture itself.
            let fixture = self.fixture;
            self.scratch.clear();
            self.scratch.extend_from_slice(match from {
                Which::Pixels(at) => &fixture.pixels[at],
                Which::Mask => &fixture.mask,
                Which::Merged => &fixture.merged,
            });
            Ok(Cow::Borrowed(&self.scratch))
        }
    }

    enum Which {
        Pixels(usize),
        Mask,
        Merged,
    }

    impl Canvases for OneAtATime<'_> {
        fn layer(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
            self.serve(Which::Pixels(index), format!("layer {index}"))
        }

        fn mask(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
            self.serve(Which::Mask, format!("mask {index}"))
        }

        fn merged(&mut self) -> Result<Cow<'_, [u8]>, SaveError> {
            self.serve(Which::Merged, "merged".to_string())
        }
    }

    /// A directory of this test's own, emptied first.
    ///
    /// The pid is not enough on its own now that these tests *sweep* the
    /// directory rather than asking about one name: a previous run that crashed
    /// with the same pid leaves a temporary this run would blame itself for.
    /// `autosave::tests::scratch` clears for the same reason.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("umber-docformat-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The whole point of the change, stated as the only thing that can settle
    /// it: the file a streamed save writes is the archive `encode` builds,
    /// byte for byte.
    ///
    /// It is a memory change and not a format change, so anything less than a
    /// byte comparison would be arguing about it. The fixture carries a folder,
    /// a mask, a text record, effects, a background, a link, a lock, a clip and
    /// a blend mode that warns, because every one of those is a branch in
    /// `write_archive` and a branch nothing drove is a branch this cannot speak
    /// for.
    #[test]
    fn a_streamed_save_is_byte_for_byte_the_archive_encode_builds() {
        let fixture = BothWays::new();

        let held = fixture.layers(false);
        let (expected, held_warnings) =
            encode(&fixture.document(&held, false)).expect("encode held");

        let deferred = fixture.layers(true);
        let mut source = OneAtATime {
            fixture: &fixture,
            scratch: Vec::new(),
            asked: Vec::new(),
        };
        let dir = temp_dir("streamed");
        let path = dir.join("streamed.ora");
        let streamed_warnings = save_from(&path, &fixture.document(&deferred, true), &mut source)
            .expect("streamed save");
        let written = std::fs::read(&path).expect("read back");

        assert_eq!(
            written,
            expected,
            "the streamed archive is not the encoded one ({} bytes against {})",
            written.len(),
            expected.len()
        );
        assert_eq!(streamed_warnings, held_warnings);
        assert!(
            !held_warnings.is_empty(),
            "the fixture stopped exercising the warning path"
        );

        // Asked for in archive order — top of the stack first, the flattened
        // image last — which is what lets a caller that can only produce one at
        // a time produce them at all.
        assert_eq!(
            source.asked,
            ["layer 3", "layer 2", "layer 0", "mask 0", "merged",],
            "the writer asked in the wrong order, or asked twice"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// The same, through the in-memory encoder rather than the file, so the
    /// deferred path is pinned independently of the streaming one.
    ///
    /// Without this a bug in `resolve` and a bug in the streaming could cancel
    /// out, and the test above would still pass.
    #[test]
    fn a_fetched_buffer_writes_what_a_held_one_does() {
        let fixture = BothWays::new();
        let held = fixture.layers(false);
        let (expected, _) = encode(&fixture.document(&held, false)).expect("encode held");

        let deferred = fixture.layers(true);
        let mut source = OneAtATime {
            fixture: &fixture,
            scratch: Vec::new(),
            asked: Vec::new(),
        };
        let dir = temp_dir("fetched");
        let path = dir.join("fetched.ora");
        save_from(&path, &fixture.document(&deferred, true), &mut source).expect("save");
        // Read back through the real reader as well as compared: equal bytes
        // that neither of them can open would be equally wrong.
        let written = std::fs::read(&path).expect("read back");
        assert_eq!(written, expected);
        let opened = docimport::read_openraster(&written).expect("reopens");
        assert_eq!(opened.layers.len(), 4, "the folder and three layers");
        let _ = std::fs::remove_file(&path);
    }

    /// A deferred buffer handed to the two functions that have no source is
    /// refused, and the document is not written.
    ///
    /// The alternative is a layer written blank, which is a document silently
    /// damaged by its own save — so this is refused loudly rather than
    /// defaulted quietly.
    #[test]
    fn a_deferred_buffer_with_nothing_to_fetch_it_is_refused() {
        let size = UVec2::new(2, 2);
        let pixels = solid(size, [9, 9, 9, 255]);

        let deferred = vec![SaveLayer::new("Ink", BlendMode::Normal, Canvas::Deferred)];
        let doc = SaveDocument {
            size,
            layers: &deferred,
            active: 0,
            background: Background::Transparent,
            dpi: Document::DEFAULT_DPI,
            merged: Canvas::Held(&pixels),
            history: None,
        };
        assert!(
            matches!(encode(&doc), Err(SaveError::NotSupplied { .. })),
            "encode accepted a buffer it has no way to fetch"
        );

        let dir = temp_dir("nosource");
        let path = dir.join("refused.ora");
        let _ = std::fs::remove_file(&path);
        assert!(matches!(
            save(&path, &doc),
            Err(SaveError::NotSupplied { .. })
        ));
        assert!(!path.exists(), "a refused save wrote a file");

        // And the merged image is refused on the same terms, which is the arm
        // reached last and therefore the one a partial fix would leave.
        let held = vec![SaveLayer::new("Ink", BlendMode::Normal, &pixels)];
        assert!(matches!(
            encode(&SaveDocument {
                merged: Canvas::Deferred,
                layers: &held,
                ..doc
            }),
            Err(SaveError::NotSupplied { .. })
        ));
    }

    /// A source that fails partway through leaves the artist's file exactly as
    /// it was, and no temporary beside it.
    ///
    /// This is the guarantee the temp-and-rename has always made, asked of the
    /// new failure it makes possible: a readback that could not map now happens
    /// *while* the archive is being written rather than before it starts.
    #[test]
    fn a_save_whose_source_fails_halfway_leaves_the_old_file_alone() {
        struct FailsOnTheSecond<'a> {
            fixture: &'a BothWays,
            served: usize,
        }
        impl Canvases for FailsOnTheSecond<'_> {
            fn layer(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
                self.served += 1;
                if self.served > 1 {
                    return Err(SaveError::Io(std::io::Error::other("the device went away")));
                }
                Ok(Cow::Borrowed(&self.fixture.pixels[index]))
            }
            fn mask(&mut self, _: usize) -> Result<Cow<'_, [u8]>, SaveError> {
                Ok(Cow::Borrowed(&self.fixture.mask))
            }
            fn merged(&mut self) -> Result<Cow<'_, [u8]>, SaveError> {
                Ok(Cow::Borrowed(&self.fixture.merged))
            }
        }

        let fixture = BothWays::new();
        let dir = temp_dir("halfway");
        let path = dir.join("precious.ora");
        std::fs::write(&path, b"the artist's last good file").unwrap();

        let deferred = fixture.layers(true);
        let mut source = FailsOnTheSecond {
            fixture: &fixture,
            served: 0,
        };
        assert!(save_from(&path, &fixture.document(&deferred, true), &mut source).is_err());

        assert_eq!(
            std::fs::read(&path).unwrap(),
            b"the artist's last good file",
            "a failed save replaced the file it was meant to protect"
        );
        assert!(
            leftovers(&dir).is_empty(),
            "the temporary was left behind: {:?}",
            leftovers(&dir)
        );
        let _ = std::fs::remove_file(&path);
    }

    /// **A sink that gives out while the archive is being built is a refusal,
    /// wherever in the archive it gives out.**
    ///
    /// This is the failure streaming introduces: the disk used to fill up after
    /// the archive existed, and now it fills up *during* it, with a PNG encoder
    /// and a ZIP writer both part-way through. The budget is swept across the
    /// whole archive so the refusal lands inside a layer's PNG, between
    /// entries, inside `stack.xml` and inside the central directory — one
    /// chosen figure would drive whichever of those it happened to hit.
    ///
    /// **It found a panic, which is why it is written this way.** Handed an
    /// I/O error part-way through an entry, zip 8.6.0 finalises the entry it
    /// was in the middle of and trips
    /// `debug_assert!(file_end >= self.stats.start)` — so a full disk during a
    /// save was a panic in any build with debug assertions on. [`Watched`] is
    /// what keeps the error away from it, and this is what says so: remove it
    /// and this test aborts.
    ///
    /// **Every budget, not a sample, and that is the difference between a guard
    /// and a coin toss.** The panicking budgets are twelve-byte windows, one per
    /// entry — the header patch seeks back, the write fails part-way, and the
    /// position is left behind where the entry started — so about 3% of the
    /// range aborts and the rest merely refuses. A stride sweep meets one or
    /// none depending on arithmetic nobody controls, and re-rolls silently every
    /// time the fixture changes. Sweeping the lot costs a fraction of a second.
    ///
    /// **What it deliberately does not claim is that it covers `finish_png`'s
    /// explicit `finish`.** Nothing can, and now less than ever: `Watched`
    /// absorbs, so the PNG encoder never sees a failure at all. See that
    /// function.
    #[test]
    fn a_sink_that_gives_out_is_a_refusal_wherever_it_gives_out() {
        /// Accepts `budget` bytes and then refuses, over a real cursor so the
        /// `Seek` the ZIP writer needs still works.
        struct Fills {
            inner: std::io::Cursor<Vec<u8>>,
            budget: usize,
        }
        impl Write for Fills {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.budget == 0 {
                    return Err(std::io::Error::other("no space left on device"));
                }
                let take = buf.len().min(self.budget);
                self.budget -= take;
                self.inner.write(&buf[..take])
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }
        impl std::io::Seek for Fills {
            fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(to)
            }
        }

        let fixture = BothWays::new();
        let held = fixture.layers(false);
        let (whole, _) = encode(&fixture.document(&held, false)).expect("encode");

        for budget in 0..whole.len() {
            let sink = Fills {
                inner: std::io::Cursor::new(Vec::new()),
                budget,
            };
            // Through `stream_archive`, which is exactly what `save_from`
            // composes — built *into* the failing sink, so the refusal happens
            // inside whichever encoder was mid-entry. Writing a *finished*
            // archive at a failing sink, which is what this drove at first,
            // tests `write_all` and nothing here.
            let out = stream_archive(sink, &fixture.document(&held, false), &mut NoCanvases);
            assert!(
                out.is_err(),
                "a sink that stopped after {budget} of {} bytes reported success",
                whole.len()
            );
        }

        // And end to end, through the real writer into a real path: an
        // encoder that cannot read must leave the file that was there.
        struct Breaks;
        impl Canvases for Breaks {
            fn layer(&mut self, _: usize) -> Result<Cow<'_, [u8]>, SaveError> {
                Err(SaveError::Io(std::io::Error::other(
                    "no space left on device",
                )))
            }
            fn mask(&mut self, _: usize) -> Result<Cow<'_, [u8]>, SaveError> {
                unreachable!()
            }
            fn merged(&mut self) -> Result<Cow<'_, [u8]>, SaveError> {
                unreachable!()
            }
        }
        let dir = temp_dir("sink");
        let path = dir.join("kept.ora");
        std::fs::write(&path, b"still theirs").unwrap();
        let deferred = fixture.layers(true);
        assert!(save_from(&path, &fixture.document(&deferred, true), &mut Breaks).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"still theirs");
        assert!(leftovers(&dir).is_empty(), "{:?}", leftovers(&dir));
        let _ = std::fs::remove_file(&path);
    }

    /// The `.ora` a save writes is the same file on every machine, still.
    ///
    /// `PrettyConfig::new()` taking the platform's line ending was a real bug
    /// in the effects record, and streaming the archive is exactly the sort of
    /// change that could reintroduce one — a `BufWriter` over a file is not a
    /// `Cursor` over a `Vec`, and a writer that translated anything would be
    /// invisible until two people compared two saves.
    #[test]
    fn a_streamed_archive_carries_no_platform_line_endings() {
        let fixture = BothWays::new();
        let deferred = fixture.layers(true);
        let mut source = OneAtATime {
            fixture: &fixture,
            scratch: Vec::new(),
            asked: Vec::new(),
        };
        let dir = temp_dir("endings");
        let path = dir.join("endings.ora");
        save_from(&path, &fixture.document(&deferred, true), &mut source).expect("save");
        let written = std::fs::read(&path).expect("read back");

        // Found by name rather than by index, because the archive numbers
        // entries top-first and the fixture is written bottom-first — a
        // hand-computed index here would be a second statement of that
        // reversal, and one that fails as a missing entry rather than as the
        // thing this is about.
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(&written)).unwrap();
        let names: Vec<String> = zip.file_names().map(str::to_string).collect();
        let effects = names
            .iter()
            .find(|n| n.starts_with("umber/effects/"))
            .expect("the fixture stopped carrying an effects record")
            .clone();
        for name in [effects.as_str(), "stack.xml"] {
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut zip.by_name(name).unwrap(), &mut body).unwrap();
            assert!(
                !body.windows(2).any(|w| w == b"\r\n"),
                "{name} carries a Windows line ending"
            );
        }
        let _ = std::fs::remove_file(&path);
    }

    /// The `stack.xml` out of an archive, as text.
    fn read_stack_xml(bytes: &[u8]) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut zip.by_name("stack.xml").unwrap(), &mut body).unwrap();
        String::from_utf8(body).unwrap()
    }

    /// Rebuild an archive with one entry's bytes replaced.
    ///
    /// What an older build that painted on a text layer leaves behind: the same
    /// document with a different layer PNG in it, and the text record still
    /// beside it.
    fn with_entry(bytes: &[u8], target: &str, body: Vec<u8>) -> Vec<u8> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut held = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut held).unwrap();
            out.start_file(&name, stored()).unwrap();
            out.write_all(if name == target { &body } else { &held })
                .unwrap();
        }
        out.finish().unwrap().into_inner()
    }

    /// Rebuild an archive with `stack.xml` passed through `f`.
    fn with_stack_xml(bytes: &[u8], f: impl Fn(String) -> String) -> Vec<u8> {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut out = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).unwrap();
            let name = entry.name().to_string();
            let mut body = Vec::new();
            std::io::Read::read_to_end(&mut entry, &mut body).unwrap();
            if name == "stack.xml" {
                body = f(String::from_utf8(body).unwrap()).into_bytes();
            }
            out.start_file(&name, stored()).unwrap();
            out.write_all(&body).unwrap();
        }
        out.finish().unwrap().into_inner()
    }
}
