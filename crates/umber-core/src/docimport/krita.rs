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
//!
//! # Masks
//!
//! A layer's masks hang off it in `maindoc.xml` as `<mask>` children of a
//! `<masks>` element, each naming its kind in `nodetype` and its own binary
//! data in `filename`. Krita has five kinds and **exactly one of them is
//! Umber's**: a `transparencymask` bounds the layer's alpha, which is what a
//! mask does here. A filter mask, a transform mask, a selection mask and a
//! colorize mask are all something else entirely, so each is named in an
//! [`ImportWarning`] rather than approximated — a filter mask read as a
//! transparency mask would hide the layer wherever the filter was strongest,
//! which is a picture nobody drew.
//!
//! A transparency mask's pixels are **not** where a layer's are. Krita builds
//! the mask out of a selection and writes that selection's paint device to
//! `<document>/layers/<filename>.pixelselection`, with the byte outside the
//! stored tiles in `<filename>.pixelselection.defaultpixel` — the same pair of
//! entries a layer has, one directory along and under a different name. The
//! tile file is the same format with `PIXELSIZE 1`, because a pixel selection
//! lives in Krita's ALPHA colour space: one byte, `0` hiding and `255`
//! revealing. `assemble_tiles` is therefore written once and told how wide a
//! pixel is, rather than copied.
//!
//! That byte is a **linear** multiplier and a mask slice does not hold one; see
//! [`srgb::encode_coverage`], which is the only place the two meet.

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

/// The one Krita mask kind that means what Umber's does.
const TRANSPARENCY_MASK: &str = "transparencymask";

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
    /// The one transparency mask this layer gets to keep, if it had one that
    /// is switched on. Everything else its `<masks>` element held has already
    /// been reported by the time this is filled in.
    mask: Option<MaskSpec>,
}

/// A `<mask nodetype="transparencymask">` as `maindoc.xml` describes it.
struct MaskSpec {
    filename: String,
    x: i64,
    y: i64,
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

    /// What the `<layer>` element currently open turned into. Krita hangs
    /// `<masks>` inside the layer they belong to, so a mask has to be able to
    /// find it — and what it does with the mask depends on which of these the
    /// layer was.
    enum Holder {
        /// A paint layer, which became `specs[i]` and can take one mask.
        Paint(usize),
        /// A group layer. Groups are flattened away here, so a mask on one
        /// shaped every layer inside it and is a real loss worth naming.
        Group,
        /// A layer this reader could not bring across at all. Its own
        /// `LayerSkipped` warning already says the whole thing has gone, so
        /// its masks stay quiet rather than adding a line each.
        Skipped,
    }

    let mut name = String::new();
    let mut size = None;
    let mut colourspace = String::new();
    let mut profile = String::new();
    let mut dpi = None;
    let mut specs: Vec<LayerSpec> = Vec::new();
    // One entry per open `<layer>` element: its name, whether it pushed a
    // group, and what it became.
    let mut open_layers: Vec<(bool, String, Holder)> = Vec::new();
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
                        let holder = match node_type.as_str() {
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
                                Holder::Group
                            }
                            "paintlayer" => {
                                specs.push(LayerSpec {
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
                                    mask: None,
                                });
                                Holder::Paint(specs.len() - 1)
                            }
                            other => {
                                warnings.push(ImportWarning::LayerSkipped {
                                    layer: layer_name,
                                    reason: format!("Umber cannot rasterise a Krita {other}"),
                                });
                                Holder::Skipped
                            }
                        };
                        if !is_empty {
                            open_layers.push((pushed_group, open_name, holder));
                        } else if pushed_group {
                            // `<layer nodetype="grouplayer"/>` with no children.
                            groups.pop();
                        }
                    }
                    // A `<mask>` inside the `<masks>` of whichever layer is
                    // open. Read here rather than in a second pass because the
                    // enclosing layer is only identifiable while its element
                    // is: `<masks>` carries no name of its own.
                    b"mask" => {
                        let attrs = Attrs::read(e).map_err(malformed)?;
                        // `last_mut` is `None` for a `<mask>` outside every
                        // layer, which is malformed and has nothing to attach
                        // to.
                        if let Some((_, layer, holder)) = open_layers.last_mut() {
                            let kind = attrs.get("nodetype").unwrap_or_default();
                            let unsupported = if kind != TRANSPARENCY_MASK {
                                Some(mask_label(kind))
                            } else if attrs.get("visible") == Some("0") {
                                // Krita has switched this one off, so it
                                // bounds nothing there either: the picture is
                                // right without it and only the mask itself is
                                // lost. `MaskIgnored` would claim the layer
                                // covers more than it did, which would be
                                // false.
                                Some("a transparency mask that was switched off".to_string())
                            } else {
                                None
                            };

                            match holder {
                                // A skipped layer's masks are not worth a line
                                // each: the layer has gone and its own warning
                                // already says so.
                                Holder::Skipped => {}
                                Holder::Group => warnings.push(match unsupported {
                                    Some(what) => ImportWarning::MaskUnsupported {
                                        layer: layer.clone(),
                                        what,
                                    },
                                    // Groups are flattened away here, so a
                                    // transparency mask on one has nowhere to
                                    // go and every layer inside it now covers
                                    // more than it did — which is exactly what
                                    // `MaskIgnored` says.
                                    None => ImportWarning::MaskIgnored {
                                        layer: layer.clone(),
                                    },
                                }),
                                Holder::Paint(i) => {
                                    let spec = &mut specs[*i];
                                    let what = match (unsupported, spec.mask.is_some()) {
                                        (Some(what), _) => Some(what),
                                        // Krita allows several and Umber holds
                                        // one. The uppermost is kept —
                                        // `<masks>` is written topmost first,
                                        // as the layer list is — and the rest
                                        // are named rather than combined,
                                        // which would be a second
                                        // implementation of Krita's mask
                                        // stack living in an importer.
                                        (None, true) => Some(
                                            "a second transparency mask, where Umber holds one \
                                             per layer"
                                                .to_string(),
                                        ),
                                        (None, false) => {
                                            spec.mask = Some(MaskSpec {
                                                filename: attrs
                                                    .string("filename")
                                                    .unwrap_or_default(),
                                                x: attrs.parse("x").unwrap_or(0),
                                                y: attrs.parse("y").unwrap_or(0),
                                            });
                                            None
                                        }
                                    };
                                    if let Some(what) = what {
                                        warnings.push(ImportWarning::MaskUnsupported {
                                            layer: layer.clone(),
                                            what,
                                        });
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Event::End(ref e) if e.local_name().as_ref() == b"layer" => {
                // Every closing tag pops, whether or not it opened a group:
                // the two stacks have to stay in step.
                let closed_a_group = open_layers.pop().is_some_and(|(group, _, _)| group);
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
    assemble_tiles(&data, (spec.x, spec.y), 4, |tile, size, at| {
        blit_tile(&mut pixels, canvas, tile, size, at);
    })?;
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
    // A mask that is named and then cannot be read is a *warning*, not a
    // skipped layer, for exactly the reason ORA's `load_mask` gives: the
    // layer's own pixels are all there, and one that comes back showing more
    // than it should is a far smaller loss than one that does not come back.
    if let Some(mask) = &spec.mask {
        match load_mask(zip, document, mask, canvas) {
            Some(pixels) => layer.mask = Some(pixels),
            None => warnings.push(ImportWarning::MaskIgnored {
                layer: spec.name.clone(),
            }),
        }
    }
    Ok(layer)
}

/// A transparency mask's coverage, canvas-sized and in mask-slice form.
///
/// The pixels are in the mask's *selection* — `<filename>.pixelselection`,
/// beside the layer files — and are one byte each, because a Krita pixel
/// selection lives in the ALPHA colour space. That byte is a linear multiplier
/// on the layer's alpha, which is not what a mask slice holds; see
/// [`srgb::encode_coverage`].
///
/// `None` where nothing could be read, and the caller says so — a default of
/// zero would mean a layer hidden completely, and inventing that from an absent
/// entry is exactly the silent damage this module refuses. Two ordinary files
/// reach it, so it is not the damaged-archive branch it looks like:
///
/// - A mask whose selection is **empty**. Krita writes no `.pixelselection` for
///   one, and its own loader then leaves the selection at its default.
/// - A mask made from a **vector** selection, which is a common enough way to
///   make one. Krita stores that as SVG under `<filename>.shapeselection/` and
///   writes no raster data beside it — its loader reads one or the other and
///   never both. Rasterising the path here would be a vector renderer inside an
///   importer, so the layer arrives unmasked and says so.
fn load_mask(zip: &mut Zip<'_>, document: &str, spec: &MaskSpec, canvas: UVec2) -> Option<Vec<u8>> {
    if spec.filename.is_empty() {
        return None;
    }
    let path = format!("{document}/layers/{}.pixelselection", spec.filename);
    let data = container::read_optional_entry(zip, &path, FORMAT)
        .ok()
        .flatten()?;

    // What the selection holds outside the tiles it stored. Zero — nothing
    // selected, so nothing revealed — is Krita's own default for a pixel
    // selection, and is what its loader falls back to when the entry is
    // missing.
    let default = container::read_optional_entry(zip, &format!("{path}.defaultpixel"), FORMAT)
        .ok()
        .flatten()
        .and_then(|b| b.first().copied())
        .unwrap_or(0);

    let mut coverage = vec![default; canvas.x as usize * canvas.y as usize];
    assemble_tiles(&data, (spec.x, spec.y), 1, |tile, size, at| {
        blit_coverage_tile(&mut coverage, canvas, tile, size, at);
    })
    .ok()?;
    Some(srgb::encode_coverage_buffer(&coverage))
}

/// Krita's mask kinds, named the way its own interface names them.
///
/// Krita's spelling, not this codebase's, for the two that differ: these name
/// a feature of somebody else's application, and an artist reading the warning
/// is going to go and look for the thing Krita calls a Colorize Mask.
fn mask_label(node_type: &str) -> String {
    match node_type {
        "filtermask" => "a Krita filter mask".to_string(),
        "transformmask" => "a Krita transform mask".to_string(),
        "selectionmask" => "a Krita selection mask".to_string(),
        "colorizemask" => "a Krita colorize mask".to_string(),
        "" => "a Krita mask of no stated kind".to_string(),
        // Named rather than called "a mask": a kind this build has never heard
        // of is one a later Krita added, and the raw word is the only true
        // thing that can be said about it.
        other => format!("a Krita {other}"),
    }
}

/// Read a tile file and hand each decoded tile to `place`.
///
/// The header is five text lines; the tiles that follow each carry their own
/// one-line header, `left,top,LZF,size`, where `size` counts the flag byte that
/// starts the payload as well as the payload itself.
///
/// `want_pixel_size` is how wide a pixel the caller can read: 4 for a layer's
/// BGRA, 1 for a transparency mask's selection. The file states its own and is
/// refused where the two disagree, because everything about the layout below —
/// the tile's length, the stride between its planes — is that number, and
/// reading a 1-byte tile as 4-byte would produce a plausible picture made of
/// the wrong bytes. One function told how wide a pixel is rather than two
/// copies of the parser, because the header, the LZF flag and the clipping are
/// the same in both cases and the last of those is the subtle one.
fn assemble_tiles(
    data: &[u8],
    offset: (i64, i64),
    want_pixel_size: u32,
    mut place: impl FnMut(&[u8], (usize, usize), (i64, i64)),
) -> Result<(), String> {
    let mut cursor = Lines::new(data);

    let _version = cursor.field("VERSION")?;
    let tile_w = cursor.field("TILEWIDTH")?;
    let tile_h = cursor.field("TILEHEIGHT")?;
    let pixel_size = cursor.field("PIXELSIZE")?;
    let tile_count = cursor.field("DATA")?;

    if pixel_size != want_pixel_size {
        return Err(format!(
            "its pixels are {pixel_size} bytes, not {want_pixel_size}"
        ));
    }
    if tile_w == 0 || tile_h == 0 || tile_w > 4096 || tile_h > 4096 {
        return Err(format!("its tiles are {tile_w}×{tile_h}"));
    }
    let tile_bytes = tile_w as usize * tile_h as usize * pixel_size as usize;

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

        place(
            &tile,
            (tile_w as usize, tile_h as usize),
            (x + offset.0, y + offset.1),
        );
    }
    Ok(())
}

/// Walk the part of a tile that lands inside the canvas, handing each
/// `(index in the tile, index of the destination pixel)` pair to `f`.
///
/// Krita keeps whole tiles even where only a corner of one is inside the
/// canvas, and layers may sit at negative coordinates, so most of a real file's
/// tiles need clipping on at least one side. Written once and shared by both
/// blits below: the clipping is the subtle half and two copies of it is two
/// chances to get an edge wrong.
fn for_each_visible(
    canvas: UVec2,
    tile_size: (usize, usize),
    at: (i64, i64),
    mut f: impl FnMut(usize, usize),
) {
    let (tw, th) = tile_size;
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
            f(
                row * tw + col,
                dy as usize * canvas.x as usize + dx as usize,
            );
        }
    }
}

/// Copy one planar BGRA tile into the RGBA canvas.
fn blit_tile(
    dst: &mut [u8],
    canvas: UVec2,
    tile: &[u8],
    tile_size: (usize, usize),
    at: (i64, i64),
) {
    let plane = tile_size.0 * tile_size.1;
    for_each_visible(canvas, tile_size, at, |s, p| {
        let d = p * 4;
        // Planar, and blue first — the one thing about this format that
        // cannot be guessed from the header.
        dst[d] = tile[2 * plane + s];
        dst[d + 1] = tile[plane + s];
        dst[d + 2] = tile[s];
        dst[d + 3] = tile[3 * plane + s];
    });
}

/// Copy one single-byte selection tile into a canvas of coverage.
///
/// One byte a pixel, so there are no planes to interleave and nothing about the
/// channel order to get wrong — which is the whole reason the mask path shares
/// the tile reader above rather than the blit.
fn blit_coverage_tile(
    dst: &mut [u8],
    canvas: UVec2,
    tile: &[u8],
    tile_size: (usize, usize),
    at: (i64, i64),
) {
    for_each_visible(canvas, tile_size, at, |s, p| dst[p] = tile[s]);
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
    use super::super::fixtures::{self, KraLayer, KraMask};
    use super::*;

    /// The mask byte at `(x, y)`, as the composite would read it: the red
    /// channel of the mask slice.
    fn mask_at(layer: &ImportedLayer, x: usize, y: usize, width: usize) -> u8 {
        layer.mask.as_ref().expect("the layer kept its mask")[(y * width + x) * 4]
    }

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

        // "Lines" carries an ordinary transparency mask, which comes across
        // rather than being reported — the group being flattened is a
        // different loss and is still reported.
        assert!(doc.layers[2].mask.is_some(), "{:?}", doc.warnings);
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

    // ------------------------------------------------------------- masks

    #[test]
    fn a_transparency_mask_arrives_as_a_mask() {
        // The mask's own pixel data lives under `.pixelselection`, not beside
        // the layer's tiles, and it is one byte a pixel rather than four.
        // Reading it out of the layer's own file — or as BGRA — is the failure
        // this pins.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines").pixel(0, 0, [9, 9, 9, 255]).mask(
                KraMask::transparency("Mask")
                    .coverage(0, 0, 255)
                    .coverage(1, 0, 0),
            )],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(doc.layers.len(), 1, "{:?}", doc.warnings);
        assert!(
            doc.warnings.is_empty(),
            "an imported mask is not a loss: {:?}",
            doc.warnings
        );

        let layer = &doc.layers[0];
        assert_eq!(mask_at(layer, 0, 0, 64), 255, "the mask reveals here");
        assert_eq!(mask_at(layer, 1, 0, 64), 0, "and hides here");
        // Canvas-sized and four bytes a pixel, exactly as the layer is —
        // `ImportedDocument::validate` debug-asserts it, and this says so
        // where the failure is legible.
        assert_eq!(layer.mask.as_ref().unwrap().len(), 64 * 64 * 4);
    }

    #[test]
    fn a_masks_coverage_is_stored_the_way_the_composite_reads_it() {
        // The one thing here that cannot be seen by looking at the picture.
        // Krita's byte is a linear multiplier on the layer's alpha; a mask
        // slice is sampled through an sRGB view, so a half has to be stored as
        // ~188. Copying 128 across unchanged would hide four fifths of a layer
        // the artist hid by half — a wrong picture that looks deliberate.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").coverage(0, 0, 128))],
        );
        let doc = read(&kra).unwrap();
        let stored = mask_at(&doc.layers[0], 0, 0, 64);
        assert!(
            (stored as i32 - 188).abs() <= 1,
            "half coverage was stored as {stored}, not ~188"
        );
    }

    #[test]
    fn everything_outside_a_masks_own_tiles_takes_its_default_pixel() {
        // Krita stores only the tiles a selection touched; the rest is the
        // `.defaultpixel`, which for a pixel selection is "not selected". A
        // reader that left the remainder at whatever it allocated would show
        // the layer wherever the mask happened not to have a tile.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").coverage(0, 0, 255).at(64, 0))],
        );
        let doc = read(&kra).unwrap();
        // The mask's one tile sits a whole tile to the right of the canvas, so
        // nothing it holds is visible and every pixel is the default.
        let layer = &doc.layers[0];
        assert_eq!(mask_at(layer, 0, 0, 64), 0);
        assert_eq!(mask_at(layer, 63, 63, 64), 0);

        // And the default is read rather than assumed: an inverted selection
        // has a default of 255, which reveals everywhere it stored nothing.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines").pixel(0, 0, [9, 9, 9, 255]).mask(
                KraMask::transparency("Mask")
                    .coverage(0, 0, 0)
                    .default_coverage(255)
                    .at(64, 0),
            )],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(mask_at(&doc.layers[0], 0, 0, 64), 255);
    }

    #[test]
    fn a_mask_offset_moves_its_tiles_the_way_a_layers_do() {
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").coverage(0, 0, 255).at(5, 7))],
        );
        let doc = read(&kra).unwrap();
        let layer = &doc.layers[0];
        assert_eq!(mask_at(layer, 5, 7, 64), 255);
        assert_eq!(mask_at(layer, 0, 0, 64), 0);
    }

    #[test]
    fn krita_masks_umber_has_no_equivalent_for_are_named_rather_than_guessed_at() {
        // A filter mask read as a transparency mask would hide the layer
        // wherever the filter was strongest, which is a picture nobody drew.
        // Each kind is named because "a mask was dropped" sends the artist
        // looking for the wrong thing.
        for (kind, expected) in [
            ("filtermask", "a Krita filter mask"),
            ("transformmask", "a Krita transform mask"),
            ("selectionmask", "a Krita selection mask"),
            ("colorizemask", "a Krita colorize mask"),
        ] {
            let kra = fixtures::kra(
                64,
                64,
                &[KraLayer::new("Lines")
                    .pixel(0, 0, [9, 9, 9, 255])
                    .mask(KraMask::transparency("Mask").kind(kind))],
            );
            let doc = read(&kra).unwrap();
            assert!(doc.layers[0].mask.is_none(), "{kind} became a mask");
            assert!(
                doc.warnings.iter().any(|w| matches!(
                    w,
                    ImportWarning::MaskUnsupported { layer, what }
                        if layer == "Lines" && what == expected
                )),
                "{kind}: {:?}",
                doc.warnings
            );
        }
    }

    #[test]
    fn a_mask_krita_had_switched_off_changes_no_pixel_and_still_says_so() {
        // A hidden mask bounds nothing in Krita either, so the picture is
        // right without it — which is why this is not `MaskIgnored`, whose
        // whole sentence is that the layer now covers more than it did.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").coverage(0, 0, 255).hidden())],
        );
        let doc = read(&kra).unwrap();
        assert!(doc.layers[0].mask.is_none());
        assert!(
            !doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::MaskIgnored { .. })),
            "nothing about the picture changed: {:?}",
            doc.warnings
        );
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskUnsupported { what, .. } if what.contains("switched off")
        )));
    }

    #[test]
    fn only_the_first_of_several_transparency_masks_is_kept_and_the_rest_are_named() {
        // Krita stacks masks and Umber holds one. Compositing them here would
        // be a second implementation of Krita's mask stack living in an
        // importer — so the uppermost is kept and the rest are reported.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Upper").coverage(0, 0, 255))
                .mask(KraMask::transparency("Lower").coverage(0, 0, 64))],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(
            mask_at(&doc.layers[0], 0, 0, 64),
            255,
            "the uppermost mask is the one kept"
        );
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskUnsupported { what, .. } if what.contains("second transparency mask")
        )));
    }

    #[test]
    fn a_mask_whose_pixels_are_missing_is_reported_and_the_layer_still_arrives() {
        // The layer's own pixels are all there; a layer that comes back
        // showing more than it should is a far smaller loss than one that does
        // not come back at all. Inventing a fully-hiding mask out of an absent
        // entry would be the silent damage this module refuses.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").without_data())],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert_eq!(&doc.layers[0].pixels[0..4], &[9, 9, 9, 255]);
        assert!(doc.layers[0].mask.is_none());
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskIgnored { layer } if layer == "Lines"
        )));
    }

    #[test]
    fn a_mask_on_a_group_is_reported_because_the_group_is_flattened_away() {
        let doc = read(&fixtures::kra_with_masked_group()).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert!(doc.layers[0].mask.is_none());
        assert!(
            doc.warnings.iter().any(|w| matches!(
                w,
                ImportWarning::MaskIgnored { layer } if layer == "Ink"
            )),
            "{:?}",
            doc.warnings
        );
    }

    #[test]
    fn a_mask_on_a_layer_that_was_skipped_adds_no_second_line() {
        // The layer has gone and its own warning already says so; a mask
        // warning beside it would be a second sentence about the same loss, in
        // the one list that has to stay worth reading.
        let kra = fixtures::kra(
            64,
            64,
            &[
                KraLayer::new("Text")
                    .vector()
                    .mask(KraMask::transparency("Mask")),
                KraLayer::new("Paint").pixel(0, 0, [1, 2, 3, 255]),
            ],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(
            doc.warnings.len(),
            1,
            "only the skipped layer should be reported: {:?}",
            doc.warnings
        );
        assert!(matches!(
            doc.warnings[0],
            ImportWarning::LayerSkipped { .. }
        ));
    }

    #[test]
    fn an_imported_mask_reaches_the_stack_as_a_slice_of_its_own() {
        // The other end of the same story: a mask is another slice of the same
        // layer array, so it is another `LayerUpload` and nothing between here
        // and `write_texture` has to know it is a mask.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").coverage(0, 0, 255))],
        );
        let opened = read(&kra).unwrap().open();
        let mask_slot = opened.stack.mask_at(0).expect("the layer kept its mask");
        let upload = opened
            .uploads
            .iter()
            .find(|u| u.slot == mask_slot)
            .expect("the mask's slice was never given any pixels");
        assert_eq!(upload.pixels.len(), 64 * 64 * 4);
        assert_eq!(&upload.pixels[0..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn a_mask_file_that_is_not_one_byte_a_pixel_is_refused_rather_than_misread() {
        // The tile reader is shared by the layer path and the mask path and is
        // told how wide a pixel it may read. A four-byte tile read as coverage
        // would be a plausible-looking mask made of the wrong bytes, which is
        // the failure the colour-space refusal at the top of this file exists
        // to prevent.
        let mut buf = vec![0u8; 64 * 64];
        buf[0] = 255;
        assert!(
            assemble_tiles(
                &fixtures::kra_tile_file(4, &vec![0u8; 64 * 64 * 4]),
                (0, 0),
                1,
                |_, _, _| unreachable!("a tile was placed from a file of the wrong pixel size"),
            )
            .is_err()
        );
        // And the right width still reads.
        let mut coverage = vec![0u8; 64 * 64];
        assemble_tiles(
            &fixtures::kra_tile_file(1, &buf),
            (0, 0),
            1,
            |tile, size, at| blit_coverage_tile(&mut coverage, UVec2::new(64, 64), tile, size, at),
        )
        .unwrap();
        assert_eq!(coverage[0], 255);
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
