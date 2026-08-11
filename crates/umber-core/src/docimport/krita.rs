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
//! # Groups
//!
//! A `<layer nodetype="grouplayer">` is a **folder**, which it did not used to
//! be: this reader flattened every group away and named the loss. Krita lists
//! its layers uppermost first, so reversing the list puts a group after its own
//! contents, which is exactly where a `LayerStack` keeps a folder — the nesting
//! comes out of the reversal for free rather than out of a second pass.
//!
//! What still folds into the children is the group's **opacity**, because a
//! folder at 50% over two overlapping children is not two children at 50% each
//! and Umber's folders carry none; that is still an [`ImportWarning`]. What no
//! longer folds is the **eye**: it lives on the folder, and
//! `LayerStack::effective_visible` walks the ancestors, so a painter who
//! reopens a folder finds its layers as they were rather than hidden by a fold
//! nothing in the file said. A group nested deeper than
//! [`LayerStack::MAX_DEPTH`] is merged into the folder outside it and *that* is
//! what raises [`ImportWarning::GroupFlattened`] now.
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
//! That byte is a **linear** multiplier and a mask slice now holds exactly one,
//! so it is copied across unchanged and widened by [`srgb::mask_buffer`]. It
//! used to be squeezed through the sRGB transfer function, which collapsed 73 of
//! its 256 states — see that module.

use glam::UVec2;
use quick_xml::events::Event;

use super::blend::{self, Fidelity};
use super::container::{self, Attrs, Zip};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, PixelPiece, SourceFormat,
    StackSize, check_bounds, flat, lzf, srgb,
};
use crate::document::Background;
use crate::geom::PixelRect;
use crate::layer::{BlendMode, LayerStack};

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
    /// How deeply nested, 0 at the top level.
    depth: u8,
    /// A `grouplayer`: it holds no pixels and takes no slot.
    folder: bool,
}

/// A `<mask nodetype="transparencymask">` as `maindoc.xml` describes it.
struct MaskSpec {
    filename: String,
    x: i64,
    y: i64,
}

pub fn read(bytes: &[u8], progress: super::Progress<'_>) -> Result<ImportedDocument, ImportError> {
    let mut zip = container::open(bytes, FORMAT)?;
    container::check_mimetype(&mut zip, "application/x-krita", FORMAT)?;

    // `maindoc.xml` is `stack.xml`'s twin and takes the same bound, which is not
    // the canvas one — see `container::MAX_STRUCTURE_BYTES`.
    let maindoc = container::read_entry_bounded(
        &mut zip,
        "maindoc.xml",
        FORMAT,
        container::MAX_STRUCTURE_BYTES,
    )?;
    let mut warnings = Vec::new();
    let doc = parse_maindoc(&maindoc, &mut warnings)?;
    let mut budget = check_bounds(
        FORMAT,
        doc.size.x,
        doc.size.y,
        StackSize::of(doc.layers.iter().map(|l| l.folder)),
    )?;

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
    let total = doc.layers.len() as u32;
    for (done, spec) in doc.layers.iter().enumerate() {
        progress(done as u32, total);
        // A folder holds no pixels and takes no slot, so there is nothing to
        // read out of the archive for one.
        if spec.folder {
            // Krita's `locked` attribute is not read for any layer, folder or
            // otherwise, so nothing is set here — `ImportedLayer::folder`
            // already leaves it unlocked and a line saying so would read as a
            // lock deliberately dropped.
            layers.push(ImportedLayer::folder(
                spec.name.clone(),
                spec.depth,
                spec.visible,
            ));
            continue;
        }
        match load_layer(&mut zip, &doc.name, spec, doc.size, &mut warnings) {
            Ok(layer) => {
                // Charged as it lands, so what bounds the accumulation is the
                // pixels the file actually held. See `PieceBudget`.
                budget.charge(&layer)?;
                layers.push(layer);
            }
            Err(reason) => warnings.push(ImportWarning::LayerSkipped {
                layer: spec.name.clone(),
                reason,
            }),
        }
    }

    // A stack of nothing but folders has nothing to show and nowhere to paint,
    // which is what `layers.is_empty()` used to mean before folders existed.
    if layers.iter().all(|l| l.folder) {
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

    /// What an open `<layer nodetype="grouplayer">` passes to its children.
    ///
    /// Opacity only. Visibility used to be here too and now lives on the
    /// folder — see the comment where a group is opened.
    #[derive(Clone)]
    struct Group {
        opacity: f32,
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

    /// One open `<layer>` element.
    struct Open {
        /// Whether this element pushed onto `groups`, so its close pops.
        pushed_group: bool,
        name: String,
        holder: Holder,
        /// It has a `<masks>` element.
        saw_masks: bool,
        /// ...and at least one `<mask>` inside it was recognised — imported,
        /// or named in a warning.
        ///
        /// The pair exists so a wrong guess about the *shape* of a mask
        /// element cannot become a **silent** loss. Everything below rests on
        /// `<masks>` holding `<mask>` children whose kind is in `nodetype`,
        /// read out of Krita's `kis_kra_savexml_visitor.cpp` — but the
        /// fixtures here are written from that same reading, so no test in
        /// this repository can say it is wrong. If it ever is, the layer still
        /// reports a mask that did not come across, which is exactly what this
        /// reader did before it could read one at all. A guess that degrades
        /// to a named loss is worth making; one that degrades to a layer
        /// quietly covering more than it should is the thing this whole module
        /// exists to refuse.
        handled_a_mask: bool,
    }

    impl Open {
        fn new(pushed_group: bool, name: String, holder: Holder) -> Self {
            Self {
                pushed_group,
                name,
                holder,
                saw_masks: false,
                handled_a_mask: false,
            }
        }
    }

    let mut name = String::new();
    let mut size = None;
    let mut colourspace = String::new();
    let mut profile = String::new();
    let mut dpi = None;
    let mut specs: Vec<LayerSpec> = Vec::new();
    // One entry per open `<layer>` element: its name, whether it pushed a
    // group, and what it became.
    let mut open_layers: Vec<Open> = Vec::new();
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
                        let inherited = groups.last().cloned().unwrap_or(Group { opacity: 1.0 });
                        let opacity = attrs
                            .parse::<f32>("opacity")
                            .filter(|v| v.is_finite())
                            .unwrap_or(255.0)
                            .clamp(0.0, 255.0)
                            / 255.0;
                        let visible = attrs.get("visible") != Some("0");

                        let open_name = layer_name.clone();
                        let mut pushed_group = false;
                        // The nesting a `<layer>` element sits at is how many
                        // groups are open around it.
                        let depth = groups.len().min(LayerStack::MAX_DEPTH as usize) as u8;
                        let holder = match node_type.as_str() {
                            "grouplayer" => {
                                // Nested deeper than Umber can hold. The depth
                                // is capped, which merges this group into the
                                // one outside it; said out loud, because the
                                // grouping is the only thing a folder *is*.
                                if groups.len() > LayerStack::MAX_DEPTH as usize {
                                    warnings.push(ImportWarning::GroupFlattened {
                                        group: layer_name.clone(),
                                    });
                                }
                                // **An opacity does not fold into a folder**,
                                // because a folder at 50% over two overlapping
                                // children is not two children at 50% each —
                                // Umber's folders carry no opacity at all, so
                                // it is folded into the children and said out
                                // loud. Visibility is the opposite: it lives on
                                // the folder, and folding it into the children
                                // as well would hide them twice, so a painter
                                // who opened the folder again would find them
                                // still hidden for a reason nothing in the file
                                // said. `LayerStack::effective_visible` walks
                                // the ancestors instead.
                                if opacity < 1.0 {
                                    warnings.push(ImportWarning::GroupOpacityFolded {
                                        group: layer_name.clone(),
                                    });
                                }
                                specs.push(LayerSpec {
                                    name: layer_name,
                                    filename: String::new(),
                                    x: 0,
                                    y: 0,
                                    opacity: 1.0,
                                    visible,
                                    composite_op: String::new(),
                                    colourspace: None,
                                    mask: None,
                                    depth,
                                    folder: true,
                                });
                                groups.push(Group {
                                    opacity: inherited.opacity * opacity,
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
                                    visible,
                                    composite_op: canonical_op(
                                        attrs.get("compositeop").unwrap_or("normal"),
                                    ),
                                    colourspace: attrs.string("colorspacename"),
                                    mask: None,
                                    depth,
                                    folder: false,
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
                            open_layers.push(Open::new(pushed_group, open_name, holder));
                        } else if pushed_group {
                            // `<layer nodetype="grouplayer"/>` with no children.
                            groups.pop();
                        }
                    }
                    // Noted, not acted on — what is inside decides everything.
                    // Recorded so that a `<masks>` yielding no `<mask>` this
                    // reader recognises is still reported; see
                    // `Open::handled_a_mask`.
                    b"masks" => {
                        if let Some(open) = open_layers.last_mut() {
                            open.saw_masks = true;
                        }
                    }
                    // A `<mask>` inside the `<masks>` of whichever layer is
                    // open. Read here rather than in a second pass because the
                    // enclosing layer is only identifiable while its element
                    // is: `<masks>` carries no name of its own.
                    b"mask" => {
                        let attrs = Attrs::read(e).map_err(malformed)?;
                        // `last_mut` is `None` for a `<mask>` sitting at the
                        // root of `<layers>` rather than inside a layer, which
                        // is how Krita serialises a **global selection
                        // mask** — a selection belonging to the image itself.
                        // Umber imports no selection out of any format, so
                        // there is nothing here for this one file to report
                        // that every other one does not.
                        if let Some(open) = open_layers.last_mut() {
                            open.handled_a_mask = true;
                            let layer = &open.name;
                            let holder = &mut open.holder;
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
                                    // A group arrives as a folder now, and a
                                    // folder holds no slot, so it can hold no
                                    // mask: every layer inside it still covers
                                    // more than it did, which is exactly what
                                    // `MaskIgnored` says. The grouping itself
                                    // is no longer lost, which is why this is
                                    // the only thing left to report here.
                                    None => ImportWarning::MaskIgnored {
                                        layer: layer.clone(),
                                    },
                                }),
                                Holder::Paint(i) => {
                                    let spec = &mut specs[*i];
                                    let what = match (unsupported, spec.mask.is_some()) {
                                        (Some(what), _) => Some(what),
                                        // Krita allows several and Umber holds
                                        // one. The first that is switched *on*
                                        // is kept — `<masks>` is written
                                        // topmost first, as the layer list is,
                                        // so that is the uppermost live one —
                                        // and the rest are named rather than
                                        // combined, which would be a second
                                        // implementation of Krita's mask stack
                                        // living in an importer.
                                        (None, true) => Some(
                                            "a second transparency mask (Umber holds one per \
                                             layer)"
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
                if let Some(open) = open_layers.pop() {
                    // It had masks and not one of them was anything this
                    // reader recognised. Reported rather than passed over —
                    // see `Open::handled_a_mask`.
                    if open.saw_masks
                        && !open.handled_a_mask
                        && !matches!(open.holder, Holder::Skipped)
                    {
                        warnings.push(ImportWarning::MaskIgnored { layer: open.name });
                    }
                    if open.pushed_group {
                        groups.pop();
                    }
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

    // **One piece per stored tile — but only where the pixels outside the
    // tiles are transparent.** A Krita layer states what it holds where it
    // stored nothing, and almost always that is transparent: then a tile the
    // file kept is a rectangle the file holds, and the canvas between them is
    // the empty value the upload's clear already leaves. A layer with a
    // *coloured* default pixel says the opposite — every pixel of the canvas
    // carries that colour — so there is nothing sparse about it and it takes
    // the dense path it always had. See `PixelPiece`'s rule 3.
    let pixels = if default_pixel == [0, 0, 0, 0] {
        let mut pieces = Vec::new();
        assemble_tiles(&data, (spec.x, spec.y), 4, |tile, size, at| {
            if let Some(piece) = tile_piece(canvas, tile, size, at) {
                pieces.push(piece);
            }
        })?;
        pieces
    } else {
        let mut dense = default_pixel
            .iter()
            .copied()
            .cycle()
            .take(canvas.x as usize * canvas.y as usize * 4)
            .collect::<Vec<u8>>();
        assemble_tiles(&data, (spec.x, spec.y), 4, |tile, size, at| {
            blit_tile(&mut dense, canvas, tile, size, at);
        })?;
        srgb::encode_buffer(&mut dense);
        vec![PixelPiece::whole(canvas, dense)]
    };

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
    layer.depth = spec.depth;
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
/// on the layer's alpha, which is exactly what a mask slice holds, so nothing
/// converts it — [`srgb::mask_buffer`] only widens it. Every one of Krita's 256
/// states arrives; 73 of them used to collide, all in the upper reveal range.
///
/// `None` where nothing could be read, and the caller raises `MaskIgnored`.
/// Two ordinary files reach it, so it is not the damaged-archive branch it
/// looks like — and in **both** the layer really does come back covering more
/// than it did, which is what that warning says:
///
/// - A mask made from a **vector** selection, which is a common enough way to
///   make one. Krita stores that as SVG under `<filename>.shapeselection/` and
///   writes no raster data beside it — its loader reads one or the other and
///   never both. Rasterising the path here would be a vector renderer inside
///   an importer.
/// - A mask whose selection is **empty**. Krita writes no `.pixelselection` at
///   all for one, and its loader defaults a pixel selection to transparent, so
///   in Krita that mask hides the layer *entirely*. This is therefore the case
///   where the warning is understated rather than wrong — and it is exactly
///   why the absent entry may not be read as "no mask": taking the default of
///   zero on faith would hide a layer completely on the strength of a file
///   that said nothing, which is the silent damage this module refuses in the
///   other direction.
fn load_mask(
    zip: &mut Zip<'_>,
    document: &str,
    spec: &MaskSpec,
    canvas: UVec2,
) -> Option<Vec<PixelPiece>> {
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
    // One canvas piece. A mask's empty value is white and the upload's clear
    // leaves transparent black, so a mask may not go sparse yet whatever its
    // default pixel is — `PixelPiece`'s rule 3.
    Some(vec![PixelPiece::whole(
        canvas,
        srgb::mask_buffer(&coverage),
    )])
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

        // **Saturating, and it is the third instance of this pattern.** Both
        // terms come out of the file — `x` and `y` off the tile's own header
        // line, `offset` off the layer element in `maindoc.xml` — so a `.kra`
        // naming `i64::MAX` for both panics a debug build here and wraps in a
        // release one. The wrapped value is then contained by `visible_rect`'s
        // saturating clamps, so the release build is a no-op and the debug build
        // is a crash on a file somebody was handed; `container::crop` was fixed
        // the same way and its comment names `blit` as the sibling left alone
        // because both its call sites pass `(0, 0)`. This one does not.
        place(
            &tile,
            (tile_w as usize, tile_h as usize),
            (x.saturating_add(offset.0), y.saturating_add(offset.1)),
        );
    }
    Ok(())
}

/// The part of a tile at `at` that lands inside the canvas, in canvas
/// coordinates, or `None` for one that misses it entirely.
///
/// Krita keeps whole tiles even where only a corner of one is inside the
/// canvas, and layers may sit at negative coordinates, so most of a real file's
/// tiles need clipping on at least one side. **This is the one statement of
/// that clipping** — [`for_each_visible`] and [`tile_piece`] both derive their
/// loops from it, because the clipping is the subtle half and three copies of
/// it would be three chances to get an edge wrong.
fn visible_rect(canvas: UVec2, tile_size: (usize, usize), at: (i64, i64)) -> Option<PixelRect> {
    let (tw, th) = tile_size;
    let x_from = at.0.saturating_neg().clamp(0, tw as i64);
    let x_to = (canvas.x as i64).saturating_sub(at.0).clamp(0, tw as i64);
    let y_from = at.1.saturating_neg().clamp(0, th as i64);
    let y_to = (canvas.y as i64).saturating_sub(at.1).clamp(0, th as i64);
    if x_to <= x_from || y_to <= y_from {
        return None;
    }
    Some(PixelRect {
        x: (at.0 + x_from) as u32,
        y: (at.1 + y_from) as u32,
        width: (x_to - x_from) as u32,
        height: (y_to - y_from) as u32,
    })
}

/// Walk the part of a tile that lands inside the canvas, handing each
/// `(index in the tile, index of the destination pixel)` pair to `f`.
fn for_each_visible(
    canvas: UVec2,
    tile_size: (usize, usize),
    at: (i64, i64),
    mut f: impl FnMut(usize, usize),
) {
    let Some(rect) = visible_rect(canvas, tile_size, at) else {
        return;
    };
    let tw = tile_size.0;
    for dy in rect.y..rect.y + rect.height {
        let row = (i64::from(dy) - at.1) as usize;
        for dx in rect.x..rect.x + rect.width {
            let col = (i64::from(dx) - at.0) as usize;
            f(
                row * tw + col,
                dy as usize * canvas.x as usize + dx as usize,
            );
        }
    }
}

/// One planar BGRA tile as a [`PixelPiece`], already sRGB-encoded.
///
/// The sparse half of what [`blit_tile`] does densely, and it is the same
/// bytes: `srgb::encode_pixel` of a fully transparent pixel is four zeroes —
/// `TABLE`'s alpha-0 row is all zeroes — so the canvas the dense path filled
/// with the default transparent pixel and then encoded is exactly the canvas
/// this leaves untouched.
fn tile_piece(
    canvas: UVec2,
    tile: &[u8],
    tile_size: (usize, usize),
    at: (i64, i64),
) -> Option<PixelPiece> {
    let rect = visible_rect(canvas, tile_size, at)?;
    let (tw, th) = tile_size;
    let plane = tw * th;
    let mut bytes = Vec::with_capacity(rect.area() as usize * 4);
    for dy in rect.y..rect.y + rect.height {
        let row = (i64::from(dy) - at.1) as usize;
        for dx in rect.x..rect.x + rect.width {
            let col = (i64::from(dx) - at.0) as usize;
            let s = row * tw + col;
            // Planar, and blue first — see `blit_tile`.
            bytes.extend_from_slice(&srgb::encode_pixel([
                tile[2 * plane + s],
                tile[plane + s],
                tile[s],
                tile[3 * plane + s],
            ]));
        }
    }
    Some(PixelPiece::new(rect, bytes))
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
    // **No reasons, deliberately.** What failed here is that the flattening
    // fallback had no `mergedimage.png` to fall back *to*, which is not "every
    // layer was refused" — listing why the stack was abandoned would name a
    // cause that is not the one the artist met.
    let merged = container::read_optional_entry(zip, "mergedimage.png", FORMAT)?.ok_or(
        ImportError::Empty {
            format: FORMAT,
            because: Vec::new(),
        },
    )?;
    let image = flat::decode_png(&merged, FORMAT)?;

    let mut pixels = vec![0u8; canvas.x as usize * canvas.y as usize * 4];
    container::blit(&mut pixels, canvas, &image.rgba, image.size, (0, 0));
    srgb::encode_buffer(&mut pixels);
    let pixels = vec![PixelPiece::whole(canvas, pixels)];

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

    /// `read` with no bar attached, which is what every test here wants.
    ///
    /// Shadows the module's own inside this scope, so the progress callback is
    /// stated once rather than at each of the several dozen call sites — none
    /// of which is about progress.
    fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
        super::read(bytes, &|_, _| {})
    }

    /// The mask byte at `(x, y)`, as the composite would read it: the red
    /// channel of the mask slice.
    fn mask_at(layer: &ImportedLayer, x: usize, y: usize, width: usize) -> u8 {
        let mask = layer.mask.as_ref().expect("the layer kept its mask");
        // A mask is one piece covering the canvas — `PixelPiece`'s rule 3 —
        // so the row stride is the canvas width and the origin is (0, 0).
        // Asserted rather than assumed: if a mask ever goes sparse this reads
        // the wrong byte and says nothing, which is the shape of bug that rule
        // exists to prevent.
        assert_eq!(mask.len(), 1, "a mask is one canvas-sized piece");
        assert_eq!(mask[0].rect.x, 0);
        assert_eq!(mask[0].rect.y, 0);
        assert_eq!(mask[0].rect.width as usize, width);
        mask[0].bytes[(y * width + x) * 4]
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
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(64, 64))[0..4],
            &[200, 100, 50, 255]
        );
    }

    /// **The reader yields the tiles the file stores and nothing else.**
    ///
    /// Krita keeps a 64-square tile only where something was painted, so a
    /// layer with one tile on a 256-square canvas is one sixteenth of the page.
    /// The picture is checked against an expectation built from the fixture
    /// rather than from the reader, the pieces are held to rules 1 and 2, and
    /// the sparsity is asserted as a fraction — a version that quietly went back
    /// to one canvas piece would pass the first two and fail the third.
    ///
    /// **The coloured-`.defaultpixel` case is not driven here, and that is a
    /// gap rather than an omission**: the fixture builder has no way to write
    /// one, so the dense fallback in `load_layer` is reached by no test. What
    /// holds it is that it is the *old* code path unchanged, guarded by a
    /// comparison against `[0, 0, 0, 0]` that a reader cannot get subtly wrong —
    /// it is either the whole canvas or the tiles.
    #[test]
    fn a_layer_yields_only_the_tiles_the_file_stores() {
        let canvas = UVec2::new(256, 256);
        let kra = fixtures::kra(
            canvas.x,
            canvas.y,
            &[KraLayer::new("Paint")
                .pixel(3, 5, [200, 100, 50, 255])
                .pixel(60, 60, [1, 2, 3, 255])],
        );
        let doc = read(&kra).unwrap();
        let layer = &doc.layers[0];

        crate::docimport::check_piece_rules(&layer.pixels, canvas);

        let mut expected = vec![0u8; (canvas.x * canvas.y * 4) as usize];
        for (x, y, rgba) in [
            (3usize, 5usize, [200, 100, 50, 255]),
            (60, 60, [1, 2, 3, 255]),
        ] {
            let px = (y * canvas.x as usize + x) * 4;
            expected[px..px + 4].copy_from_slice(&rgba);
        }
        assert_eq!(layer.dense(canvas), expected, "the picture moved");

        // One 64-square tile, not a 256-square canvas.
        assert_eq!(layer.pixel_bytes(), 64 * 64 * 4);
        assert!(
            layer.pixel_bytes() * 8 < u64::from(canvas.x) * u64::from(canvas.y) * 4,
            "one tile of sixteen must not be charged the page: {} bytes",
            layer.pixel_bytes()
        );
    }

    /// A tile's position is a number out of somebody else's file, and the
    /// arithmetic that places it must not panic on any of them.
    ///
    /// `-i64::MIN` panics in a debug build, which is why [`visible_rect`] is
    /// saturating throughout. Nothing that reads a real `.kra` can reach these
    /// values, which is exactly why they need a test rather than a reader.
    #[test]
    fn a_tile_placed_absurdly_far_off_the_page_lands_nowhere() {
        let canvas = UVec2::new(64, 64);
        for at in [
            (i64::MIN, 0),
            (0, i64::MIN),
            (i64::MAX, i64::MAX),
            (i64::MIN, i64::MAX),
            (-64, 0),
            (64, 0),
        ] {
            assert!(
                visible_rect(canvas, (64, 64), at).is_none(),
                "a 64-square tile at {at:?} does not reach a 64-square canvas"
            );
        }
        // And one that does land, so the sweep is not passing by refusing
        // everything.
        assert!(visible_rect(canvas, (64, 64), (-1, -1)).is_some());
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
        let canvas = UVec2::new(64, 64);
        assert_eq!(
            read(&compressed).unwrap().layers[0].dense(canvas),
            read(&plain).unwrap().layers[0].dense(canvas)
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
        let pixels = doc.layers[0].dense(UVec2::new(64, 64));
        let at = |x: usize, y: usize| &pixels[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
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

    /// **A Krita group arrives as a folder**, which it did not used to: this
    /// reader flattened every group away and said so. Umber has folders now,
    /// and a `<layer nodetype="grouplayer">` is exactly one.
    ///
    /// Three things are pinned and each was a decision. The folder sits
    /// **after** its own contents in the stack, which is where a `LayerStack`
    /// keeps one and is what the reversal of Krita's uppermost-first list
    /// produces for free. Its **eye stays on the folder** rather than being
    /// folded into the children, or a painter who reopened the folder would
    /// find them still hidden for a reason nothing in the file said —
    /// `LayerStack::effective_visible` walks the ancestors instead. Its
    /// **opacity is still folded**, because a folder at 50% over two
    /// overlapping children is not two children at 50% each and Umber's
    /// folders carry no opacity at all, so that one is still a named loss.
    #[test]
    fn a_group_arrives_as_a_folder_with_its_opacity_folded_into_its_layers() {
        let doc = read(&fixtures::kra_with_group()).unwrap();

        let shape: Vec<(&str, u8, bool)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), l.depth, l.folder))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Paper", 0, false),
                ("Fills", 1, false),
                ("Lines", 1, false),
                ("Ink", 0, true),
            ]
        );

        // The group is hidden and half opaque. The eye is the folder's; the
        // opacity is the children's.
        assert!(doc.layers[0].visible, "“Paper” is not in the group");
        assert!(!doc.layers[3].visible, "the folder carries the group's eye");
        assert!(
            doc.layers[1].visible,
            "a child keeps its own eye; the folder's is what hides it"
        );
        assert!((doc.layers[1].opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(doc.layers[0].opacity, 1.0);
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::GroupOpacityFolded { group } if group == "Ink"
        )));

        // The grouping is no longer a loss, so nothing may claim it was.
        assert!(
            !doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::GroupFlattened { .. })),
            "{:?}",
            doc.warnings
        );

        // "Lines" carries an ordinary transparency mask, which comes across.
        assert!(doc.layers[2].mask.is_some(), "{:?}", doc.warnings);
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
        let mask = layer.mask.as_ref().unwrap();
        assert_eq!(
            mask.len(),
            1,
            "a mask is one piece and it covers the canvas"
        );
        assert_eq!(mask[0].bytes.len(), 64 * 64 * 4);
    }

    #[test]
    fn a_masks_coverage_is_stored_the_way_the_composite_reads_it() {
        // The one thing here that cannot be seen by looking at the picture, and
        // this guard used to say the opposite: a mask slice was sampled through
        // an sRGB view, so a half had to be stored as ~188 and this asserted it.
        // It is read through the raw view now and holds the linear multiplier,
        // so Krita's byte is Umber's byte. **The direction is what matters** —
        // put the old encode back and every one of these arrives brighter than
        // the artist set it.
        //
        // Every one of Krita's 256 states is driven rather than one of them, and
        // the count is the assertion: the old encode was monotone, so a sampled
        // check passed while 73 states collided into their neighbours.
        let steps: Vec<u8> = (0..=255u8).collect();
        let mut seen = std::collections::BTreeSet::new();
        for &c in &steps {
            let kra = fixtures::kra(
                64,
                64,
                &[KraLayer::new("Lines")
                    .pixel(0, 0, [9, 9, 9, 255])
                    .mask(KraMask::transparency("Mask").coverage(0, 0, c))],
            );
            let doc = read(&kra).unwrap();
            let stored = mask_at(&doc.layers[0], 0, 0, 64);
            assert_eq!(stored, c, "coverage {c} did not arrive as itself");
            seen.insert(stored);
        }
        assert_eq!(
            seen.len(),
            256,
            "every coverage Krita can state has to reach the slice"
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

        // A default that is not one of the two fixed points, so it also pins
        // that the default goes down the same path the tiles do. 0 and 255
        // alone would pass whether it did or not — they are the fixed points of
        // the transfer function, which is exactly why they could not see the
        // encode this used to assert either.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines").pixel(0, 0, [9, 9, 9, 255]).mask(
                KraMask::transparency("Mask")
                    .coverage(0, 0, 0)
                    .default_coverage(128)
                    .at(64, 0),
            )],
        );
        let doc = read(&kra).unwrap();
        assert_eq!(
            mask_at(&doc.layers[0], 0, 0, 64),
            128,
            "the default pixel did not go down the path the tiles do"
        );
    }

    #[test]
    fn a_mask_kind_this_build_has_never_heard_of_is_named_by_its_own_word() {
        // The two arms of `mask_label` that carry no translation. A kind a
        // later Krita adds is one where the raw word is the only true thing
        // that can be said, and "a mask" would send somebody looking through
        // the wrong part of their document.
        assert_eq!(mask_label("weathermask"), "a Krita weathermask");
        assert_eq!(mask_label(""), "a Krita mask of no stated kind");

        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").kind("weathermask"))],
        );
        let doc = read(&kra).unwrap();
        assert!(doc.layers[0].mask.is_none());
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskUnsupported { what, .. } if what == "a Krita weathermask"
        )));
    }

    #[test]
    fn a_mask_that_names_no_pixel_data_is_reported_like_one_whose_data_is_gone() {
        // A `<mask>` with no `filename` at all, which is the malformed cousin
        // of the missing-entry case and must not be read as "no mask".
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").unnamed())],
        );
        let doc = read(&kra).unwrap();
        assert!(doc.layers[0].mask.is_none());
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskIgnored { layer } if layer == "Lines"
        )));
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
        assert_eq!(
            &doc.layers[0].dense(UVec2::new(64, 64))[0..4],
            &[9, 9, 9, 255]
        );
        assert!(doc.layers[0].mask.is_none());
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskIgnored { layer } if layer == "Lines"
        )));
    }

    /// A group arrives as a folder now, and a folder still cannot hold a
    /// mask: it has no slot for one, because it has no pixels of its own. So
    /// the loss is smaller than it was — the grouping survives — and it is
    /// still a loss, because every layer inside the folder now covers more
    /// than it did.
    #[test]
    fn a_mask_on_a_group_is_reported_because_a_folder_cannot_hold_one() {
        let doc = read(&fixtures::kra_with_masked_group()).unwrap();
        assert_eq!(doc.layers.len(), 2, "the folder and the layer inside it");
        assert!(doc.layers[1].folder);
        assert!(doc.layers.iter().all(|l| l.mask.is_none()));
        assert!(
            doc.warnings.iter().any(|w| matches!(
                w,
                ImportWarning::MaskIgnored { layer } if layer == "Ink"
            )),
            "{:?}",
            doc.warnings
        );
    }

    /// A `<masks>` element holding nothing this reader recognises is still
    /// reported, and that is what keeps a wrong reading of the format from
    /// being a *silent* loss.
    ///
    /// Every fixture here is written from the same reading of Krita's source
    /// the reader is, so no test in this repository can tell us that reading is
    /// wrong. What *can* be tested is the failure mode if it ever is — and the
    /// answer has to be that the layer still says it had a mask that did not
    /// come across, which is exactly what this reader did before it could read
    /// one at all. A guess that degrades to a named loss is worth making; one
    /// that degrades to a layer quietly covering more than it should is the
    /// thing this module exists to refuse.
    #[test]
    fn a_masks_element_this_reader_cannot_make_sense_of_is_still_reported() {
        // The element itself is unknown, so neither the `<mask>` arm nor the
        // `<layer>` arm fires and only `<masks>` was ever seen.
        let doc = read(&fixtures::kra_with_unreadable_mask_element()).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert!(doc.layers[0].mask.is_none());
        assert!(
            doc.warnings.iter().any(|w| matches!(
                w,
                ImportWarning::MaskIgnored { layer } if layer == "Lines"
            )),
            "an unrecognised mask must not vanish quietly: {:?}",
            doc.warnings
        );

        // And the narrower miss — the element is right, its kind attribute is
        // not — lands on the kind that says so rather than on nothing. `type=`
        // is how an older note had it; Krita writes `nodetype=`.
        let kra = fixtures::kra(
            64,
            64,
            &[KraLayer::new("Lines")
                .pixel(0, 0, [9, 9, 9, 255])
                .mask(KraMask::transparency("Mask").kind(""))],
        );
        let doc = read(&kra).unwrap();
        assert!(doc.layers[0].mask.is_none());
        assert!(doc.warnings.iter().any(|w| matches!(
            w,
            ImportWarning::MaskUnsupported { what, .. } if what == "a Krita mask of no stated kind"
        )));
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
        let pixels = crate::docimport::assemble(&upload.pieces, UVec2::new(64, 64));
        assert_eq!(pixels.len(), 64 * 64 * 4);
        assert_eq!(&pixels[0..4], &[255, 255, 255, 255]);
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

    /// `maindoc.xml` is `stack.xml`'s twin and takes the same bound at the call
    /// site — see `openraster`'s guard for why the container's own is not
    /// enough.
    #[test]
    fn a_maindoc_past_the_structure_bound_is_refused_by_the_reader() {
        let bytes = fixtures::kra_with_padded_maindoc(container::MAX_STRUCTURE_BYTES as usize + 1);
        let err = read(&bytes).expect_err("a maindoc.xml past the bound");
        assert!(
            matches!(err, ImportError::Malformed { ref detail, .. } if detail.contains("maindoc.xml")),
            "{err:?}"
        );
    }

    /// **A tile offset out of somebody else's file cannot overflow.**
    ///
    /// Both terms are read from the archive — `x`/`y` off the tile's own header
    /// line and the offset off the layer element — so `i64::MAX` in both panics
    /// a debug build and wraps in a release one. Saturating is what makes the
    /// two agree, and `visible_rect` then refuses the saturated value exactly as
    /// it refuses any other tile that misses the canvas.
    ///
    /// The extremes are the case, not decoration: nothing else in this module
    /// drives them, and `container::crop` carries the same sweep for the same
    /// reason. Demonstrated by mutation — put `x + offset.0` back and this
    /// panics with "attempt to add with overflow" on the first pair.
    ///
    /// **What is asserted is containment rather than the sum.** `i64::MAX`
    /// saturated against `i64::MIN` is `-1`, an ordinary tile hanging one pixel
    /// off the corner and genuinely visible — so "nothing reaches the canvas"
    /// would be false for that pair and true for every other, which is exactly
    /// the shape of assertion that passes for the wrong reason. What has to hold
    /// for every pair is that the position `visible_rect` is handed produces
    /// either nothing or a rectangle inside the canvas.
    #[test]
    fn a_tile_offset_out_of_a_file_cannot_overflow() {
        // Both terms extreme and in both directions, because the addition is of
        // two numbers the file states and either can be the one that overflows.
        const CANVAS: u32 = 64;
        for tile_at in [(i64::MAX, i64::MAX), (i64::MIN, i64::MIN), (0, 0)] {
            let tile = fixtures::kra_tile_file_at(4, &[0u8; 64 * 64 * 4], tile_at);
            for offset in [
                (i64::MAX, i64::MAX),
                (i64::MIN, i64::MIN),
                (i64::MAX, i64::MIN),
                (i64::MIN, i64::MAX),
            ] {
                let mut placed = Vec::new();
                assemble_tiles(&tile, offset, 4, |_, _, at| placed.push(at))
                    .expect("the tile file itself is well formed");
                assert_eq!(placed.len(), 1, "the fixture holds exactly one tile");
                let canvas = UVec2::new(CANVAS, CANVAS);
                if let Some(rect) = visible_rect(canvas, (64, 64), placed[0]) {
                    assert!(
                        rect.x + rect.width <= CANVAS && rect.y + rect.height <= CANVAS,
                        "a tile at {tile_at:?} plus {offset:?} landed outside the canvas: {rect:?}"
                    );
                }
            }
        }
    }
}
