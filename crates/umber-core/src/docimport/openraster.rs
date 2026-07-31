//! OpenRaster (`.ora`).
//!
//! The one format in this module that is a published, open specification —
//! <https://www.openraster.org/baseline/file-layout-spec.html> and
//! <https://www.openraster.org/baseline/layer-stack-spec.html> — and
//! consequently the one that arrives exactly. A `.ora` is a ZIP holding a
//! `stack.xml` and one PNG per layer, which is very nearly Umber's own model:
//! straight-alpha sRGB pixels, a name, an opacity, a visibility and a
//! blend mode named with SVG's vocabulary.
//!
//! Two things do not survive: nested stacks (Umber has no groups) and the
//! fifteen blend modes it does not implement. Both are reported.
//!
//! Krita, GIMP, MyPaint, Drawpile and Pinta all write `.ora`, which makes it
//! the recommended way into Umber from anything this module declines.

use glam::UVec2;
use quick_xml::events::Event;

use super::blend::{self, Fidelity};
use super::container::{self, Attrs, Zip};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, SourceFormat, check_bounds, flat,
    srgb,
};

const FORMAT: SourceFormat = SourceFormat::OpenRaster;

/// A `<layer>` as `stack.xml` describes it, with its groups' effects already
/// folded in. Collected before any PNG is decoded so the layer count can be
/// checked while it is still cheap.
struct LayerSpec {
    name: String,
    src: String,
    x: i64,
    y: i64,
    opacity: f32,
    visible: bool,
    composite_op: String,
}

pub fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let mut zip = container::open(bytes, FORMAT)?;
    container::check_mimetype(&mut zip, "image/openraster", FORMAT)?;

    let stack_xml = container::read_entry(&mut zip, "stack.xml", FORMAT)?;
    let mut warnings = Vec::new();
    let (size, specs) = parse_stack(&stack_xml, &mut warnings)?;
    check_bounds(FORMAT, size.x, size.y, specs.len())?;

    let mut layers = Vec::with_capacity(specs.len());
    for spec in specs {
        match load_layer(&mut zip, &spec, size, &mut warnings) {
            Ok(layer) => layers.push(layer),
            Err(reason) => warnings.push(ImportWarning::LayerSkipped {
                layer: spec.name.clone(),
                reason,
            }),
        }
    }

    if layers.is_empty() {
        // A file whose layer PNGs are unreadable can still be opened through
        // the composite the spec requires every writer to include. Losing the
        // layer structure is a real loss, but less of one than refusing.
        return flattened_fallback(&mut zip, size, warnings);
    }

    Ok(ImportedDocument {
        format: FORMAT,
        size,
        layers,
        warnings,
    })
}

/// Read `stack.xml`.
///
/// The stack is walked depth first. **The first element in a stack is the
/// uppermost**, per the specification, so the collected order is top to bottom
/// and gets reversed at the end — Umber's `LayerStack` is bottom first. Getting
/// this backwards inverts the whole image and is not obvious on a symmetrical
/// test file, so `layers_arrive_bottom_first` pins it down.
fn parse_stack(
    xml: &[u8],
    warnings: &mut Vec<ImportWarning>,
) -> Result<(UVec2, Vec<LayerSpec>), ImportError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let malformed = |detail: String| ImportError::Malformed {
        format: FORMAT,
        detail,
    };

    // One frame per open `<stack>`: a group's opacity and visibility apply to
    // everything inside it.
    #[derive(Clone)]
    struct Group {
        opacity: f32,
        visible: bool,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut size: Option<UVec2> = None;
    let mut specs: Vec<LayerSpec> = Vec::new();
    let mut depth = 0usize;

    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| malformed(format!("stack.xml is not valid XML ({e})")))?;

        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let empty = matches!(event, Event::Empty(_));
                let attrs = Attrs::read(e).map_err(malformed)?;
                match e.local_name().as_ref() {
                    b"image" => {
                        let w = attrs.parse("w").unwrap_or(0);
                        let h = attrs.parse("h").unwrap_or(0);
                        size = Some(UVec2::new(w, h));
                    }
                    b"stack" => {
                        depth += 1;
                        // The root stack carries no attributes and is not a
                        // group; only nested ones are.
                        if depth > 1 {
                            let name = attrs.string("name").unwrap_or_else(|| "Group".into());
                            let opacity = opacity(&attrs);
                            let visible = visible(&attrs);
                            let op = composite_op(&attrs);

                            warnings.push(ImportWarning::GroupFlattened {
                                group: name.clone(),
                            });
                            if opacity < 1.0 {
                                warnings.push(ImportWarning::GroupOpacityFolded {
                                    group: name.clone(),
                                });
                            }
                            if blend::nearest(&op).1 != Fidelity::Exact {
                                warnings.push(ImportWarning::BlendDropped {
                                    layer: name,
                                    source: op,
                                });
                            }

                            let inherited = groups.last().cloned().unwrap_or(Group {
                                opacity: 1.0,
                                visible: true,
                            });
                            groups.push(Group {
                                opacity: inherited.opacity * opacity,
                                visible: inherited.visible && visible,
                            });
                        }
                        // `<stack/>` — an empty group — never gets an End.
                        if empty {
                            depth -= 1;
                            if depth >= 1 {
                                groups.pop();
                            }
                        }
                    }
                    b"layer" => {
                        let Some(src) = attrs.string("src") else {
                            // Without a src there is nothing to load; the file
                            // is malformed but the rest of it may be fine.
                            warnings.push(ImportWarning::LayerSkipped {
                                layer: attrs.string("name").unwrap_or_else(|| "?".into()),
                                reason: "it names no image file".into(),
                            });
                            continue;
                        };
                        let group = groups.last().cloned().unwrap_or(Group {
                            opacity: 1.0,
                            visible: true,
                        });
                        specs.push(LayerSpec {
                            name: attrs.string("name").unwrap_or_else(|| name_from_src(&src)),
                            src,
                            x: attrs.parse("x").unwrap_or(0),
                            y: attrs.parse("y").unwrap_or(0),
                            opacity: opacity(&attrs) * group.opacity,
                            visible: visible(&attrs) && group.visible,
                            composite_op: composite_op(&attrs),
                        });
                    }
                    _ => {}
                }
            }
            Event::End(ref e) if e.local_name().as_ref() == b"stack" => {
                depth = depth.saturating_sub(1);
                if depth >= 1 {
                    groups.pop();
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let size = size.ok_or_else(|| malformed("stack.xml has no <image> element".into()))?;
    // Top first in the file, bottom first in the stack.
    specs.reverse();
    Ok((size, specs))
}

fn load_layer(
    zip: &mut Zip<'_>,
    spec: &LayerSpec,
    canvas: UVec2,
    warnings: &mut Vec<ImportWarning>,
) -> Result<ImportedLayer, String> {
    let png = container::read_optional_entry(zip, &spec.src, FORMAT)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("`{}` is not in the file", spec.src))?;
    let image = flat::decode_png(&png, FORMAT).map_err(|e| e.to_string())?;

    let (mode, fidelity) = blend::nearest(&spec.composite_op);
    match fidelity {
        Fidelity::Exact => {}
        Fidelity::Approximate => warnings.push(ImportWarning::BlendApproximated {
            layer: spec.name.clone(),
            source: spec.composite_op.clone(),
            used: mode.label(),
        }),
        Fidelity::Dropped => warnings.push(ImportWarning::BlendDropped {
            layer: spec.name.clone(),
            source: spec.composite_op.clone(),
        }),
    }

    let mut pixels = vec![0u8; canvas.x as usize * canvas.y as usize * 4];
    container::blit(
        &mut pixels,
        canvas,
        &image.rgba,
        image.size,
        (spec.x, spec.y),
    );
    srgb::encode_buffer(&mut pixels);

    Ok(ImportedLayer {
        name: spec.name.clone(),
        visible: spec.visible,
        opacity: spec.opacity,
        blend: mode,
        pixels,
    })
}

/// Last resort: the composite every ORA is required to carry.
fn flattened_fallback(
    zip: &mut Zip<'_>,
    canvas: UVec2,
    mut warnings: Vec<ImportWarning>,
) -> Result<ImportedDocument, ImportError> {
    let merged = container::read_optional_entry(zip, "mergedimage.png", FORMAT)?
        .ok_or(ImportError::Empty { format: FORMAT })?;
    let image = flat::decode_png(&merged, FORMAT)?;

    let mut pixels = vec![0u8; canvas.x as usize * canvas.y as usize * 4];
    container::blit(&mut pixels, canvas, &image.rgba, image.size, (0, 0));
    srgb::encode_buffer(&mut pixels);

    warnings.push(ImportWarning::DocumentFlattened {
        reason: "no layer image could be read".into(),
    });
    Ok(ImportedDocument {
        format: FORMAT,
        size: canvas,
        layers: vec![ImportedLayer {
            name: "Merged image".to_string(),
            visible: true,
            opacity: 1.0,
            blend: crate::layer::BlendMode::Normal,
            pixels,
        }],
        warnings,
    })
}

/// A layer name for a `<layer>` that has none.
///
/// `name` is optional and MyPaint leaves it out, so the alternative is a
/// layers panel reading "data/layer000.png" all the way down.
fn name_from_src(src: &str) -> String {
    let file = src.rsplit(['/', '\\']).next().unwrap_or(src);
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    if stem.is_empty() {
        "Layer".to_string()
    } else {
        stem.to_string()
    }
}

/// `opacity` is a 0..1 float, defaulting to fully opaque.
fn opacity(attrs: &Attrs) -> f32 {
    attrs
        .parse::<f32>("opacity")
        .filter(|v| v.is_finite())
        .unwrap_or(1.0)
        .clamp(0.0, 1.0)
}

/// `visibility` is the words `visible` or `hidden`, not a boolean.
fn visible(attrs: &Attrs) -> bool {
    !matches!(attrs.get("visibility"), Some("hidden"))
}

/// `composite-op`, with the `svg:` namespace prefix dropped so it matches the
/// canonical names in `blend`.
fn composite_op(attrs: &Attrs) -> String {
    let raw = attrs.get("composite-op").unwrap_or("svg:src-over").trim();
    raw.strip_prefix("svg:").unwrap_or(raw).to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{self, OraLayer};
    use super::*;
    use crate::layer::BlendMode;

    fn two_layer_ora() -> Vec<u8> {
        fixtures::ora(
            4,
            4,
            // Top first, the way the specification orders them.
            &[
                OraLayer::new("Top", 1, 1, &[0, 0, 255, 255]).op("svg:multiply"),
                OraLayer::new("Bottom", 4, 4, &[255, 0, 0, 255]),
            ],
        )
    }

    #[test]
    fn layers_arrive_bottom_first() {
        // stack.xml lists the top layer first; LayerStack is bottom first.
        // Import that skips the reversal produces an upside-down document.
        let doc = read(&two_layer_ora()).unwrap();
        assert_eq!(doc.size, UVec2::new(4, 4));
        assert_eq!(doc.layers.len(), 2);
        assert_eq!(doc.layers[0].name, "Bottom");
        assert_eq!(doc.layers[1].name, "Top");
    }

    #[test]
    fn layer_properties_come_across() {
        let doc = read(&two_layer_ora()).unwrap();
        assert_eq!(doc.layers[1].blend, BlendMode::Multiply);
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
    }

    #[test]
    fn a_small_layer_lands_at_its_offset() {
        // The 1×1 blue top layer sits at x=1,y=1 in the fixture.
        let doc = read(&two_layer_ora()).unwrap();
        let top = &doc.layers[1];
        let at = |x: usize, y: usize| &top.pixels[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(1, 1), [0, 0, 255, 255]);
        assert_eq!(at(0, 0), [0, 0, 0, 0], "outside the layer must be empty");
    }

    #[test]
    fn opacity_and_visibility_are_read_as_written() {
        let ora = fixtures::ora(
            1,
            1,
            &[OraLayer::new("Faint", 1, 1, &[0, 0, 0, 255])
                .opacity(0.25)
                .hidden()],
        );
        let doc = read(&ora).unwrap();
        assert_eq!(doc.layers[0].opacity, 0.25);
        assert!(!doc.layers[0].visible);
    }

    #[test]
    fn an_unsupported_blend_mode_is_reported_not_hidden() {
        let ora = fixtures::ora(
            1,
            1,
            &[OraLayer::new("Odd", 1, 1, &[0, 0, 0, 255]).op("svg:difference")],
        );
        let doc = read(&ora).unwrap();
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
    fn a_group_is_flattened_and_its_state_folded_into_its_layers() {
        let doc = read(&fixtures::ora_with_group()).unwrap();
        // Two layers inside a hidden, half-opaque group.
        assert_eq!(doc.layers.len(), 2);
        assert!(
            doc.layers.iter().all(|l| !l.visible),
            "a hidden group must hide what is inside it"
        );
        assert_eq!(doc.layers[0].opacity, 0.5);
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::GroupFlattened { .. }))
        );
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::GroupOpacityFolded { .. }))
        );
    }

    #[test]
    fn a_missing_layer_png_falls_back_to_the_merged_image() {
        let ora = fixtures::ora_missing_layer_data();
        let doc = read(&ora).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Merged image");
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::DocumentFlattened { .. }))
        );
    }

    #[test]
    fn an_unnamed_layer_is_named_after_its_file() {
        // MyPaint writes `<layer src="data/layer000.png"/>` with no name.
        assert_eq!(name_from_src("data/layer000.png"), "layer000");
        assert_eq!(name_from_src(""), "Layer");
    }

    #[test]
    fn a_zip_that_is_not_an_ora_is_refused() {
        let err = read(&fixtures::wrong_mimetype_zip()).unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }

    #[test]
    fn a_file_that_is_not_a_zip_is_refused() {
        let err = read(b"PK not really").unwrap_err();
        assert!(matches!(err, ImportError::Malformed { .. }), "{err:?}");
    }
}
