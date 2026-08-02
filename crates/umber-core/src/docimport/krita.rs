//! Krita (`.kra`).
//!
//! A `.kra` is a ZIP like an ORA, but where ORA stores each layer as a PNG,
//! Krita stores its own tile format: `<document>/layers/layerN`, a text header
//! followed by 64×64 tiles, each LZF-compressed and stored **planar in BGRA
//! order**. That is the whole of the difference, and it is documented well
//! enough to implement exactly:
//!
//! - <https://community.kde.org/Krita/Tile_Data_Format>
//! - <https://github.com/2shady4u/godot-kra-psd-importer/blob/master/docs/KRA_FORMAT.md>
//!
//! Krita can also export ORA, so this reader exists for convenience rather
//! than necessity — which is exactly why it refuses to guess. A document whose
//! layers are in a colour space other than 8-bit RGBA, or whose layers are
//! vector, filter or clone layers, cannot be brought across faithfully, and in
//! that case the `mergedimage.png` every `.kra` carries is imported as a single
//! flat layer with a warning saying so.

use glam::UVec2;
use quick_xml::events::Event;

use super::blend::{self, Fidelity};
use super::container::{self, Attrs, Zip};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, SourceFormat, check_bounds, flat,
    lzf, srgb,
};
use crate::document::Background;
use crate::layer::BlendMode;

const FORMAT: SourceFormat = SourceFormat::Krita;

/// The only colour space this reader will read tiles from.
///
/// Krita's other spaces (`RGBA16`, `RGBAF32`, `GRAYA`, `CMYKA`, `LABA`) put a
/// different number of bytes, in a different order, in the same tile layout —
/// reading them as 8-bit RGBA would produce a plausible-looking image made of
/// the wrong bytes, which is the worst possible outcome.
const SUPPORTED_COLOURSPACE: &str = "RGBA";

struct LayerSpec {
    name: String,
    filename: String,
    x: i64,
    y: i64,
    opacity: f32,
    visible: bool,
    composite_op: String,
    colourspace: Option<String>,
}

pub fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let mut zip = container::open(bytes, FORMAT)?;
    container::check_mimetype(&mut zip, "application/x-krita", FORMAT)?;

    let maindoc = container::read_entry(&mut zip, "maindoc.xml", FORMAT)?;
    let mut warnings = Vec::new();
    let doc = parse_maindoc(&maindoc, &mut warnings)?;
    check_bounds(FORMAT, doc.size.x, doc.size.y, doc.layers.len())?;

    if doc.colourspace != SUPPORTED_COLOURSPACE {
        return flattened_fallback(
            &mut zip,
            doc.size,
            warnings,
            format!(
                "the document is in Krita's {} colour space",
                doc.colourspace
            ),
        );
    }
    if !doc.profile.is_empty() && !is_srgb_profile(&doc.profile) {
        warnings.push(ImportWarning::ColourProfileAssumed {
            detail: format!("the file names the profile {}", doc.profile),
        });
    }

    let mut layers = Vec::with_capacity(doc.layers.len());
    for spec in &doc.layers {
        match load_layer(&mut zip, &doc.name, spec, doc.size, &mut warnings) {
            Ok(layer) => layers.push(layer),
            Err(reason) => warnings.push(ImportWarning::LayerSkipped {
                layer: spec.name.clone(),
                reason,
            }),
        }
    }

    if layers.is_empty() {
        return flattened_fallback(
            &mut zip,
            doc.size,
            warnings,
            "no layer could be read".to_string(),
        );
    }

    Ok(ImportedDocument {
        format: FORMAT,
        size: doc.size,
        layers,
        active: None,
        background: Background::Transparent,
        dpi: doc.dpi,
        history: None,
        warnings,
    })
}

struct MainDoc {
    /// The `IMAGE` element's `name`, which is also the directory layers live in.
    name: String,
    size: UVec2,
    colourspace: String,
    profile: String,
    /// `x-res`, when the file states it. Krita writes a resolution on every
    /// document, so this is nearly always present; `None` opens at Umber's
    /// default rather than at a number nobody chose.
    dpi: Option<f32>,
    /// Bottom first.
    layers: Vec<LayerSpec>,
}

/// Parse `maindoc.xml`.
///
/// Layers are listed **uppermost first**, the same convention as ORA — Krita's
/// own example file ends with `Background` — so the list is reversed on the way
/// out to match `LayerStack`.
fn parse_maindoc(xml: &[u8], warnings: &mut Vec<ImportWarning>) -> Result<MainDoc, ImportError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let malformed = |detail: String| ImportError::Malformed {
        format: FORMAT,
        detail,
    };

    #[derive(Clone)]
    struct Group {
        opacity: f32,
        visible: bool,
    }

    let mut name = String::new();
    let mut size = None;
    let mut colourspace = String::new();
    let mut profile = String::new();
    let mut dpi = None;
    let mut specs: Vec<LayerSpec> = Vec::new();
    // One entry per open `<layer>` element: its name, and whether it pushed a
    // group. Krita hangs `<masks>` inside the layer they belong to, so the name
    // has to be to hand while the element is open.
    let mut open_layers: Vec<(bool, String)> = Vec::new();
    let mut groups: Vec<Group> = Vec::new();

    let mut buf = Vec::new();
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| malformed(format!("maindoc.xml is not valid XML ({e})")))?;
        match event {
            Event::Eof => break,
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_empty = matches!(event, Event::Empty(_));
                match e.local_name().as_ref() {
                    b"IMAGE" => {
                        let attrs = Attrs::read(e).map_err(malformed)?;
                        name = attrs.string("name").unwrap_or_default();
                        colourspace = attrs
                            .string("colorspacename")
                            .unwrap_or_else(|| SUPPORTED_COLOURSPACE.to_string());
                        profile = attrs.string("profile").unwrap_or_default();
                        size = Some(UVec2::new(
                            attrs.parse("width").unwrap_or(0),
                            attrs.parse("height").unwrap_or(0),
                        ));
                        // Krita's resolution is per axis. Umber holds one
                        // number, so a document with non-square pixels — which
                        // Krita allows and nothing here can represent — takes
                        // the horizontal one rather than an average nobody
                        // wrote down.
                        dpi = attrs.parse::<f32>("x-res").filter(|v| *v > 0.0);
                    }
                    b"layer" => {
                        let attrs = Attrs::read(e).map_err(malformed)?;
                        let layer_name = attrs.string("name").unwrap_or_else(|| "Layer".into());
                        let node_type = attrs.get("nodetype").unwrap_or("paintlayer").to_string();
                        let inherited = groups.last().cloned().unwrap_or(Group {
                            opacity: 1.0,
                            visible: true,
                        });
                        let opacity = attrs
                            .parse::<f32>("opacity")
                            .filter(|v| v.is_finite())
                            .unwrap_or(255.0)
                            .clamp(0.0, 255.0)
                            / 255.0;
                        let visible = attrs.get("visible") != Some("0");

                        let open_name = layer_name.clone();
                        let mut pushed_group = false;
                        match node_type.as_str() {
                            "grouplayer" => {
                                warnings.push(ImportWarning::GroupFlattened {
                                    group: layer_name.clone(),
                                });
                                if opacity < 1.0 {
                                    warnings.push(ImportWarning::GroupOpacityFolded {
                                        group: layer_name.clone(),
                                    });
                                }
                                groups.push(Group {
                                    opacity: inherited.opacity * opacity,
                                    visible: inherited.visible && visible,
                                });
                                pushed_group = true;
                            }
                            "paintlayer" => specs.push(LayerSpec {
                                name: layer_name,
                                filename: attrs.string("filename").unwrap_or_default(),
                                x: attrs.parse("x").unwrap_or(0),
                                y: attrs.parse("y").unwrap_or(0),
                                opacity: opacity * inherited.opacity,
                                visible: visible && inherited.visible,
                                composite_op: canonical_op(
                                    attrs.get("compositeop").unwrap_or("normal"),
                                ),
                                colourspace: attrs.string("colorspacename"),
                            }),
                            other => warnings.push(ImportWarning::LayerSkipped {
                                layer: layer_name,
                                reason: format!("Umber cannot rasterise a Krita {other}"),
                            }),
                        }
                        if !is_empty {
                            open_layers.push((pushed_group, open_name));
                        } else if pushed_group {
                            // `<layer nodetype="grouplayer"/>` with no children.
                            groups.pop();
                        }
                    }
                    b"masks" => {
                        if let Some((_, layer)) = open_layers.last() {
                            warnings.push(ImportWarning::MaskIgnored {
                                layer: layer.clone(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref e) if e.local_name().as_ref() == b"layer" => {
                // Every closing tag pops, whether or not it opened a group:
                // the two stacks have to stay in step.
                let closed_a_group = open_layers.pop().is_some_and(|(group, _)| group);
                if closed_a_group {
                    groups.pop();
                }
            }
            _ => {}
        }
        buf.clear();
    }

    let size = size.ok_or_else(|| malformed("maindoc.xml has no <IMAGE> element".into()))?;
    specs.reverse();
    Ok(MainDoc {
        name,
        size,
        colourspace,
        profile,
        dpi,
        layers: specs,
    })
}

fn load_layer(
    zip: &mut Zip<'_>,
    document: &str,
    spec: &LayerSpec,
    canvas: UVec2,
    warnings: &mut Vec<ImportWarning>,
) -> Result<ImportedLayer, String> {
    if let Some(cs) = spec
        .colourspace
        .as_deref()
        .filter(|cs| *cs != SUPPORTED_COLOURSPACE)
    {
        return Err(format!("it is in Krita's {cs} colour space"));
    }
    if spec.filename.is_empty() {
        return Err("it names no pixel data".to_string());
    }

    let path = format!("{document}/layers/{}", spec.filename);
    let data = container::read_optional_entry(zip, &path, FORMAT)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("`{path}` is not in the file"))?;

    // The colour every pixel outside the stored tiles takes. Almost always
    // transparent; a filled layer is the exception. Stored in the same BGRA
    // order as the tiles themselves.
    let default_pixel =
        container::read_optional_entry(zip, &format!("{path}.defaultpixel"), FORMAT)
            .map_err(|e| e.to_string())?
            .and_then(|b| (b.len() >= 4).then(|| [b[2], b[1], b[0], b[3]]))
            .unwrap_or([0, 0, 0, 0]);

    let mut pixels = default_pixel
        .iter()
        .copied()
        .cycle()
        .take(canvas.x as usize * canvas.y as usize * 4)
        .collect::<Vec<u8>>();
    assemble_tiles(&data, &mut pixels, canvas, (spec.x, spec.y))?;
    srgb::encode_buffer(&mut pixels);

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

    let mut layer = ImportedLayer::new(spec.name.clone(), mode, pixels);
    layer.visible = spec.visible;
    layer.opacity = spec.opacity;
    Ok(layer)
}

/// Read a layer's tile file and paint it onto a canvas-sized buffer.
///
/// The header is five text lines; the tiles that follow each carry their own
/// one-line header, `left,top,LZF,size`, where `size` counts the flag byte that
/// starts the payload as well as the payload itself.
fn assemble_tiles(
    data: &[u8],
    dst: &mut [u8],
    canvas: UVec2,
    offset: (i64, i64),
) -> Result<(), String> {
    let mut cursor = Lines::new(data);

    let _version = cursor.field("VERSION")?;
    let tile_w = cursor.field("TILEWIDTH")?;
    let tile_h = cursor.field("TILEHEIGHT")?;
    let pixel_size = cursor.field("PIXELSIZE")?;
    let tile_count = cursor.field("DATA")?;

    if pixel_size != 4 {
        return Err(format!("its pixels are {pixel_size} bytes, not 4"));
    }
    if tile_w == 0 || tile_h == 0 || tile_w > 4096 || tile_h > 4096 {
        return Err(format!("its tiles are {tile_w}×{tile_h}"));
    }
    let tile_bytes = tile_w as usize * tile_h as usize * 4;

    for _ in 0..tile_count {
        let header = cursor.line().ok_or("its tile data ends early")?;
        let header = std::str::from_utf8(header).map_err(|_| "an unreadable tile header")?;
        let mut parts = header.trim().split(',');
        let x: i64 = parse_part(&mut parts)?;
        let y: i64 = parse_part(&mut parts)?;
        let compression = parts.next().unwrap_or_default();
        let len: usize = parse_part(&mut parts)?;

        let payload = cursor.take(len).ok_or("its tile data ends early")?;
        // The first byte is a flag, not data: 1 means the rest is compressed.
        let (&flag, body) = payload.split_first().ok_or("an empty tile")?;
        let tile = if flag == 1 {
            if compression != "LZF" {
                return Err(format!("it uses {compression} compression"));
            }
            lzf::decompress(body, tile_bytes).ok_or("a corrupt compressed tile")?
        } else {
            if body.len() < tile_bytes {
                return Err("a short uncompressed tile".to_string());
            }
            body[..tile_bytes].to_vec()
        };

        blit_tile(
            dst,
            canvas,
            &tile,
            (tile_w as usize, tile_h as usize),
            (x + offset.0, y + offset.1),
        );
    }
    Ok(())
}

/// Copy one planar BGRA tile into the RGBA canvas, clipping at every edge.
///
/// Krita keeps whole tiles even where only a corner of one is inside the
/// canvas, and layers may sit at negative coordinates, so most of a real file's
/// tiles need clipping on at least one side.
fn blit_tile(
    dst: &mut [u8],
    canvas: UVec2,
    tile: &[u8],
    tile_size: (usize, usize),
    at: (i64, i64),
) {
    let (tw, th) = tile_size;
    let plane = tw * th;
    for row in 0..th {
        let dy = at.1 + row as i64;
        if dy < 0 || dy >= canvas.y as i64 {
            continue;
        }
        for col in 0..tw {
            let dx = at.0 + col as i64;
            if dx < 0 || dx >= canvas.x as i64 {
                continue;
            }
            let s = row * tw + col;
            let d = (dy as usize * canvas.x as usize + dx as usize) * 4;
            // Planar, and blue first — the one thing about this format that
            // cannot be guessed from the header.
            dst[d] = tile[2 * plane + s];
            dst[d + 1] = tile[plane + s];
            dst[d + 2] = tile[s];
            dst[d + 3] = tile[3 * plane + s];
        }
    }
}

fn flattened_fallback(
    zip: &mut Zip<'_>,
    canvas: UVec2,
    mut warnings: Vec<ImportWarning>,
    reason: String,
) -> Result<ImportedDocument, ImportError> {
    let merged = container::read_optional_entry(zip, "mergedimage.png", FORMAT)?
        .ok_or(ImportError::Empty { format: FORMAT })?;
    let image = flat::decode_png(&merged, FORMAT)?;

    let mut pixels = vec![0u8; canvas.x as usize * canvas.y as usize * 4];
    container::blit(&mut pixels, canvas, &image.rgba, image.size, (0, 0));
    srgb::encode_buffer(&mut pixels);

    warnings.push(ImportWarning::DocumentFlattened { reason });
    Ok(ImportedDocument {
        format: FORMAT,
        size: canvas,
        layers: vec![ImportedLayer::new(
            "Merged image",
            BlendMode::Normal,
            pixels,
        )],
        active: None,
        background: Background::Transparent,
        dpi: None,
        history: None,
        warnings,
    })
}

/// Krita's blend-mode ids, translated to the SVG names `blend` is written in.
fn canonical_op(op: &str) -> String {
    match op {
        "normal" => "src-over".to_string(),
        "add" | "linear_dodge" => "plus".to_string(),
        "dodge" => "color-dodge".to_string(),
        "burn" => "color-burn".to_string(),
        // Krita ships several soft-light variants that differ only in the
        // rounding of their curve.
        other if other.starts_with("soft_light") => "soft-light".to_string(),
        other => other.replace('_', "-"),
    }
}

/// Whether a Krita profile name describes an sRGB tone curve.
///
/// Krita documents can be in a linear or a Rec.709 profile, in which case the
/// bytes in the tiles do not mean what this importer assumes. The names Krita
/// ships are recognisable enough to tell the user when that has happened; doing
/// better needs an ICC engine.
fn is_srgb_profile(profile: &str) -> bool {
    let p = profile.to_ascii_lowercase();
    p.contains("srgbtrc")
        || p.contains("srgb iec")
        || p == "srgb"
        || p.contains("srgb-elle-v2-srgbtrc")
}

fn parse_part<'a, T: std::str::FromStr>(
    parts: &mut impl Iterator<Item = &'a str>,
) -> Result<T, String> {
    parts
        .next()
        .and_then(|p| p.trim().parse().ok())
        .ok_or_else(|| "a malformed tile header".to_string())
}

/// Cursor over the mixed text-and-binary tile file.
struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lines<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn line(&mut self) -> Option<&'a [u8]> {
        let rest = self.data.get(self.pos..)?;
        let end = rest.iter().position(|&b| b == b'\n')?;
        self.pos += end + 1;
        Some(&rest[..end])
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let out = self.data.get(self.pos..self.pos.checked_add(len)?)?;
        self.pos += len;
        Some(out)
    }

    /// One `KEY value` header line.
    fn field(&mut self, key: &str) -> Result<u32, String> {
        let line = self.line().ok_or("a truncated layer header")?;
        let line = std::str::from_utf8(line).map_err(|_| "an unreadable layer header")?;
        line.strip_prefix(key)
            .and_then(|v| v.trim().parse().ok())
            .ok_or_else(|| format!("a layer header without {key}"))
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{self, KraLayer};
    use super::*;

    #[test]
    fn tiles_are_assembled_bgra_planar_into_rgba() {
        // The one thing about the tile format that cannot be inferred from the
        // file: the channels are stored plane by plane, blue first. Reading it
        // as interleaved RGBA gives a picture that is recognisable but wrong,
        // which is exactly the failure this module exists to avoid.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Paint").pixel(0, 0, [200, 100, 50, 255])],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(doc.layers.len(), 1, "{:?}", doc.warnings);
        assert_eq!(&doc.layers[0].pixels[0..4], &[200, 100, 50, 255]);
    }

    #[test]
    fn an_uncompressed_tile_reads_the_same_as_a_compressed_one() {
        let compressed = fixtures::kra(
            64,
            64,
            &[KraLayer::new("A")
                .pixel(1, 2, [10, 20, 30, 255])
                .compressed()],
        );
        let plain = fixtures::kra(64, 64, &[KraLayer::new("A").pixel(1, 2, [10, 20, 30, 255])]);
        assert_eq!(
            read(&compressed).unwrap().layers[0].pixels,
            read(&plain).unwrap().layers[0].pixels
        );
    }

    #[test]
    fn layers_arrive_bottom_first() {
        // maindoc.xml lists the uppermost layer first, as ORA does.
        let kra = fixtures::kra(64, 64, &[KraLayer::new("Top"), KraLayer::new("Background")]);
        let doc = read(&kra).unwrap();
        assert_eq!(doc.layers[0].name, "Background");
        assert_eq!(doc.layers[1].name, "Top");
    }

    #[test]
    fn opacity_visibility_and_blend_mode_come_across() {
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Shade").opacity(128).hidden().op("multiply")],
        );
        let doc = read(&kra).unwrap();
        let layer = &doc.layers[0];
        assert!((layer.opacity - 128.0 / 255.0).abs() < 1e-6);
        assert!(!layer.visible);
        assert_eq!(layer.blend, BlendMode::Multiply);
    }

    #[test]
    fn a_layer_offset_moves_its_tiles() {
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Shifted")
                .pixel(0, 0, [1, 2, 3, 255])
                .at(5, 7)],
        );
        let doc = read(&kra).unwrap();
        let at = |x: usize, y: usize| &doc.layers[0].pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
        assert_eq!(at(5, 7), [1, 2, 3, 255]);
        assert_eq!(at(0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn a_sixteen_bit_document_falls_back_to_the_merged_image() {
        // Reading RGBA16 tiles as RGBA8 would produce a picture made of the
        // wrong halves of every sample. Refusing to try is the whole point.
        let kra = fixtures::kra_in_colourspace("RGBA16");
        let doc = read(&kra).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(doc.layers[0].name, "Merged image");
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::DocumentFlattened { .. }))
        );
    }

    #[test]
    fn a_group_is_flattened_and_its_state_folded_into_its_layers() {
        let doc = read(&fixtures::kra_with_group()).unwrap();

        // Three paint layers survive: the two inside the group and the one
        // outside it, still bottom first.
        let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Paper", "Fills", "Lines"]);

        // The group is hidden and half opaque; the layer outside it is neither.
        assert!(doc.layers[0].visible, "“Paper” is not in the group");
        assert!(!doc.layers[1].visible, "a hidden group hides its layers");
        assert!((doc.layers[1].opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(doc.layers[0].opacity, 1.0);

        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskIgnored { layer } if layer == "Lines"
        )));
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::GroupFlattened { group } if group == "Ink"
        )));
    }

    #[test]
    fn a_vector_layer_is_reported_rather_than_dropped_silently() {
        let kra = fixtures::kra_with_vector_layer();
        let doc = read(&kra).unwrap();
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::LayerSkipped { layer, .. } if layer == "Text"
        )));
        assert_eq!(doc.layers.len(), 1, "the paint layer should still arrive");
    }

    #[test]
    fn krita_blend_names_map_onto_the_svg_ones() {
        assert_eq!(canonical_op("normal"), "src-over");
        assert_eq!(canonical_op("add"), "plus");
        assert_eq!(canonical_op("dodge"), "color-dodge");
        assert_eq!(canonical_op("soft_light_svg"), "soft-light");
        assert_eq!(canonical_op("hard_light"), "hard-light");
        assert_eq!(
            blend::nearest(&canonical_op("multiply")).0,
            BlendMode::Multiply
        );
    }

    #[test]
    fn a_non_srgb_profile_is_flagged() {
        assert!(is_srgb_profile("sRGB-elle-V2-srgbtrc.icc"));
        assert!(!is_srgb_profile("Rec2020-elle-V4-g10.icc"));
    }
}
