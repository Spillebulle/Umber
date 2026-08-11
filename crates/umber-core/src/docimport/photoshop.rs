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
//! # Why a mask is still reported lost, when a `.kra`'s is not
//!
//! `psd` 0.3.5 does not carry a layer mask out of the file, and that is a
//! limit rather than a decision here. It was checked rather than assumed, and
//! this is what is in the way — all four, not just the first:
//!
//! - **The mask's rectangle never leaves the parser.** `read_layer_record`
//!   reads the length of the "Layer mask / adjustment layer data" block and
//!   skips the block — the comment in the crate says so. That block is where
//!   the mask's own rectangle lives, and a mask's pixels are stored in *that*
//!   rectangle rather than the layer's, so without it the bytes cannot be put
//!   anywhere. The default colour outside the rectangle, and the flags saying
//!   whether Photoshop has the mask switched off, are in the same block.
//! - **The bytes are unreachable anyway.** They are kept, in
//!   `PsdLayer::channels`, but that field is `pub(crate)` and `get_channel` is
//!   private. The one public thing about them is `compression()`, which
//!   answers how they are packed and never hands them over — which is exactly
//!   why [`has_mask`] is written the way it is: asking for the compression of
//!   a channel that is not there is an error, and that is enough to *know* a
//!   mask was dropped without being able to read it.
//! - **An RLE mask channel takes the whole file down, and this is the one that
//!   costs a user something today.** `read_layer_channels` skips the
//!   per-scanline length table with `&channel_data[2 * scanlines..]`, using the
//!   **layer's** height for every channel. A mask shorter than the layer — the
//!   ordinary case, since a mask is stored in its own rectangle — makes that a
//!   slice past the end, which panics. `catch` turns it into a refusal, so the
//!   document does not open **at all**: a real Photoshop file with a
//!   compressed mask is not a lossy import here, it is a declined one.
//!   `an_rle_mask_channel_refuses_the_file_rather_than_taking_the_process_with_
//!   it` pins the refusal, which is the part that must not regress.
//! - **There is no newer version to move to.** 0.3.5 is the latest published
//!   (January 2024).
//!
//! Reading it would therefore mean parsing the layer record here, in parallel
//! with the crate — and that is the fork the module docs above decline. Two
//! parsers walking the same bytes and disagreeing about where a section ends
//! is a worse failure than a named loss, because it produces a picture.
//!
//! So every masked layer raises [`ImportWarning::MaskIgnored`] and the layer
//! comes back covering more than it did. The rest of what a Photoshop mask
//! carries — its density, its feather, whether it is switched off, and the
//! separate vector mask — is in the same block and equally out of reach, so it
//! is stated here rather than claimed per layer: a warning naming a feather
//! this module cannot see would be an invention.
//!
//! One trap that is *not* waiting here: the mask flags byte has "disabled" at
//! bit 1, which is the same bit position as the layer flags' inverted
//! `visible` above. Nothing reads it, so nothing can read it the wrong way
//! round — and anything that starts reading it must decide that question
//! against a real file, exactly as the three inversions above were.
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
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, PixelPiece, SourceFormat,
    StackSize, check_bounds, srgb,
};
use crate::document::Background;
use crate::layer::BlendMode;

const FORMAT: SourceFormat = SourceFormat::Photoshop;

pub fn read(bytes: &[u8], progress: super::Progress<'_>) -> Result<ImportedDocument, ImportError> {
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
    // This reader makes no folders: a PSD group arrives as nothing at all, so
    // every entry it will produce holds pixels. If groups are ever read, this
    // is one of the two places that has to learn about them.
    let painted = psd.layers().len().max(1);
    let mut budget = check_bounds(FORMAT, size.x, size.y, StackSize::all_painted(painted))?;
    // **The one reader that must still be refused off its header**, and the
    // reason is three lines below at `Layer::rgba()`: this reader cannot yield
    // pieces, so a claim is a cost here where it is not in the other four.
    // Reserved before the loop rather than charged after each layer, or a
    // malformed file declaring a huge canvas is refused once the gigabytes are
    // already resident. See `PieceBudget::reserve`.
    budget.reserve(u64::from(size.x) * u64::from(size.y) * 4 * painted as u64)?;

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
    let total = psd.layers().len() as u32;
    // Top first in the file; bottom first in a LayerStack.
    for (done, layer) in psd.layers().iter().rev().enumerate() {
        progress(done as u32, total);
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

        // Carried across rather than reported lost: Umber's own clipping means
        // the same thing — bounded by the nearest unclipped layer below — so
        // there is nothing to warn about any more.
        let clipped = is_clipped(layer.is_clipping_mask());
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

        // **One canvas-sized piece, because the crate gives nothing better.**
        // A `.psd` does store per-layer rectangles, and `psd` 0.3.5's
        // `Layer::rgba()` resolves them into a canvas-sized buffer before this
        // reader ever sees one — the layer's own rectangle is behind a private
        // accessor, the same limit that keeps its masks unreadable. Cropping
        // this buffer back down would be scanning for content, which is not the
        // same claim as "the file holds this rectangle" and would cost a full
        // pass to learn something the file already knows.
        let mut imported = ImportedLayer::new(name, mode, vec![PixelPiece::whole(size, pixels)]);
        imported.visible = visible;
        imported.opacity = opacity;
        imported.clipped = clipped;
        budget.charge(&imported)?;
        layers.push(imported);
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
        background: Background::Transparent,
        dpi: None,
        history: None,
        warnings,
    })
}

fn finish_flat(size: UVec2, mut pixels: Vec<u8>, warnings: Vec<ImportWarning>) -> ImportedDocument {
    srgb::encode_buffer(&mut pixels);
    ImportedDocument {
        format: FORMAT,
        size,
        layers: vec![ImportedLayer::new(
            "Background",
            BlendMode::Normal,
            vec![PixelPiece::whole(size, pixels)],
        )],
        active: None,
        background: Background::Transparent,
        dpi: None,
        history: None,
        warnings,
    }
}

/// Run a `psd` call, turning a panic into an error.
///
/// The crate reaches for `unimplemented!()` on ZIP-compressed channels and
/// indexes slices unchecked in several places, so a truncated or merely unusual
/// file can panic. Opening a file the user chose must not be able to end the
/// process.
///
/// **`pub(super)` because [`super::preview`] is the other entry point into this
/// crate**, and it was the one that had no `catch` around it. One function
/// rather than a second `catch_unwind` over there: the sentence a refusal
/// carries and the format it names are as much part of this as the
/// `catch_unwind` is, and two copies would drift.
///
/// **It only works because panics unwind.** `panic = "abort"` is set in no
/// manifest in this workspace — see `CLAUDE.md`'s Crash reporting section, which
/// asserted the opposite for a long time — and setting it would turn every
/// refusal here into the process dying.
///
/// **What it does not stop is the crash reporter**, and that is worth saying
/// beside the sentence above rather than leaving somebody to find it. The panic
/// hook has already run by the time this catches: on the **main** thread it
/// writes a report, spawns the reporter window and latches `REPORTING` for the
/// rest of the session, so a *later* genuine crash produces no report at all.
/// That is the ordinary import path — `open_path` → [`read`] → here.
/// `umber-app::thumbnail::run` returns before `crash::install_hook` is called,
/// and `umber-shellext` installs no hook, so neither of the callers this
/// function was made `pub(super)` for is affected. Closing it means a
/// `panic::take_hook` around these calls, which is a change to the *hook's*
/// contract rather than to this one.
pub(super) fn catch<T>(f: impl FnOnce() -> T, detail: &str) -> Result<T, ImportError> {
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
/// which is enough to tell the user their mask was dropped. See the module
/// docs for why that is as far as this goes.
///
/// Both kinds are asked after: `-2` is the user-supplied layer mask and `-3`
/// is the "real" one Photoshop writes when a layer carries a vector mask as
/// well. A layer with only the second is still a masked layer, and reporting
/// one and not the other would be a loss that depends on which controls the
/// artist happened to use.
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
    use super::super::fixtures::{self, PsdLayerSpec, PsdMask};
    use super::*;

    /// `read` with no bar attached, which is what every test here wants.
    ///
    /// Shadows the module's own inside this scope, so the progress callback is
    /// stated once rather than at each of the several dozen call sites — none
    /// of which is about progress.
    fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
        super::read(bytes, &|_, _| {})
    }

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
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(2, 2))[0..4],
            &[255, 0, 0, 255]
        );
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
            &[PsdLayerSpec::new("Odd", [0, 0, 0, 255]).blend(*b"diss")],
        );
        let doc = read(&psd).unwrap();
        assert_eq!(doc.layers[0].blend, BlendMode::Normal);
        assert_eq!(
            doc.warnings,
            vec![ImportWarning::BlendDropped {
                layer: "Odd".into(),
                source: "dissolve".into()
            }]
        );
    }

    /// Clipping used to be a reported loss and is now carried across, because
    /// Umber's own flag means the same thing. A warning here would be an
    /// import claiming to have dropped something it kept.
    #[test]
    fn a_clipped_layer_arrives_clipped() {
        let psd = fixtures::psd(
            1,
            1,
            &[
                PsdLayerSpec::new("Base", [0, 0, 0, 255]),
                PsdLayerSpec::new("Clipped", [0, 0, 0, 255]).clipped(),
            ],
        );
        let doc = read(&psd).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
        // Bottom first, so the clipped one is on top.
        assert!(!doc.layers[0].clipped, "the base is not clipped");
        assert!(doc.layers[1].clipped);
    }

    #[test]
    fn transparency_is_premultiplied_like_every_other_import() {
        let psd = fixtures::psd(1, 1, &[PsdLayerSpec::new("Soft", [255, 255, 255, 128])]);
        let doc = read(&psd).unwrap();
        assert!((doc.layers[0].dense(UVec2::new(1, 1))[0] as i32 - 188).abs() <= 1);
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

    // ------------------------------------------------------------- masks

    /// A masked layer is reported as masked, and its **own** pixels are not
    /// disturbed by the extra channel sitting beside them.
    ///
    /// Until this test the mask path had never been exercised at all: every
    /// PSD fixture wrote "no mask data", so `has_mask` was a function nothing
    /// had ever called with a mask in front of it. The second half matters
    /// more than the first — a fifth channel whose length the reader got wrong
    /// would desynchronise every layer after it, and the symptom would be
    /// wrong pixels rather than a missing warning.
    #[test]
    fn a_masked_layer_is_reported_and_keeps_its_own_pixels() {
        let psd = fixtures::psd(
            4,
            4,
            &[
                PsdLayerSpec::new("Paper", [255, 0, 0, 255]),
                // Deliberately a rectangle unlike the layer's, which is the
                // ordinary case and the one that makes the reason a mask
                // cannot be read out of `psd` 0.3.5 concrete: the block naming
                // this rectangle is skipped by the crate.
                PsdLayerSpec::new("Ink", [0, 0, 255, 255]).mask(PsdMask::new((1, 1, 3, 3), 128)),
            ],
        );
        let doc = read(&psd).unwrap();

        assert_eq!(doc.layers.len(), 2, "{:?}", doc.warnings);
        assert_eq!(doc.layers[0].name, "Paper");
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(4, 4))[0..4],
            &[255, 0, 0, 255]
        );
        assert_eq!(
            &doc.layers[1].dense(UVec2::new(4, 4))[0..4],
            &[0, 0, 255, 255],
            "the mask channel must not be read as the layer's own"
        );

        assert!(doc.layers[1].mask.is_none(), "see the module docs");
        assert_eq!(
            doc.warnings,
            vec![ImportWarning::MaskIgnored {
                layer: "Ink".into()
            }],
            "a dropped mask must be named exactly once"
        );
    }

    /// A mask Photoshop has switched off is reported the same as a live one,
    /// and that is honest rather than lazy.
    ///
    /// The flag saying it is off lives in the block `psd` 0.3.5 skips, so this
    /// module genuinely cannot tell the two apart. Reporting only the ones it
    /// could see would be a warning list that depends on what a crate happens
    /// to parse; reporting both is a true statement — a mask was there and did
    /// not come across. If a future version exposes the flag, the disabled
    /// case can join Krita's under `MaskUnsupported`, where the picture is
    /// right and only the mask is lost.
    #[test]
    fn a_disabled_mask_is_reported_because_this_module_cannot_see_that_it_is_off() {
        let psd = fixtures::psd(
            2,
            2,
            &[PsdLayerSpec::new("Ink", [0, 0, 0, 255])
                .mask(PsdMask::new((0, 0, 2, 2), 255).disabled())],
        );
        let doc = read(&psd).unwrap();
        assert_eq!(
            doc.warnings,
            vec![ImportWarning::MaskIgnored {
                layer: "Ink".into()
            }]
        );
    }

    /// `psd` 0.3.5 hands over no route to a mask's bytes, and this pins that
    /// the reader is not quietly relying on one.
    ///
    /// `compression()` is the whole of the public surface: it says how the
    /// channel is packed and never yields it. The day that changes, this test
    /// is where to start — and the module docs list the other three things
    /// that would have to change with it.
    #[test]
    fn the_crate_reports_a_mask_channel_and_still_hands_over_none_of_it() {
        let bytes = fixtures::psd(
            2,
            2,
            &[PsdLayerSpec::new("Ink", [0, 0, 0, 255]).mask(PsdMask::new((0, 0, 2, 2), 200))],
        );
        let psd = psd::Psd::from_bytes(&bytes).unwrap();
        let layer = &psd.layers()[0];

        assert!(
            layer
                .compression(PsdChannelKind::UserSuppliedLayerMask)
                .is_ok(),
            "the fixture does not carry a mask channel at all"
        );
        assert!(has_mask(layer));
        // And the layer this crate *can* build is the layer without it.
        assert_eq!(&layer.rgba()[0..4], &[0, 0, 0, 255]);
    }

    /// An RLE-compressed mask channel — what Photoshop ordinarily writes —
    /// refuses the file rather than taking the process with it.
    ///
    /// `psd` 0.3.5 skips the per-scanline length table using the *layer's*
    /// height for every channel, so a mask stored in a shorter rectangle makes
    /// that a slice past the end of the channel. `catch` is the whole of why
    /// that is a message instead of a crash, and this is the case that proves
    /// the module docs' claim rather than restating it: a real Photoshop file
    /// with a compressed mask is a *declined* import here, not a lossy one.
    #[test]
    fn an_rle_mask_channel_refuses_the_file_rather_than_taking_the_process_with_it() {
        let psd = fixtures::psd(
            4,
            8,
            &[PsdLayerSpec::new("Ink", [0, 0, 0, 255])
                .mask(PsdMask::new((0, 0, 2, 4), 200).compressed())],
        );
        let err = read(&psd).unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn a_file_with_no_layer_records_opens_as_its_composite() {
        let psd = fixtures::psd_flattened(2, 1, &[10, 20, 30, 40, 50, 60]);
        let doc = read(&psd).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Background");
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(2, 1))[0..4],
            &[10, 20, 30, 255]
        );
    }
}
