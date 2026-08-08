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
//!
//! # Umber's own documents come through here
//!
//! [`crate::docformat`] writes ORA, so this is also the reader for Umber's own
//! saved files. Its extra attributes carry what baseline ORA has nowhere to
//! put — `umber-version`, `umber-selected`, `umber-blend`, `umber-background`,
//! `umber-history`, `umber-mask` and `umber-effects`, all documented there —
//! and they are read here rather than in a second reader, because two readers
//! for one format is two things to keep in step. A file written by anything
//! else simply has none of them.
//!
//! `umber-background` is the one that changes what this reader *does* rather
//! than what it concludes: the layer carrying it is the document background,
//! written as a real layer so that every other application shows the right
//! picture. Here it is turned back into a document property, and its PNG is
//! never decoded — the attribute already holds the colour, so skipping it saves
//! a canvas-sized decode on every open.
//!
//! # Somebody else's masks are not in here, and that is the file's doing
//!
//! `umber-mask` is Umber's own, so a mask arrives out of an ORA only where
//! Umber wrote it. It is worth saying what the other writers do, because the
//! obvious worry — that a mask arrives disguised as a layer and quietly erases
//! the one below it — is not one this reader can walk into:
//!
//! - **Krita bakes them in.** Its ORA export writes each layer's
//!   *projection* — the layer with its masks already applied — and emits no
//!   mask element of any kind. So a masked Krita layer arrives here looking
//!   exactly as it looked in Krita, and the loss is that the mask is no longer
//!   separable, which changes no pixel. Krita does write `svg:dst-in`, but
//!   only where a *layer* carries Destination In as its blend mode, which is
//!   an ordinary blend mode there and not a mask convention.
//! - **A `dst-in` layer is therefore an ordinary unsupported blend mode**, and
//!   goes through `blend::nearest` like every other: Normal, and a
//!   [`ImportWarning::BlendDropped`] naming it. That is the right answer and
//!   the important half is that it is *named* — a layer meant to erase the one
//!   below arriving as flat paint over it is a large change to the picture,
//!   and one nobody was told about would be the silent loss this module
//!   exists to refuse. `a_layer_that_would_erase_the_one_below_is_named`
//!   pins it.
//! - **GIMP and MyPaint have nothing to write.** Neither carries a layer mask
//!   into an ORA either.

use glam::UVec2;
use quick_xml::events::Event;

use super::blend::{self, Fidelity};
use super::container::{self, Attrs, Zip};
use super::{
    ImportError, ImportWarning, ImportedDocument, ImportedLayer, SourceFormat, check_bounds,
    disable_effects_over_budget, flat, history, srgb,
};
use crate::color::Color;
use crate::docformat;
use crate::document::Background;
use crate::effect::Effect;
use crate::layer::{BlendMode, LayerStack};

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
    /// `umber-blend`, when Umber wrote this file and the SVG name it had to use
    /// is not an exact match for the mode it meant.
    umber_blend: Option<BlendMode>,
    /// `umber-selected`: the layer that was being painted on when it was saved.
    selected: bool,
    /// `umber-background`: this "layer" is really the document background, and
    /// this is its colour. The PNG beside it is for other applications.
    background: Option<Color>,
    /// `umber-mask`: the archive entry holding this layer's mask, outside the
    /// ORA layer stack. See [`docformat::MASK_ATTR`].
    mask_src: Option<String>,
    /// `umber-effects`: the archive entry holding this layer's effects, outside
    /// the ORA layer stack. See [`docformat::EFFECTS_ATTR`].
    ///
    /// Read off a `<layer>` and deliberately **not** off a nested `<stack>`: a
    /// folder can hold no effect — `LayerStack::plan_set_effect` refuses one,
    /// because there is no coverage to derive it from until group compositing
    /// lands — and the writer skips a folder for exactly that reason, so
    /// reading one here would be reading something nothing writes. When a
    /// folder can carry an effect, both halves move together.
    effects_src: Option<String>,
    /// `umber-clip`, `umber-lock`, and the link group.
    clipped: bool,
    locked: bool,
    /// `umber-link-group` where the file has it, and `umber-link` alone read as
    /// group zero where it does not: a file written before groups existed said
    /// "one set", and group zero is that set. See
    /// [`docformat::LINK_GROUP_ATTR`].
    link: Option<u8>,
    /// How deeply nested inside `<stack>` elements, 0 at the top level.
    depth: u8,
    /// This spec is a nested `<stack>`, and becomes a folder rather than a
    /// layer. It names no `src` and decodes no PNG.
    folder: bool,
}

pub fn read(bytes: &[u8]) -> Result<ImportedDocument, ImportError> {
    let mut zip = container::open(bytes, FORMAT)?;
    container::check_mimetype(&mut zip, "image/openraster", FORMAT)?;

    let stack_xml = container::read_entry(&mut zip, "stack.xml", FORMAT)?;
    let mut warnings = Vec::new();
    let (size, dpi, manifest, mut specs) = parse_stack(&stack_xml, &mut warnings)?;

    // The background is a layer in the file and a property here, so it comes
    // out of the list before anything counts or decodes it. Removed rather than
    // read past: it must not also arrive as a layer, and the count
    // `check_bounds` sees has to be the number of layers the stack will really
    // hold — a document with the full 64 plus a background is one Umber wrote
    // and must be able to reopen.
    let mut background = Background::Transparent;
    if let Some(i) = specs.iter().position(|spec| spec.background.is_some()) {
        let spec = specs.remove(i);
        background = spec
            .background
            .map_or(Background::Transparent, Background::opaque);
    }
    check_bounds(FORMAT, size.x, size.y, specs.len())?;

    let mut layers = Vec::with_capacity(specs.len());
    // Tracked against the layers that actually loaded, not against the specs:
    // a skipped layer shifts every position after it.
    let mut active = None;
    for spec in specs {
        match load_layer(&mut zip, &spec, size, &mut warnings) {
            Ok(layer) => {
                if spec.selected {
                    active = Some(layers.len());
                }
                layers.push(layer);
            }
            Err(reason) => warnings.push(ImportWarning::LayerSkipped {
                layer: spec.name.clone(),
                reason,
            }),
        }
    }

    if !layers.iter().any(|l| !l.folder) {
        // A file whose layer PNGs are unreadable can still be opened through
        // the composite the spec requires every writer to include. Losing the
        // layer structure is a real loss, but less of one than refusing.
        //
        // Folders do not count towards "there is something here": one holds no
        // pixels, so a document of nothing but empty groups has nothing to show
        // and nowhere to paint.
        return flattened_fallback(&mut zip, size, dpi, warnings);
    }

    // A document can name more enabled effects than Umber can draw, and it has
    // to open anyway. Settled here rather than at the install, where the
    // refusal would have nowhere to be reported — see
    // `disable_effects_over_budget`.
    disable_effects_over_budget(&mut layers, &mut warnings);

    // The undo history, when the document says it has one. Read last, and
    // against the layers that actually *loaded* rather than against the specs:
    // a layer skipped above shifts every stack position after it, and the
    // positions are the whole of how a patch finds its layer again.
    let history = manifest.and_then(|path| {
        let names: Vec<String> = layers.iter().map(|l| l.name.clone()).collect();
        history::read(&mut zip, &path, size, &names, &mut warnings)
    });

    Ok(ImportedDocument {
        format: FORMAT,
        size,
        layers,
        active,
        background,
        dpi,
        history,
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
///
/// The third value is `umber-history` — the entry naming the saved undo
/// history's manifest, when there is one.
#[allow(clippy::type_complexity)]
fn parse_stack(
    xml: &[u8],
    warnings: &mut Vec<ImportWarning>,
) -> Result<(UVec2, Option<f32>, Option<String>, Vec<LayerSpec>), ImportError> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    reader.config_mut().trim_text(true);

    let malformed = |detail: String| ImportError::Malformed {
        format: FORMAT,
        detail,
    };

    // One frame per open `<stack>`.
    //
    // Only the opacity is carried now: a group *is* a folder here, so its
    // visibility and its name stay on the folder where they belong, and only
    // the one thing a pass-through folder cannot hold still gets folded into
    // the layers inside — with a warning, since folding a group's opacity into
    // its children is not the same picture wherever those children overlap.
    #[derive(Clone)]
    struct Group {
        opacity: f32,
    }
    let mut groups: Vec<Group> = Vec::new();
    let mut size: Option<UVec2> = None;
    let mut dpi: Option<f32> = None;
    let mut manifest: Option<String> = None;
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
                        // OpenRaster's own resolution attributes, in pixels per
                        // inch. Umber holds one number, so a file whose `xres`
                        // and `yres` differ — which the format allows and
                        // nothing here can represent — is read by its
                        // horizontal, rather than by an average nobody wrote.
                        dpi = attrs.parse::<f32>("xres").filter(|v| *v > 0.0);
                        // The saved undo history is found through this
                        // attribute and not by looking for the entry: every
                        // writer of an ORA rewrites `stack.xml`, so an
                        // application that copied Umber's private entries
                        // across while rearranging the stack cannot leave a
                        // history pointing at layers that have moved.
                        manifest = attrs.string(docformat::HISTORY_ATTR);

                        // Checked before a single pixel is decoded, so a file
                        // from a future Umber costs nothing to refuse.
                        if let Some(version) = attrs.parse::<u32>(docformat::VERSION_ATTR)
                            && version > docformat::VERSION
                        {
                            return Err(ImportError::NewerVersion {
                                version,
                                supported: docformat::VERSION,
                            });
                        }
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

                            // **The group itself is kept**, as a folder, which
                            // is why there is no `GroupFlattened` here any
                            // more. What is still folded away is a group's
                            // *opacity* and *blend mode*: a folder in this
                            // build is pass-through, so it can carry the name
                            // and the eye and nothing else, and the two that do
                            // not survive are reported exactly as they were
                            // before. See `docs/layer-folders.md` — the
                            // difference is group compositing, and it is not
                            // built.
                            if opacity < 1.0 {
                                warnings.push(ImportWarning::GroupOpacityFolded {
                                    group: name.clone(),
                                });
                            }
                            if blend::nearest(&op).1 != Fidelity::Exact {
                                warnings.push(ImportWarning::BlendDropped {
                                    layer: name.clone(),
                                    source: op,
                                });
                            }
                            // Nested deeper than Umber can hold. The depths are
                            // capped below, which merges this group into the one
                            // outside it; said out loud, because the grouping is
                            // the only thing a folder *is* and losing it
                            // silently is exactly the quiet loss this module
                            // exists to refuse.
                            if depth - 2 > LayerStack::MAX_DEPTH as usize {
                                warnings.push(ImportWarning::GroupFlattened {
                                    group: name.clone(),
                                });
                            }

                            let inherited =
                                groups.last().cloned().unwrap_or(Group { opacity: 1.0 });
                            // Visibility is deliberately *not* inherited here.
                            // It lives on the folder now, and folding an outer
                            // group's eye into an inner one as well would hide
                            // the same layers twice — a painter who opened the
                            // outer folder again would find the inner one still
                            // shut for a reason nothing in the file said.
                            // `LayerStack::effective_visible` walks the
                            // ancestors instead, which is one rule rather than a
                            // second copy baked into the import.
                            groups.push(Group {
                                opacity: inherited.opacity * opacity,
                            });
                            specs.push(LayerSpec {
                                name,
                                src: String::new(),
                                x: 0,
                                y: 0,
                                opacity: 1.0,
                                visible,
                                composite_op: "src-over".into(),
                                umber_blend: None,
                                selected: attrs.get(docformat::SELECTED_ATTR) == Some("true"),
                                background: None,
                                mask_src: None,
                                effects_src: None,
                                clipped: false,
                                locked: attrs.get(docformat::LOCK_ATTR) == Some("true"),
                                // A folder can belong to a link group — read
                                // exactly as a layer's is, or a set that
                                // contained one comes back a member short and
                                // the survivor draws a chain meaning "moves
                                // together with nothing".
                                link: attrs
                                    .get(docformat::LINK_GROUP_ATTR)
                                    .and_then(|g| g.parse::<u8>().ok())
                                    .filter(|g| (*g as usize) < LayerStack::LINK_GROUPS)
                                    .or_else(|| {
                                        (attrs.get(docformat::LINK_ATTR) == Some("true"))
                                            .then_some(0)
                                    }),
                                depth: depth.saturating_sub(2).min(u8::MAX as usize) as u8,
                                folder: true,
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
                        let group = groups.last().cloned().unwrap_or(Group { opacity: 1.0 });
                        specs.push(LayerSpec {
                            name: attrs.string("name").unwrap_or_else(|| name_from_src(&src)),
                            src,
                            x: attrs.parse("x").unwrap_or(0),
                            y: attrs.parse("y").unwrap_or(0),
                            opacity: opacity(&attrs) * group.opacity,
                            visible: visible(&attrs),
                            composite_op: composite_op(&attrs),
                            umber_blend: attrs
                                .get(docformat::BLEND_ATTR)
                                .and_then(docformat::blend_from_id),
                            // A layer inside a group can carry it too, and the
                            // group is flattened away, so it still points at
                            // the right layer.
                            selected: attrs.get(docformat::SELECTED_ATTR) == Some("true"),
                            // An unreadable value yields `None`, which leaves
                            // this as an ordinary layer: the picture is still
                            // right, where a colour guessed out of a malformed
                            // attribute would not be.
                            background: attrs
                                .get(docformat::BACKGROUND_ATTR)
                                .and_then(docformat::background_from_id),
                            mask_src: attrs.string(docformat::MASK_ATTR),
                            effects_src: attrs.string(docformat::EFFECTS_ATTR),
                            clipped: attrs.get(docformat::CLIP_ATTR) == Some("true"),
                            locked: attrs.get(docformat::LOCK_ATTR) == Some("true"),
                            link: attrs
                                .get(docformat::LINK_GROUP_ATTR)
                                .and_then(|g| g.parse::<u8>().ok())
                                .filter(|g| (*g as usize) < LayerStack::LINK_GROUPS)
                                .or_else(|| {
                                    (attrs.get(docformat::LINK_ATTR) == Some("true")).then_some(0)
                                }),
                            // One less than the XML nesting: the root `<stack>`
                            // is the document, not a group.
                            depth: depth.saturating_sub(1).min(u8::MAX as usize) as u8,
                            folder: false,
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
    Ok((size, dpi, manifest, specs))
}

fn load_layer(
    zip: &mut Zip<'_>,
    spec: &LayerSpec,
    canvas: UVec2,
    warnings: &mut Vec<ImportWarning>,
) -> Result<ImportedLayer, String> {
    // A folder has no `src` and nothing to decode. It still becomes an entry,
    // because the *nesting* is what a folder is, and a group whose contents
    // loaded but whose own row did not would leave every layer in it at a depth
    // enclosed by nothing.
    if spec.folder {
        let mut folder = ImportedLayer::folder(spec.name.clone(), spec.depth, spec.visible);
        folder.locked = spec.locked;
        folder.link = spec.link;
        return Ok(folder);
    }
    let png = container::read_optional_entry(zip, &spec.src, FORMAT)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("`{}` is not in the file", spec.src))?;
    let image = flat::decode_png(&png, FORMAT).map_err(|e| e.to_string())?;

    // An Umber document says outright which of Umber's own modes it meant. That
    // matters for Add, whose nearest SVG name — `svg:plus` — is only
    // approximate: without the hint, reopening a document Umber itself wrote
    // would report a loss that did not happen.
    let (mode, fidelity) = match spec.umber_blend {
        Some(mode) => (mode, Fidelity::Exact),
        None => blend::nearest(&spec.composite_op),
    };
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

    let mut layer = ImportedLayer::new(spec.name.clone(), mode, pixels);
    // How many `<stack>` elements this layer was inside. Carried even though
    // the layer itself is not a folder: it is what puts the layer *in* one.
    layer.depth = spec.depth;
    layer.visible = spec.visible;
    layer.opacity = spec.opacity;
    layer.clipped = spec.clipped;
    layer.locked = spec.locked;
    layer.link = spec.link;
    layer.mask = load_mask(zip, spec, canvas, warnings);
    layer.effects = load_effects(zip, spec, warnings);
    Ok(layer)
}

/// A layer's effects, when the file names them.
///
/// A RON sequence under `umber/effects/`, written by [`docformat`] and read by
/// nothing else. See [`docformat::EFFECTS_ATTR`].
///
/// **A record that will not read costs the layer its effects and nothing
/// else**, and the layer still arrives — the mask's rule, not the saved
/// history's. The history is dropped whole because its entries are a sequence
/// in which each restores the pixels the next expects; effects are independent
/// per layer, so keeping the rest is keeping something correct rather than
/// something half right. It is a warning because it is a real loss: an effect
/// is the only record of itself, and a document reopened without one and saved
/// again has lost it for good.
///
/// **`kind` is what makes a malformed record malformed**, and that is
/// deliberate: [`crate::effect::Effect`] gives `kind` no serde default, so a
/// record whose kind is absent or is a name this build has never heard of is
/// *refused* rather than read as an arbitrary effect. Every other field
/// defaults to its own neutral, so a record written by an older build loads.
///
/// **Two effects of one kind are refused here, and it has to be here**, which
/// is the one refusal in this function that serde cannot make. A layer holds at
/// most one per kind — `LayerStack::plan_set_effect` maintains it — and
/// [`ImportedDocument::open`](super::ImportedDocument::open) installs them one
/// at a time, so a record naming two drop shadows would have the second
/// *replace* the first and the document would silently hold one where the file
/// said two. `set_effect` returning `true` both times is exactly the shrug
/// CLAUDE.md's "Partial exhaustiveness" section warns about: the refusal is
/// real at the site that makes it and invisible at the call site. Refused
/// whole rather than deduplicated, because the record is one unit and nothing
/// in it says which of the two the artist meant.
fn load_effects(
    zip: &mut Zip<'_>,
    spec: &LayerSpec,
    warnings: &mut Vec<ImportWarning>,
) -> Vec<Effect> {
    let Some(src) = spec.effects_src.as_ref() else {
        return Vec::new();
    };
    let mut read = || -> Result<Vec<Effect>, String> {
        let bytes = container::read_optional_entry(zip, src, FORMAT)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("`{src}` is not in the file"))?;
        let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
        let effects: Vec<Effect> = ron::from_str(text).map_err(|e| e.to_string())?;
        for (i, effect) in effects.iter().enumerate() {
            if effects[..i].iter().any(|e| e.kind == effect.kind) {
                return Err(format!("it names two {:?} effects", effect.kind));
            }
        }
        Ok(effects)
    };
    match read() {
        Ok(effects) => effects,
        Err(reason) => {
            warnings.push(ImportWarning::EffectsIgnored {
                layer: spec.name.clone(),
                reason,
            });
            Vec::new()
        }
    }
}

/// A layer's mask, when the file names one.
///
/// Canvas-sized and **not** put through `srgb`: the bytes went in raw, because
/// a mask is coverage rather than colour and nothing but Umber reads them.
/// `decode_png` widens the greyscale entry back to `(g, g, g, 255)`, which is
/// exactly what a mask slice holds.
///
/// A mask that is named and then cannot be read is a *warning*, not a skipped
/// layer: the pixels are all there, and a layer that comes back showing more
/// than it should is a far smaller loss than one that does not come back at
/// all. Saying so is the point — subtly wrong pixels are what the rule about
/// silent losses exists for.
fn load_mask(
    zip: &mut Zip<'_>,
    spec: &LayerSpec,
    canvas: UVec2,
    warnings: &mut Vec<ImportWarning>,
) -> Option<Vec<u8>> {
    let src = spec.mask_src.as_ref()?;
    let decoded = container::read_optional_entry(zip, src, FORMAT)
        .ok()
        .flatten()
        .and_then(|png| flat::decode_png(&png, FORMAT).ok())
        .filter(|image| image.size == canvas);
    match decoded {
        Some(image) => Some(image.rgba),
        None => {
            warnings.push(ImportWarning::MaskIgnored {
                layer: spec.name.clone(),
            });
            None
        }
    }
}

/// Last resort: the composite every ORA is required to carry.
fn flattened_fallback(
    zip: &mut Zip<'_>,
    canvas: UVec2,
    dpi: Option<f32>,
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
        layers: vec![ImportedLayer::new(
            "Merged image",
            BlendMode::Normal,
            pixels,
        )],
        active: None,
        // `mergedimage.png` already has the background composited into it, so
        // carrying the property across as well would paint it a second time.
        background: Background::Transparent,
        dpi,
        // A document whose layers could not be read has no stack for a history
        // to name, so there is nothing a patch could safely be replayed into.
        history: None,
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

    /// A nested `<stack>` comes back as a **folder**, with its two layers
    /// inside it — not flattened away, which is what this reader did before
    /// folders existed.
    ///
    /// The folder is *above* its contents in the stack, which is the invariant
    /// the whole tree rests on and the one thing here that is easy to get
    /// backwards: the first element of an ORA stack is the uppermost, and
    /// `LayerStack` is bottom first, so the group's own entry ends up last.
    #[test]
    fn a_group_comes_back_as_a_folder_holding_its_layers() {
        let doc = read(&fixtures::ora_with_group()).unwrap();
        // Two layers inside a hidden, half-opaque group, plus the group itself.
        assert_eq!(doc.layers.len(), 3);
        let names: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["B", "A", "Ink"]);
        assert_eq!(
            doc.layers.iter().map(|l| l.depth).collect::<Vec<_>>(),
            vec![1, 1, 0]
        );
        assert!(doc.layers[2].folder, "the group is the entry above its own");
        assert!(!doc.layers[0].folder && !doc.layers[1].folder);
    }

    /// The group's **eye stays on the folder** and is not folded into the
    /// layers inside it.
    ///
    /// Folding it in as well would hide them twice: a painter who opened the
    /// folder again would find every layer in it still individually shut, for a
    /// reason nothing in the file said. What hides them is
    /// `LayerStack::effective_visible`, which walks the ancestors — one rule
    /// rather than a second copy baked into the import.
    #[test]
    fn a_hidden_group_keeps_its_eye_rather_than_shutting_its_layers() {
        let doc = read(&fixtures::ora_with_group()).unwrap();
        assert!(
            !doc.layers[2].visible,
            "the folder carries the hidden group"
        );
        assert!(
            doc.layers[..2].iter().all(|l| l.visible),
            "the layers inside were not hidden individually"
        );

        // And the stack agrees once it is built: nothing in the group shows.
        let opened = doc.open();
        assert!(!opened.stack.any_visible());
        assert!(!opened.stack.effective_visible(0));
    }

    /// A group's **opacity** is the one thing a pass-through folder cannot
    /// hold, so it is still folded into the layers inside and still reported.
    /// That fold is only exact where the children do not overlap, which is why
    /// it is a warning rather than a silent conversion — and why a folder with
    /// an opacity of its own is group compositing and is not built. See
    /// `docs/layer-folders.md`.
    #[test]
    fn a_groups_opacity_is_still_folded_in_and_still_reported() {
        let doc = read(&fixtures::ora_with_group()).unwrap();
        assert_eq!(doc.layers[0].opacity, 0.5);
        assert_eq!(doc.layers[1].opacity, 0.5);
        assert_eq!(
            doc.layers[2].opacity, 1.0,
            "the folder itself has no opacity to carry"
        );
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::GroupOpacityFolded { .. }))
        );
        assert!(
            !doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::GroupFlattened { .. })),
            "the group was kept, so nothing was flattened"
        );
    }

    /// `<stack/>` — an empty group written self-closing — never gets an `End`
    /// event, so the depth bookkeeping has a branch of its own. Umber's writer
    /// does not produce that form, which is exactly why it needs a test: it
    /// arrives from other applications and nothing else exercises it.
    ///
    /// Getting it wrong leaves the depth counter one too deep for the rest of
    /// the file, so every layer after it comes back nested inside a group that
    /// closed before them.
    #[test]
    fn a_self_closing_empty_group_does_not_swallow_what_follows_it() {
        let doc = read(&fixtures::ora_with_empty_group()).unwrap();

        let shape: Vec<(&str, u8, bool)> = doc
            .layers
            .iter()
            .map(|l| (l.name.as_str(), l.depth, l.folder))
            .collect();
        assert_eq!(
            shape,
            vec![("After", 0, false), ("Empty", 0, true)],
            "the empty group must close before the layer after it"
        );
        // And the stack it opens as is a tree, with the folder holding nothing.
        let opened = doc.open();
        assert_eq!(opened.stack.subtree(1), 1..2, "the group is empty");
        assert_eq!(opened.stack.pixel_count(), 1);
    }

    /// A layer whose blend mode would erase the one below it is reported, not
    /// composited over it in silence.
    ///
    /// `svg:dst-in` is the mode a mask written as a layer would carry, and it
    /// is also just a blend mode Krita offers on ordinary layers. Umber has
    /// neither, so the layer arrives Normal — which over the layer below is a
    /// large change to the picture, and exactly the kind that has to be named.
    /// The same holds for a nested `<stack>` that carries it, since a folder
    /// here is pass-through and can hold no mode at all.
    #[test]
    fn a_layer_that_would_erase_the_one_below_is_named() {
        let ora = fixtures::ora(
            1,
            1,
            &[
                OraLayer::new("Cutout", 1, 1, &[0, 0, 0, 128]).op("svg:dst-in"),
                OraLayer::new("Paint", 1, 1, &[255, 0, 0, 255]),
            ],
        );
        let doc = read(&ora).unwrap();
        assert_eq!(doc.layers[1].name, "Cutout");
        assert_eq!(doc.layers[1].blend, BlendMode::Normal);
        assert!(
            doc.layers[1].mask.is_none(),
            "a dst-in layer is a layer, never somebody's mask guessed at"
        );
        assert_eq!(
            doc.warnings,
            vec![ImportWarning::BlendDropped {
                layer: "Cutout".into(),
                source: "dst-in".into()
            }]
        );
    }

    // --- layer effects --------------------------------------------------

    /// A record written by hand, read as the effects it names, and reaching
    /// the layer it was written for rather than the one beside it.
    ///
    /// The fixture lists the effected layer *first*, which is uppermost in an
    /// ORA and last in a `LayerStack` — so a reader that read the record
    /// against the file's order rather than the stack's would put a shadow on
    /// the wrong layer, and this is the ordering that catches it.
    #[test]
    fn a_layers_effects_are_read_onto_that_layer() {
        let ora = fixtures::ora(
            2,
            2,
            &[
                OraLayer::new("Top", 2, 2, &[0, 0, 255, 255])
                    .effects("[(kind: DropShadow, angle: 90.0, distance: 4.0)]"),
                OraLayer::new("Bottom", 2, 2, &[255, 0, 0, 255]),
            ],
        );
        let doc = read(&ora).unwrap();
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);

        assert_eq!(doc.layers[0].name, "Bottom");
        assert!(doc.layers[0].effects.is_empty());
        assert_eq!(doc.layers[1].name, "Top");
        assert_eq!(doc.layers[1].effects.len(), 1);
        let effect = doc.layers[1].effects[0];
        assert_eq!(effect.kind, crate::effect::EffectKind::DropShadow);
        assert_eq!(effect.angle, 90.0);
        assert_eq!(effect.distance, 4.0);
        // Every field the record left out reads at its own **neutral**, not at
        // the kind's own starting point — `Effect::drop_shadow` opens at 5.0
        // softness and 0.75 opacity, and neither may leak in here. This is the
        // per-field `#[serde(default)]` rule seen from the file rather than
        // from the struct.
        assert_eq!(effect.softness, 0.0);
        assert_eq!(effect.opacity, 1.0);
        assert!(effect.enabled, "an effect in a file is one somebody made");

        // And it survives being installed in a stack.
        let opened = doc.open();
        assert!(opened.stack.get(0).unwrap().effects().is_empty());
        assert_eq!(opened.stack.get(1).unwrap().effects(), &[effect]);
    }

    /// **A record with no `kind` is refused, and the layer still arrives.**
    ///
    /// Two rules meeting. `Effect` deliberately gives `kind` no serde default,
    /// so a record that omits it — or names a kind this build has never heard
    /// of — cannot be read as an arbitrary effect; that refusal has to survive
    /// the trip through the file, which is what the first half checks. And the
    /// refusal costs the *layer* nothing: the picture is all there, and losing
    /// it over a decoration would be far worse than losing the decoration.
    ///
    /// The named loss is the whole point. An effect is the only record of
    /// itself, so a document reopened without one and saved again has lost it
    /// for good — which is exactly the silent loss this module refuses.
    #[test]
    fn an_effects_record_with_no_kind_is_refused_and_the_layer_still_arrives() {
        for record in [
            // No kind at all.
            "[(spread: 8.0)]",
            // A kind from a build this one does not know.
            "[(kind: InnerGlow, spread: 8.0)]",
            // One good effect and one bad: the sequence is one record, so the
            // good one goes with it rather than being read past.
            "[(kind: Outline),(spread: 8.0)]",
            // Not RON at all.
            "not a record",
            // Empty, which is a truncated write rather than "no effects".
            "",
            // **Two of one kind**, which serde cannot refuse and the install
            // loop would not either: `set_effect` replaces whatever the layer
            // held of that kind, so the second would quietly overwrite the
            // first and the document would hold one where the file said two.
            // The one refusal in `load_effects` that is not the deserialiser's.
            "[(kind: Outline, spread: 1.0),(kind: Outline, spread: 9.0)]",
        ] {
            let ora = fixtures::ora(
                2,
                2,
                &[OraLayer::new("Ink", 2, 2, &[1, 2, 3, 255]).effects(record)],
            );
            let doc = read(&ora).unwrap();
            assert_eq!(doc.layers.len(), 1, "{record}");
            assert!(doc.layers[0].effects.is_empty(), "{record}");
            assert!(
                doc.warnings.iter().any(
                    |w| matches!(w, ImportWarning::EffectsIgnored { layer, .. } if layer == "Ink")
                ),
                "{record} was dropped in silence: {:?}",
                doc.warnings
            );
            // The pixels are untouched, which is the half that matters. The
            // fixture places every layer at (1,1), so that is where to look.
            assert_eq!(&doc.layers[0].pixels[12..16], [1, 2, 3, 255], "{record}");
        }
    }

    /// A record the file names and does not contain. Same answer, and it has
    /// to be reached without the `?` in `read` turning it into a refusal of
    /// the whole document.
    #[test]
    fn an_effects_record_that_is_not_in_the_file_is_named() {
        let ora = fixtures::ora(
            2,
            2,
            &[OraLayer::new("Ink", 2, 2, &[1, 2, 3, 255]).effects_named_but_absent()],
        );
        let doc = read(&ora).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert!(doc.layers[0].effects.is_empty());
        assert!(
            doc.warnings
                .iter()
                .any(|w| matches!(w, ImportWarning::EffectsIgnored { .. })),
            "{:?}",
            doc.warnings
        );
    }

    /// A layer that names no record says nothing at all.
    ///
    /// The other half of every warning rule here: a list that speaks about
    /// every document is a list nobody reads, and every ORA from every other
    /// application is in this case.
    #[test]
    fn a_layer_with_no_effects_raises_nothing() {
        let doc = read(&two_layer_ora()).unwrap();
        assert!(doc.layers.iter().all(|l| l.effects.is_empty()));
        assert!(doc.warnings.is_empty(), "{:?}", doc.warnings);
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
