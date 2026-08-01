//! Reading back the undo history [`crate::docformat::history`] writes.
//!
//! The governing rule is the one in the writer's module docs: **a history that
//! replays into the wrong layer is far worse than no saved history.** So this
//! reads defensively and drops the whole thing at the first thing that does not
//! line up — the manifest's canvas, its layer names, an entry naming a layer
//! that is not there, a patch whose PNG is missing or the wrong size. There is
//! no half-restored state, because the entries are a sequence in which each
//! restores the pixels the next one expects; one missing from the middle is not
//! a shorter history but a wrong one.
//!
//! A drop is reported as an [`ImportWarning`], and only ever when the file
//! actually carried a history. A file that has none — every ORA from every
//! other application, and every document Umber wrote before this — says
//! nothing, which is what keeps the warning list worth reading.

use glam::UVec2;

use super::container::{self, Zip};
use super::{ImportWarning, SourceFormat, flat};
use crate::docformat::history::{self as fmt, Manifest};
use crate::geom::PixelRect;
use crate::history::EditKind;
use crate::time::Timestamp;

/// One recorded edit as it came out of a file.
///
/// `layer` is a **stack position**, bottom first — never a texture slot. See
/// [`crate::docformat::history`] for why the two must not be confused.
#[derive(Clone, Debug)]
pub struct ImportedEdit {
    pub layer: usize,
    pub kind: EditKind,
    /// When the edit was made, where the file says.
    ///
    /// `None` for a document written before the manifest carried times, and
    /// that stays `None` all the way to the History list, which shows an empty
    /// column for it. A time invented at import — the file's own modification
    /// date, say — would be indistinguishable from a recorded one and would
    /// make the list assert something nobody measured.
    pub at: Option<Timestamp>,
    pub rect: PixelRect,
    /// Layer-texture bytes, `rect.area() * 4` of them — sRGB-encoded with alpha
    /// premultiplied, exactly as `write_layer_rect` wants them.
    pub bytes: Vec<u8>,
}

/// An undo history read out of a document.
#[derive(Clone, Debug)]
pub struct ImportedHistory {
    /// Timeline order, oldest first.
    pub entries: Vec<ImportedEdit>,
    /// How many of them are applied.
    pub position: usize,
    /// How many older ones the budget had already dropped.
    pub dropped: usize,
}

/// Read the history named by `manifest_path`, or drop it and say why.
///
/// `layer_names` is the stack that actually loaded, bottom first: comparing
/// against it rather than against what `stack.xml` described is what catches a
/// layer that failed to decode, which would shift every position after it.
pub(crate) fn read(
    zip: &mut Zip<'_>,
    manifest_path: &str,
    canvas: UVec2,
    layer_names: &[String],
    warnings: &mut Vec<ImportWarning>,
) -> Option<ImportedHistory> {
    match load(zip, manifest_path, canvas, layer_names) {
        Ok(history) => Some(history),
        Err(reason) => {
            warnings.push(ImportWarning::HistoryDropped { reason });
            None
        }
    }
}

/// The same, with every refusal spelled out as the sentence the warning shows.
fn load(
    zip: &mut Zip<'_>,
    manifest_path: &str,
    canvas: UVec2,
    layer_names: &[String],
) -> Result<ImportedHistory, String> {
    let format = SourceFormat::OpenRaster;
    let bytes = container::read_optional_entry(zip, manifest_path, format)
        .map_err(|e| e.to_string())?
        .ok_or("the document says it has one, but the record is not in the file")?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| format!("the record of it could not be read ({e})"))?;

    if manifest.version > fmt::VERSION {
        return Err(format!(
            "it was written in a newer form than this build reads (history {}, \
             this build reads up to {})",
            manifest.version,
            fmt::VERSION
        ));
    }
    // A patch is a rectangle of a *particular* canvas. This is the same reason
    // resizing a document clears its history.
    if manifest.canvas != [canvas.x, canvas.y] {
        return Err(format!(
            "it was recorded on a {} × {} canvas and this document is {} × {}",
            manifest.canvas[0], manifest.canvas[1], canvas.x, canvas.y
        ));
    }
    // The fingerprint. Entries name layers by position, so a stack that is not
    // the one they were written against makes every position mean something
    // else — which is the failure this whole module exists to avoid.
    if manifest.layers != layer_names {
        return Err("the layers are no longer the ones it was recorded against".into());
    }
    if manifest.position > manifest.entries.len() {
        return Err("its record of how far back it had been stepped is out of range".into());
    }

    let mut entries = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        if entry.layer >= layer_names.len() {
            return Err("one of its entries names a layer this document does not have".into());
        }
        let kind = fmt::kind_from_id(&entry.kind)
            .ok_or("one of its entries records something this build cannot undo")?;
        // Bounds first: a rectangle running off the canvas would be written
        // back into whatever the arithmetic landed on.
        if entry.w == 0
            || entry.h == 0
            || entry.x.saturating_add(entry.w) > canvas.x
            || entry.y.saturating_add(entry.h) > canvas.y
        {
            return Err("one of its entries covers an area outside the canvas".into());
        }

        let png = container::read_optional_entry(zip, &entry.src, format)
            .map_err(|e| e.to_string())?
            .ok_or("part of it is missing from the file")?;
        let image = flat::decode_png(&png, format).map_err(|e| e.to_string())?;
        if image.size != UVec2::new(entry.w, entry.h) {
            return Err("part of it is not the size it says it is".into());
        }

        entries.push(ImportedEdit {
            layer: entry.layer,
            kind,
            // Not validated beyond being a number. There is no range a
            // timestamp can be *wrong* in — a clock really can be set to 1904
            // — and the one thing an absurd value could do downstream is make a
            // gap come out negative, which `Timestamp::since` already answers
            // by declining to report one.
            at: entry.at.map(Timestamp::from_unix_millis),
            rect: PixelRect {
                x: entry.x,
                y: entry.y,
                width: entry.w,
                height: entry.h,
            },
            // Unconverted: these are layer-texture bytes, not a picture anyone
            // else reads. See the writer's module docs.
            bytes: image.rgba,
        });
    }

    Ok(ImportedHistory {
        entries,
        position: manifest.position,
        dropped: manifest.dropped,
    })
}
