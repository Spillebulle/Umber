//! Importing layered documents written by other painting applications.
//!
//! ```no_run
//! # use std::path::Path;
//! let doc = umber_core::docimport::import(Path::new("sketch.ora"))?;
//! for warning in &doc.warnings {
//!     eprintln!("{warning}");
//! }
//! let (document, stack, uploads) = doc.into_stack();
//! # Ok::<(), umber_core::docimport::ImportError>(())
//! ```
//!
//! # What is here and what is not
//!
//! Four formats land: OpenRaster (`.ora`), Krita (`.kra`), Photoshop (`.psd`)
//! and a flat `.png` as a single layer. Clip Studio, MediBang, Procreate, GIMP
//! and layered TIFF are declined, each for a reason written down in
//! `docs/document-import.md`. The governing rule is that an import which
//! produces subtly wrong pixels is worse than one that refuses: a refusal sends
//! the artist to export a PNG or an ORA, whereas a wrong import wastes an
//! afternoon before they notice the colours moved.
//!
//! The same rule shapes what happens *inside* a supported format. Umber has no
//! layer groups, no masks, no clipping and five blend modes, so a real
//! Photoshop file cannot arrive intact. Every such loss appends an
//! [`ImportWarning`], and the UI is expected to show them.
//!
//! # Pixel convention
//!
//! [`ImportedLayer::pixels`] is canvas-sized RGBA8 in exactly the form a layer
//! texture holds — sRGB-encoded, alpha premultiplied in linear space. It can go
//! straight to `queue.write_texture`. See [`srgb`] for why that is the right
//! form and what the wrong ones look like.

mod blend;
mod container;
mod flat;
mod krita;
mod lzf;
mod openraster;
mod photoshop;
mod srgb;

#[cfg(test)]
mod fixtures;

use std::fmt;
use std::path::Path;

use glam::UVec2;

use crate::document::Document;
use crate::layer::{BlendMode, LayerStack};

/// Formats [`import`] can read.
///
/// Ordered so that the file-open dialog can list the layered formats first.
pub fn supported_extensions() -> &'static [&'static str] {
    &["ora", "kra", "psd", "png"]
}

/// Read a document written by another application.
///
/// Dispatches on the file extension. The readers still check the file's own
/// magic, so a mislabelled file fails with [`ImportError::Malformed`] rather
/// than by misreading.
pub fn import(path: &Path) -> Result<ImportedDocument, ImportError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    // Checked before the read so that an unreadable name gives the useful
    // answer ("Umber cannot open .clip files") rather than an I/O error.
    if !supported_extensions().contains(&ext.as_str()) {
        return Err(ImportError::UnsupportedExtension(ext));
    }

    let bytes = std::fs::read(path)?;
    let doc = match ext.as_str() {
        "ora" => openraster::read(&bytes),
        "kra" => krita::read(&bytes),
        "psd" => photoshop::read(&bytes),
        "png" => flat::read_png(&bytes),
        _ => unreachable!("extension was checked against supported_extensions"),
    }?;

    doc.validate()?;
    Ok(doc)
}

/// Which application's format a document came out of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceFormat {
    OpenRaster,
    Krita,
    Photoshop,
    Png,
}

impl SourceFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::OpenRaster => "OpenRaster",
            Self::Krita => "Krita",
            Self::Photoshop => "Photoshop",
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
    /// Everything the import could not represent. Empty means nothing was lost.
    pub warnings: Vec<ImportWarning>,
}

/// A layer's pixels together with the texture-array slot they belong in.
pub struct LayerUpload {
    pub slot: u32,
    pub pixels: Vec<u8>,
}

impl ImportedDocument {
    pub fn document(&self) -> Document {
        Document::new(self.size.x, self.size.y)
    }

    /// Build the engine-side state for this import.
    ///
    /// Returns the uploads separately rather than hanging pixels off the stack:
    /// `LayerStack` deliberately holds no pixel data — it holds slots, and the
    /// pixels live on the GPU. The caller writes each `LayerUpload` into its
    /// slot and the document is open.
    pub fn into_stack(self) -> (Document, LayerStack, Vec<LayerUpload>) {
        let document = Document::new(self.size.x, self.size.y);
        let mut stack = LayerStack::new();
        let mut uploads = Vec::with_capacity(self.layers.len());

        // A fresh stack already has one layer, so only the rest are added. The
        // count is guaranteed to fit by `validate`.
        for _ in 1..self.layers.len() {
            stack.add();
        }

        for (i, layer) in self.layers.into_iter().enumerate() {
            let slot = {
                let dst = stack
                    .get_mut(i)
                    .expect("the stack was sized to the import; see `validate`");
                dst.name = layer.name;
                dst.visible = layer.visible;
                dst.opacity = layer.opacity;
                dst.blend = layer.blend;
                dst.slot()
            };
            uploads.push(LayerUpload {
                slot,
                pixels: layer.pixels,
            });
        }

        // The top layer is the one an artist expects to be painting on.
        stack.set_active(stack.len() - 1);
        (document, stack, uploads)
    }

    /// Largest canvas edge an import will accept.
    ///
    /// Not a GPU limit — `umber-core` cannot see the adapter — but a sanity
    /// bound: 16384 is the ceiling on current desktop hardware, and one layer
    /// that size is already a gigabyte. The caller must still check the real
    /// `max_texture_dimension_2d` before uploading.
    pub const MAX_DIMENSION: u32 = 16384;

    /// Total layer bytes an import will accept, across the whole stack.
    pub const MAX_TOTAL_BYTES: u64 = 2 << 30;

    /// Check what [`into_stack`](Self::into_stack) relies on.
    ///
    /// Every reader guards its own bounds before decoding, but a reader that
    /// grew a flattening fallback could still hand back something the stack
    /// cannot hold; this is the one place that has to be true.
    fn validate(&self) -> Result<(), ImportError> {
        if self.layers.is_empty() {
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
        for layer in &self.layers {
            debug_assert_eq!(
                layer.pixels.len(),
                expected,
                "reader produced a layer that is not canvas-sized"
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
    /// A layer group became plain layers.
    GroupFlattened { group: String },
    /// A group's own opacity was folded into its children, which is only the
    /// same picture when they do not overlap.
    GroupOpacityFolded { group: String },
    /// A layer mask was ignored, so the layer covers more than it should.
    MaskIgnored { layer: String },
    /// A clipping layer now paints outside the layer it was clipped to.
    ClippingIgnored { layer: String },
    /// A layer could not be brought across at all.
    LayerSkipped { layer: String, reason: String },
    /// Layer structure was lost and the flattened image was used instead.
    DocumentFlattened { reason: String },
    /// Pixels were taken to be sRGB when the file said otherwise.
    ColourProfileAssumed { detail: String },
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
                    "Group “{group}” was flattened — Umber has no layer groups."
                )
            }
            Self::GroupOpacityFolded { group } => write!(
                f,
                "Group “{group}” had its own opacity, which was folded into its layers; overlapping layers inside it will look slightly different."
            ),
            Self::MaskIgnored { layer } => write!(
                f,
                "Layer “{layer}” has a mask, which was ignored — the layer covers more than it did."
            ),
            Self::ClippingIgnored { layer } => write!(
                f,
                "Layer “{layer}” was clipped to the layer below it; Umber has no clipping, so it now paints everywhere."
            ),
            Self::LayerSkipped { layer, reason } => {
                write!(f, "Layer “{layer}” could not be imported: {reason}.")
            }
            Self::DocumentFlattened { reason } => write!(
                f,
                "The layers could not be read ({reason}), so the flattened image was imported as a single layer."
            ),
            Self::ColourProfileAssumed { detail } => write!(
                f,
                "Colours were read as sRGB ({detail}); they may not match the original exactly."
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
    /// More layers than the texture array has slices.
    TooManyLayers {
        found: usize,
        max: usize,
    },
    CanvasTooLarge {
        width: u32,
        height: u32,
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
            Self::TooManyLayers { found, max } => write!(
                f,
                "The document has {found} layers; Umber supports at most {max}."
            ),
            Self::CanvasTooLarge { width, height } => write!(
                f,
                "The canvas is {width}×{height}, which is larger than Umber can open."
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

/// Reject canvases and stacks an import could never open, before decoding any
/// pixels.
///
/// Called by each reader as soon as it knows the header, which is the point of
/// it: decoding sixty 8000² layers and *then* refusing would allocate several
/// gigabytes to reach the same answer.
fn check_bounds(
    format: SourceFormat,
    width: u32,
    height: u32,
    layers: usize,
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
    if layers > LayerStack::MAX {
        return Err(ImportError::TooManyLayers {
            found: layers,
            max: LayerStack::MAX,
        });
    }
    let total = width as u64 * height as u64 * 4 * layers.max(1) as u64;
    if total > ImportedDocument::MAX_TOTAL_BYTES {
        return Err(ImportError::CanvasTooLarge { width, height });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(name: &str, size: UVec2) -> ImportedLayer {
        ImportedLayer {
            name: name.to_string(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            pixels: vec![0; size.x as usize * size.y as usize * 4],
        }
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
            warnings: vec![],
        };
        let (document, stack, uploads) = doc.into_stack();

        assert_eq!(document.size, size);
        assert_eq!(stack.len(), 3);
        assert_eq!(stack.get(0).unwrap().name, "bottom");
        assert_eq!(stack.get(2).unwrap().name, "top");
        assert_eq!(stack.active_index(), 2, "the top layer should be selected");

        // Slots are the renderer's contract: every upload must name the slot of
        // the layer at the same stack position.
        for (i, upload) in uploads.iter().enumerate() {
            assert_eq!(upload.slot, stack.get(i).unwrap().slot());
        }
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
            warnings: vec![],
        };
        let (_, stack, uploads) = doc.into_stack();
        assert_eq!(stack.len(), 1);
        assert_eq!(uploads.len(), 1);
    }

    #[test]
    fn bounds_are_checked_before_any_decoding() {
        let f = SourceFormat::Photoshop;
        assert!(matches!(
            check_bounds(f, 0, 10, 1),
            Err(ImportError::Malformed { .. })
        ));
        assert!(matches!(
            check_bounds(f, 40000, 10, 1),
            Err(ImportError::CanvasTooLarge { .. })
        ));
        assert!(matches!(
            check_bounds(f, 100, 100, LayerStack::MAX + 1),
            Err(ImportError::TooManyLayers { .. })
        ));
        // 64 layers of 8192² is 17 GB — plausible in Photoshop, hopeless here.
        assert!(matches!(
            check_bounds(f, 8192, 8192, 64),
            Err(ImportError::CanvasTooLarge { .. })
        ));
        assert!(check_bounds(f, 2048, 2048, 8).is_ok());
    }

    #[test]
    fn an_unknown_extension_is_refused_by_name() {
        let err = import(Path::new("drawing.clip")).unwrap_err();
        assert!(
            matches!(&err, ImportError::UnsupportedExtension(e) if e == "clip"),
            "got {err:?}"
        );
        // The extension is checked before the file is opened, so the message
        // does not depend on the file existing.
        assert_eq!(err.to_string(), "Umber cannot open .clip files.");
    }

    #[test]
    fn a_file_on_disk_imports_end_to_end() {
        // The one test that exercises the whole public entry point: extension
        // dispatch, reading, decoding, and building the engine state the UI
        // will be handed.
        let path = std::env::temp_dir().join("umber-docimport-end-to-end.ora");
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
        let (document, stack, uploads) = doc.into_stack();
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
            warnings: vec![],
        };
        assert!(matches!(
            doc.validate(),
            Err(ImportError::TooManyLayers { .. })
        ));
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
