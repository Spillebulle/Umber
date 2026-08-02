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
#[derive(Clone, Debug)]
pub struct ImportedEdit {
    pub kind: EditKind,
    /// When the edit was made, where the file says.
    ///
    /// `None` for a document written before the manifest carried times, and
    /// that stays `None` all the way to the History list, which shows an empty
    /// column for it. A time invented at import — the file's own modification
    /// date, say — would be indistinguishable from a recorded one and would
    /// make the list assert something nobody measured.
    pub at: Option<Timestamp>,
    pub body: ImportedBody,
}

/// What one entry carries, mirroring [`crate::history::EditBody`].
#[derive(Clone, Debug)]
pub enum ImportedBody {
    Pixels {
        /// A **stack position**, bottom first — never a texture slot. See
        /// [`crate::docformat::history`] for why the two must not be confused.
        layer: usize,
        /// The patch belongs to that layer's **mask** rather than to its
        /// pixels. False for every entry of a document written before masks
        /// existed, which is what those entries meant.
        mask: bool,
        /// The whole region the stroke damaged.
        rect: PixelRect,
        /// The parts of it the stroke actually touched. One covering the whole
        /// of `rect` for an entry out of a revision-1 manifest, which is what
        /// that revision could say.
        pieces: Vec<ImportedPiece>,
    },
    /// A canvas flip: no layer, no rectangle and no pixels. Undoing it is
    /// flipping again.
    Flip,
}

/// One piece of a recorded edit.
#[derive(Clone, Debug)]
pub struct ImportedPiece {
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
        let kind = fmt::kind_from_id(&entry.kind)
            .ok_or("one of its entries records something this build cannot undo")?;
        // The kind decides the shape of the entry, so it is read before any of
        // the fields that only a pixel entry has. A flip names no layer and no
        // rectangle — the zeroes the writer put there are not a rectangle of
        // the canvas and must not be checked as one.
        if kind.flip_axis().is_some() {
            if !entry.pieces.is_empty() || !entry.src.is_empty() {
                return Err("one of its entries carries pixels it should not have".into());
            }
            entries.push(ImportedEdit {
                kind,
                at: entry.at.map(Timestamp::from_unix_millis),
                body: ImportedBody::Flip,
            });
            continue;
        }
        if entry.layer >= layer_names.len() {
            return Err("one of its entries names a layer this document does not have".into());
        }
        // Bounds first: a rectangle running off the canvas would be written
        // back into whatever the arithmetic landed on.
        if entry.w == 0
            || entry.h == 0
            || entry.x.saturating_add(entry.w) > canvas.x
            || entry.y.saturating_add(entry.h) > canvas.y
        {
            return Err("one of its entries covers an area outside the canvas".into());
        }

        // Revision 1 said one rectangle and named one PNG; revision 2 names the
        // pieces of it the stroke actually touched. Reading the former as a
        // single piece is the whole of the difference — see
        // [`fmt::VERSION`].
        let listed: Vec<(PixelRect, &str)> = if entry.pieces.is_empty() {
            vec![(
                PixelRect {
                    x: entry.x,
                    y: entry.y,
                    width: entry.w,
                    height: entry.h,
                },
                entry.src.as_str(),
            )]
        } else {
            entry
                .pieces
                .iter()
                .map(|p| {
                    (
                        PixelRect {
                            x: p.x,
                            y: p.y,
                            width: p.w,
                            height: p.h,
                        },
                        p.src.as_str(),
                    )
                })
                .collect()
        };

        let mut pieces = Vec::with_capacity(listed.len());
        for (rect, src) in listed {
            // Every piece bounded on its own. A piece is what gets written back
            // into the layer, so a rectangle running off the canvas here is the
            // one that would land on whatever the arithmetic reached.
            if rect.width == 0
                || rect.height == 0
                || rect.x.saturating_add(rect.width) > canvas.x
                || rect.y.saturating_add(rect.height) > canvas.y
            {
                return Err("one of its entries covers an area outside the canvas".into());
            }
            let png = container::read_optional_entry(zip, src, format)
                .map_err(|e| e.to_string())?
                .ok_or("part of it is missing from the file")?;
            let image = flat::decode_png(&png, format).map_err(|e| e.to_string())?;
            if image.size != UVec2::new(rect.width, rect.height) {
                return Err("part of it is not the size it says it is".into());
            }
            pieces.push(ImportedPiece {
                rect,
                // Unconverted: these are layer-texture bytes, not a picture
                // anyone else reads. See the writer's module docs.
                bytes: image.rgba,
            });
        }

        entries.push(ImportedEdit {
            kind,
            // Not validated beyond being a number. There is no range a
            // timestamp can be *wrong* in — a clock really can be set to 1904
            // — and the one thing an absurd value could do downstream is make a
            // gap come out negative, which `Timestamp::since` already answers
            // by declining to report one.
            at: entry.at.map(Timestamp::from_unix_millis),
            body: ImportedBody::Pixels {
                layer: entry.layer,
                mask: entry.mask,
                rect: PixelRect {
                    x: entry.x,
                    y: entry.y,
                    width: entry.w,
                    height: entry.h,
                },
                pieces,
            },
        });
    }

    Ok(ImportedHistory {
        entries,
        position: manifest.position,
        dropped: manifest.dropped,
    })
}
