//! Photoshop (`.psd`), through the `psd` crate.
//!
//! # Why a crate rather than our own reader
//!
//! The PSD layer record is not hard to parse — the fixture builder in
//! `fixtures.rs` writes one — but the parts around it are: PackBits per
//! scanline, additional-info blocks, Unicode names, section dividers. The `psd`
//! crate has all of that and hands back a canvas-sized RGBA buffer per layer,
//! which is precisely Umber's layer shape.
//!
//! # What it gets wrong, and how this module compensates
//!
//! The crate has three behaviours that would each silently produce a wrong
//! document. All three were established by running it over its own test
//! fixtures, which are real Photoshop files:
//!
//! - **`layers()` is ordered top first.** Its own doc comment says index 0 is
//!   the bottom layer; it is not. `transparent-top-layer-2x1.psd`, whose top
//!   layer the fixture README says is blue, yields "Blue Layer" at index 0.
//!   Used as-is, every imported document comes out inverted.
//! - **`visible()` is really "hidden".** PSD flag bit 1 is set when a layer is
//!   *not* visible; the crate returns the raw bit. Every layer of every fixture
//!   — all of them ordinary visible layers — reports `visible() == false`.
//! - **`is_clipping_mask()` is really "is a clipping base".** In
//!   `green-clipping-10x10.psd` it is true for the base layer and false for the
//!   two layers clipped to it.
//!
//! It also panics rather than erroring on some real files: ZIP-compressed
//! channel data is an `unimplemented!()`, the major-section split slices
//! without bounds checks, and `negative-top-left-layer.psd` — a file the crate
//! ships itself — panics inside `rgba()`. Parsing therefore runs inside
//! `catch_unwind`, so a bad file refuses to open instead of taking the
//! application with it.
//!
//! # What is refused
//!
//! Anything that is not 8-bit RGB. The crate reads channel bytes without
//! consulting the file's depth or colour mode, so a 16-bit or CMYK document
//! either comes back with no layers at all (the deep-colour layer data lives in
//! an `Lr16`/`Lr32` block it does not read) or with channels reinterpreted as
//! something they are not. `.psb` is refused by the crate itself: it is version
//! 2 of the header and the crate only accepts version 1.

use std::panic::AssertUnwindSafe;

use glam::UVec2;
use psd::{ColorMode, PsdChannelKind, PsdDepth};

use super::blend::{self, Fidelity};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, SourceFormat, check_bounds, srgb,
};
use crate::layer::BlendMode;

const FORMAT: SourceFormat = SourceFormat::Photoshop;

pub fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let psd = catch(
        || psd::Psd::from_bytes(bytes),
        "the file could not be parsed",
    )?
    .map_err(|e| ImportError::Malformed {
        format: FORMAT,
        detail: e.to_string(),
    })?;

    if psd.depth() != PsdDepth::Eight {
        return Err(ImportError::Unsupported {
            format: FORMAT,
            detail: format!("{:?} bits per channel", psd.depth()),
        });
    }
    if psd.color_mode() != ColorMode::Rgb {
        return Err(ImportError::Unsupported {
            format: FORMAT,
            detail: format!("{:?} colour", psd.color_mode()),
        });
    }

    let size = UVec2::new(psd.width(), psd.height());
    check_bounds(FORMAT, size.x, size.y, psd.layers().len().max(1))?;

    let mut warnings = Vec::new();

    // A file saved without "maximize compatibility", or one flattened on save,
    // has no layer records at all — only the composite in the image data
    // section. That is still a document worth opening.
    if psd.layers().is_empty() {
        let pixels = catch(|| psd.rgba(), "the flattened image could not be decoded")?;
        return Ok(finish_flat(size, pixels, warnings));
    }

    for group_id in psd.group_ids_in_order() {
        if let Some(group) = psd.groups().get(group_id) {
            warnings.push(ImportWarning::GroupFlattened {
                group: group.name().to_string(),
            });
            if group.opacity() != 255 {
                warnings.push(ImportWarning::GroupOpacityFolded {
                    group: group.name().to_string(),
                });
            }
        }
    }

    let mut layers = Vec::with_capacity(psd.layers().len());
    // Top first in the file; bottom first in a LayerStack.
    for layer in psd.layers().iter().rev() {
        let name = clean_name(layer.name());

        // A group's visibility and opacity apply to everything inside it, and
        // Umber has no groups to hang them on, so they are folded into the
        // children. Folding opacity is only exactly right when the children do
        // not overlap, which is what the warning above is for.
        let mut visible = is_visible(layer.visible());
        let mut opacity = layer.opacity() as f32 / 255.0;
        let mut parent = layer.parent_id();
        while let Some(id) = parent {
            let Some(group) = psd.groups().get(&id) else {
                break;
            };
            visible &= is_visible(group.visible());
            opacity *= group.opacity() as f32 / 255.0;
            parent = group.parent_id();
        }

        if is_clipped(layer.is_clipping_mask()) {
            warnings.push(ImportWarning::ClippingIgnored {
                layer: name.clone(),
            });
        }
        if has_mask(layer) {
            warnings.push(ImportWarning::MaskIgnored {
                layer: name.clone(),
            });
        }

        let source = blend_name(layer.blend_mode());
        let (mode, fidelity) = blend::nearest(source);
        match fidelity {
            Fidelity::Exact => {}
            Fidelity::Approximate => warnings.push(ImportWarning::BlendApproximated {
                layer: name.clone(),
                source: source.to_string(),
                used: mode.label(),
            }),
            Fidelity::Dropped => warnings.push(ImportWarning::BlendDropped {
                layer: name.clone(),
                source: source.to_string(),
            }),
        }

        // Per layer rather than once around the loop: one layer with a rect
        // outside the canvas should not cost the other thirty.
        let Ok(mut pixels) = catch(|| layer.rgba(), "") else {
            warnings.push(ImportWarning::LayerSkipped {
                layer: name,
                reason: "its pixel data could not be decoded".to_string(),
            });
            continue;
        };
        if pixels.len() != size.x as usize * size.y as usize * 4 {
            warnings.push(ImportWarning::LayerSkipped {
                layer: name,
                reason: "its pixel data is not the size of the canvas".to_string(),
            });
            continue;
        }
        srgb::encode_buffer(&mut pixels);

        layers.push(ImportedLayer {
            name,
            visible,
            opacity,
            blend: mode,
            pixels,
        });
    }

    if layers.is_empty() {
        let pixels = catch(|| psd.rgba(), "the flattened image could not be decoded")?;
        warnings.push(ImportWarning::DocumentFlattened {
            reason: "no layer could be decoded".to_string(),
        });
        return Ok(finish_flat(size, pixels, warnings));
    }

    Ok(ImportedDocument {
        format: FORMAT,
        size,
        layers,
        active: None,
        warnings,
    })
}

fn finish_flat(size: UVec2, mut pixels: Vec<u8>, warnings: Vec<ImportWarning>) -> ImportedDocument {
    srgb::encode_buffer(&mut pixels);
    ImportedDocument {
        format: FORMAT,
        size,
        layers: vec![ImportedLayer {
            name: "Background".to_string(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            pixels,
        }],
        active: None,
        warnings,
    }
}

/// Run a `psd` call, turning a panic into an error.
///
/// The crate reaches for `unimplemented!()` on ZIP-compressed channels and
/// indexes slices unchecked in several places, so a truncated or merely unusual
/// file can panic. Opening a file the user chose must not be able to end the
/// process.
fn catch<T>(f: impl FnOnce() -> T, detail: &str) -> Result<T, ImportError> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|_| ImportError::Malformed {
        format: FORMAT,
        detail: if detail.is_empty() {
            "the file could not be parsed".to_string()
        } else {
            detail.to_string()
        },
    })
}

/// Tidy a layer name for display.
///
/// Photoshop's Unicode layer names are NUL-terminated and the `psd` crate
/// keeps the terminator, so `luni.psd` — another of its own fixtures — yields
/// names like `"2 のコピー\0"`. Umber draws layer names in a font with no glyph
/// for that, so it would appear as a box after every name in the panel.
fn clean_name(raw: &str) -> String {
    raw.trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

/// `psd`'s `visible()` returns PSD flag bit 1, which is set when a layer is
/// **hidden**. See the module docs.
fn is_visible(psd_flag: bool) -> bool {
    !psd_flag
}

/// `psd`'s `is_clipping_mask()` is true for the *base* of a clipping group, so
/// a layer is clipped when it is false. See the module docs.
fn is_clipped(psd_flag: bool) -> bool {
    !psd_flag
}

/// Whether a layer carries a mask.
///
/// The crate does not expose masks, but it does keep their channels, and
/// asking for the compression of a channel that is not there is an error —
/// which is enough to tell the user their mask was dropped.
fn has_mask(layer: &psd::PsdLayer) -> bool {
    layer
        .compression(PsdChannelKind::UserSuppliedLayerMask)
        .is_ok()
        || layer
            .compression(PsdChannelKind::RealUserSuppliedLayerMask)
            .is_ok()
}

/// Photoshop's blend modes, named the way `blend` expects.
///
/// Matched on the mode's `Debug` spelling because the enum itself cannot be
/// named: `PsdLayer::blend_mode` returns a type from a private module, so the
/// crate leaks a value whose type no caller can write down. Matching on the
/// variant name is the only route to it that does not involve a fork.
///
/// It degrades in the right direction. If a future version renames a variant,
/// the name falls through to `"unknown"`, `blend::nearest` reports it as
/// dropped and the user is told — rather than the mode being quietly mapped to
/// the wrong one.
fn blend_name(mode: impl std::fmt::Debug) -> &'static str {
    match format!("{mode:?}").as_str() {
        "Normal" => "src-over",
        "Multiply" => "multiply",
        "Screen" => "screen",
        "Overlay" => "overlay",
        "Darken" => "darken",
        "Lighten" => "lighten",
        "ColorDodge" => "color-dodge",
        "ColorBurn" => "color-burn",
        "LinearDodge" => "linear-dodge",
        "LinearBurn" => "linear-burn",
        "HardLight" => "hard-light",
        "SoftLight" => "soft-light",
        "VividLight" => "vivid-light",
        "LinearLight" => "linear-light",
        "PinLight" => "pin-light",
        // `pass` is a group's mode, and a flattened group has nowhere to put
        // it; the children keep their own modes, which is the right answer.
        "PassThrough" => "src-over",
        "Dissolve" => "dissolve",
        "Difference" => "difference",
        "Exclusion" => "exclusion",
        "Subtract" => "subtract",
        "Divide" => "divide",
        "Hue" => "hue",
        "Saturation" => "saturation",
        "Color" => "color",
        "Luminosity" => "luminosity",
        "DarkerColor" => "darker-color",
        "LighterColor" => "lighter-color",
        "HardMix" => "hard-mix",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{self, PsdLayerSpec};
    use super::*;

    fn two_layers() -> Vec<u8> {
        fixtures::psd(
            2,
            1,
            // Bottom first, the order a PSD stores layer records in.
            &[
                PsdLayerSpec::new("Background", [255, 0, 0, 255]),
                PsdLayerSpec::new("Ink", [0, 0, 255, 255]).blend(*b"mul "),
            ],
        )
    }

    #[test]
    fn layers_arrive_bottom_first() {
        // The `psd` crate hands them back top first despite documenting the
        // opposite; an import that trusts the doc comment inverts the file.
        let doc = read(&two_layers()).unwrap();
        assert_eq!(doc.layers.len(), 2, "{:?}", doc.warnings);
        assert_eq!(doc.layers[0].name, "Background");
        assert_eq!(doc.layers[1].name, "Ink");
    }

    #[test]
    fn pixels_and_blend_modes_come_across() {
        let doc = read(&two_layers()).unwrap();
        assert_eq!(&doc.layers[0].pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(doc.layers[1].blend, BlendMode::Multiply);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    #[test]
    fn a_hidden_layer_is_imported_hidden() {
        // The flag in the file means "hidden"; the crate returns it as
        // `visible()`. If this ever passes without `is_visible`, the crate has
        // been fixed and the compensation must come out.
        let psd = fixtures::psd(
            1,
            1,
            &[
                PsdLayerSpec::new("Shown", [1, 2, 3, 255]),
                PsdLayerSpec::new("Hidden", [4, 5, 6, 255]).hidden(),
            ],
        );
        let doc = read(&psd).unwrap();
        assert!(doc.layers[0].visible, "“Shown” should be visible");
        assert!(!doc.layers[1].visible, "“Hidden” should not be");
    }

    #[test]
    fn opacity_is_scaled_out_of_two_hundred_and_fifty_five() {
        let psd = fixtures::psd(
            1,
            1,
            &[PsdLayerSpec::new("Faint", [0, 0, 0, 255]).opacity(128)],
        );
        let doc = read(&psd).unwrap();
        assert!((doc.layers[0].opacity - 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn an_unsupported_blend_mode_is_reported() {
        let psd = fixtures::psd(
            1,
            1,
            &[PsdLayerSpec::new("Odd", [0, 0, 0, 255]).blend(*b"diff")],
        );
        let doc = read(&psd).unwrap();
        assert_eq!(doc.layers[0].blend, BlendMode::Normal);
        assert_eq!(
            doc.warnings,
            vec![ImportWarning::BlendDropped {
                layer: "Odd".into(),
                source: "difference".into()
            }]
        );
    }

    #[test]
    fn a_clipped_layer_is_reported() {
        let psd = fixtures::psd(
            1,
            1,
            &[
                PsdLayerSpec::new("Base", [0, 0, 0, 255]),
                PsdLayerSpec::new("Clipped", [0, 0, 0, 255]).clipped(),
            ],
        );
        let doc = read(&psd).unwrap();
        assert_eq!(
            doc.warnings,
            vec![ImportWarning::ClippingIgnored {
                layer: "Clipped".into()
            }]
        );
    }

    #[test]
    fn transparency_is_premultiplied_like_every_other_import() {
        let psd = fixtures::psd(1, 1, &[PsdLayerSpec::new("Soft", [255, 255, 255, 128])]);
        let doc = read(&psd).unwrap();
        assert!((doc.layers[0].pixels[0] as i32 - 188).abs() <= 1);
    }

    #[test]
    fn unicode_layer_names_lose_their_terminator() {
        assert_eq!(clean_name("2 のコピー\u{0}"), "2 のコピー");
        assert_eq!(clean_name("Layer 1"), "Layer 1");
    }

    #[test]
    fn a_sixteen_bit_file_is_refused_rather_than_misread() {
        // The crate reads channel bytes without consulting the depth, so a
        // 16-bit file would come back as noise or as nothing at all.
        let psd = fixtures::psd_with_depth(16);
        let err = read(&psd).unwrap_err();
        assert!(
            matches!(&err, ImportError::Unsupported { detail, .. } if detail.contains("Sixteen")),
            "{err:?}"
        );
    }

    #[test]
    fn a_cmyk_file_is_refused_rather_than_misread() {
        let psd = fixtures::psd_in_cmyk();
        let err = read(&psd).unwrap_err();
        assert!(matches!(err, ImportError::Unsupported { .. }), "{err:?}");
    }

    #[test]
    fn a_psb_is_refused() {
        // PSB is header version 2. Umber does not open them, and saying so is
        // better than a confusing parse failure.
        let psb = fixtures::psb();
        let err = read(&psb).unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn a_truncated_file_errors_instead_of_panicking() {
        // The crate slices the major sections without bounds checks; this is
        // the case that would otherwise take the whole application down.
        let mut psd = two_layers();
        psd.truncate(30);
        let err = read(&psd).unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn a_file_with_no_layer_records_opens_as_its_composite() {
        let psd = fixtures::psd_flattened(2, 1, &[10, 20, 30, 40, 50, 60]);
        let doc = read(&psd).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Background");
        assert_eq!(&doc.layers[0].pixels[0..4], &[10, 20, 30, 255]);
    }
}
