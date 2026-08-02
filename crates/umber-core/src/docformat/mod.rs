//! Writing documents — Umber's own saved-file format.
//!
//! ```no_run
//! # use std::path::Path;
//! # use glam::UVec2;
//! # let layers: Vec<umber_core::docformat::SaveLayer> = vec![];
//! # let merged = vec![];
//! let doc = umber_core::docformat::SaveDocument {
//!     size: UVec2::new(2048, 2048),
//!     layers: &layers,
//!     active: 0,
//!     background: umber_core::Background::WHITE,
//!     dpi: 72.0,
//!     merged: &merged,
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

pub mod history;

use std::io::Write;
use std::path::Path;

use glam::UVec2;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::color::Color;
use crate::docimport::srgb;
use crate::document::Background;
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
pub const VERSION: u32 = 2;

/// The lowest revision that describes `layers`.
///
/// The point is that a document with no masks and no clipping still declares
/// **1** and therefore still opens in every Umber that came before. Writing
/// [`VERSION`] unconditionally would lock every file this build touches away
/// from older ones in exchange for nothing: a revision number is a statement
/// about what a file *contains*, not about what wrote it.
fn required_version(layers: &[SaveLayer<'_>]) -> u32 {
    if layers.iter().any(|l| l.mask.is_some() || l.clipped) {
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

/// One layer on its way to disk.
pub struct SaveLayer<'a> {
    pub name: &'a str,
    pub visible: bool,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub blend: BlendMode,
    /// `width * height * 4` bytes in layer-texture form — sRGB-encoded with
    /// alpha premultiplied in linear space. See the module docs.
    pub pixels: &'a [u8],
    /// The layer's mask slice, canvas-sized and in the same form as `pixels`.
    ///
    /// Only the **red** channel is written — a mask is coverage, and the slice
    /// carries the same value in all three colour channels. See
    /// [`MASK_ATTR`].
    pub mask: Option<&'a [u8]>,
    /// Bounded by the alpha of the nearest unclipped layer below.
    pub clipped: bool,
    /// Refuses edits until unlocked.
    pub locked: bool,
    /// Which link group this layer belongs to, if any. See [`LINK_GROUP_ATTR`].
    pub link: Option<u8>,
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
    pub fn new(name: &'a str, blend: BlendMode, pixels: &'a [u8]) -> Self {
        Self {
            name,
            visible: true,
            opacity: 1.0,
            blend,
            pixels,
            mask: None,
            clipped: false,
            locked: false,
            link: None,
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
            ..Self::new(name, BlendMode::Normal, &[])
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
    pub merged: &'a [u8],
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
/// The file is written whole to a temporary neighbour and renamed into place.
/// A save that fails halfway — a full disk, a pulled USB stick — would
/// otherwise leave a truncated archive where the artist's last good version
/// used to be, which is the one failure a save must not have.
pub fn save(path: &Path, doc: &SaveDocument<'_>) -> Result<Vec<SaveWarning>, SaveError> {
    let (bytes, warnings) = encode(doc)?;
    write_encoded(path, &bytes)?;
    Ok(warnings)
}

/// Put an already-[`encode`]d archive at `path`, whole or not at all.
///
/// The file is written to a temporary neighbour and renamed into place. A write
/// that fails halfway — a full disk, a pulled USB stick — would otherwise leave
/// a truncated archive where the artist's last good version used to be, which
/// is the one failure a save must not have. That matters more for an autosave
/// than for a save, because nobody is watching it happen.
///
/// Separate from [`save`] because the autosave writes one archive to *two*
/// places — the document's own file and an internal copy — and encoding a
/// document twice to do it would double the only expensive part. Having the
/// atomic write in one place is what stops the second caller reinventing it
/// slightly differently. [`crate::export`] is the third caller, which is why
/// the temporary is named after the *target's* extension rather than after
/// `.ora`: for a document that is the same `sketch.ora.saving` it always was,
/// and for an exported `sketch.png` it is `sketch.png.saving` rather than a
/// name that would collide with the save of a document beside it.
pub fn write_encoded(path: &Path, bytes: &[u8]) -> Result<(), SaveError> {
    let mut temporary = path.to_path_buf().into_os_string();
    temporary.push(".saving");
    let temporary = std::path::PathBuf::from(temporary);
    std::fs::write(&temporary, bytes)?;
    if let Err(e) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(SaveError::Io(e));
    }
    Ok(())
}

/// Build the archive in memory.
///
/// Separate from [`save`] so the round-trip tests can write and read without
/// touching a disk.
pub fn encode(doc: &SaveDocument<'_>) -> Result<(Vec<u8>, Vec<SaveWarning>), SaveError> {
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
    for layer in doc.layers.iter().filter(|l| !l.folder) {
        if layer.pixels.len() != expected {
            return Err(SaveError::WrongSize {
                what: format!("pixels of layer “{}”", layer.name),
                found: layer.pixels.len(),
                expected,
            });
        }
        if let Some(mask) = layer.mask
            && mask.len() != expected
        {
            return Err(SaveError::WrongSize {
                what: format!("mask of layer “{}”", layer.name),
                found: mask.len(),
                expected,
            });
        }
    }
    if doc.merged.len() != expected {
        return Err(SaveError::WrongSize {
            what: "flattened image".to_string(),
            found: doc.merged.len(),
            expected,
        });
    }

    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));

    // The specification requires `mimetype` first and uncompressed, and real
    // readers — Umber's own included — check it.
    zip.start_file("mimetype", stored())?;
    zip.write_all(b"image/openraster")?;

    let mut warnings = Vec::new();
    let mut entries = Vec::with_capacity(doc.layers.len());

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
        let selected = doc.layers.len() - 1 - i == doc.active;
        if layer.folder {
            entries.push(Entry {
                depth: layer.depth,
                folder: true,
                xml: folder_xml(layer, selected),
            });
            continue;
        }
        let src = format!("data/layer{i:03}.png");
        let placed = trim(layer.pixels, doc.size);
        let png = encode_png(placed.size, &placed.pixels)?;
        zip.start_file(&src, stored())?;
        zip.write_all(&png)?;

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
                let grey: Vec<u8> = mask.chunks_exact(4).map(|px| px[0]).collect();
                zip.start_file(&src, stored())?;
                zip.write_all(&encode_png_grey(doc.size, &grey)?)?;
                Some(src)
            }
            None => None,
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
            ),
        });
    }

    // The background goes in last, so it is the bottom of the stack — and it is
    // a real layer with real pixels, because every application but this one
    // would otherwise open the document on transparency. See the module docs.
    if let Some(colour) = doc.background.colour() {
        zip.start_file(BACKGROUND_SRC, stored())?;
        zip.write_all(&encode_png(doc.size, &solid(doc.size, colour))?)?;
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
        Some(h) if !h.is_empty() => history::write(&mut zip, doc.size, &names, h)?,
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
    // manager something to show.
    zip.start_file("mergedimage.png", stored())?;
    zip.write_all(&encode_png(doc.size, doc.merged)?)?;

    let (thumb_size, thumb) = thumbnail(doc.merged, doc.size);
    zip.start_file("Thumbnails/thumbnail.png", stored())?;
    zip.write_all(&encode_png(thumb_size, &thumb)?)?;

    Ok((zip.finish()?.into_inner(), warnings))
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
    }
}

/// Stable name for [`BLEND_ATTR`]. The debug spelling, so it cannot drift out
/// of step with the enum the way a second hand-written table would.
pub fn blend_id(mode: BlendMode) -> String {
    format!("{mode:?}")
}

/// Inverse of [`blend_id`]. An unrecognised name comes from a version that has
/// modes this one does not, and yields `None` so the reader falls back to the
/// `composite-op` every ORA also carries.
pub fn blend_from_id(id: &str) -> Option<BlendMode> {
    BlendMode::ALL.into_iter().find(|m| blend_id(*m) == id)
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
/// `umber-selected` can land here: a folder is selectable, and a document
/// saved with one in hand must reopen with it in hand.
fn folder_xml(layer: &SaveLayer<'_>, selected: bool) -> String {
    let mut out = format!(
        "<stack name=\"{}\" visibility=\"{}\"",
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
fn encode_png_grey(size: UVec2, grey: &[u8]) -> Result<Vec<u8>, SaveError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size.x, size.y);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(grey))
        .map_err(|e| SaveError::Io(std::io::Error::other(e)))?;
    Ok(out)
}

fn encode_png(size: UVec2, rgba: &[u8]) -> Result<Vec<u8>, SaveError> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, size.x, size.y);
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
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(rgba))
        .map_err(|e| SaveError::Io(std::io::Error::other(e)))?;
    Ok(out)
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
            merged: &px,
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
            merged: &px,
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
            merged: &px,
            history: Some(saved),
        });
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        let opened = doc.open();
        assert_eq!(opened.history.len(), 1, "the history came back");
        let slot = opened.history.entry_at(0).unwrap().patch().unwrap().slot;
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
            merged: &merged,
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
                mask: Some(&mask),
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
            merged: &pixels,
            history: None,
        })
        .expect("encode");
        assert!(warnings.is_empty(), "{warnings:?}");

        // A document that carries a mask needs the revision that was raised
        // for one, so an older build refuses it rather than opening a picture
        // with the mask silently gone.
        assert!(
            read_stack_xml(&bytes).contains(&format!("{VERSION_ATTR}=\"{VERSION}\"")),
            "a masked document must declare the revision it needs"
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &merged,
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
            merged: &merged,
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
            merged: &painted,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &pixels,
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
            merged: &full,
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

    #[test]
    fn save_replaces_the_file_only_once_it_has_written_all_of_it() {
        let dir = std::env::temp_dir().join("umber-docformat-save");
        std::fs::create_dir_all(&dir).unwrap();
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
                merged: &pixels,
                history: None,
            },
        )
        .unwrap();

        assert!(docimport::import(&path).is_ok());
        assert!(
            !path.with_extension("ora.saving").exists(),
            "the temporary file was left behind"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The `stack.xml` out of an archive, as text.
    fn read_stack_xml(bytes: &[u8]) -> String {
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut zip.by_name("stack.xml").unwrap(), &mut body).unwrap();
        String::from_utf8(body).unwrap()
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
