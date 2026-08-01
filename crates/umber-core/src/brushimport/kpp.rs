//! Krita paintop presets (`.kpp`).
//!
//! The single highest-value importer here: `.kpp` is what David Revoy,
//! Raghukamath and GDQuest all publish, so one reader unlocks three of the
//! packs `docs/brush-sources.md` lists.
//!
//! A `.kpp` is a **PNG** — the thumbnail you see in Krita's brush chooser —
//! carrying the settings in a text chunk keyed `preset`, usually `zTXt`
//! (deflated) and occasionally `tEXt`. The chunk holds XML:
//!
//! ```xml
//! <Preset embedded_resources="1" name="Pencil" paintopid="paintbrush">
//!   <resources>
//!     <resource name="bristle" filename="bristle.png" type="brushes">
//!       <![CDATA[iVBORw0KGgo…]]></resource>
//!   </resources>
//!   <param name="OpacityValue" type="string"><![CDATA[1]]></param>
//!   <param type="internal" name="PressureSize">true</param>
//!   <param name="brush_definition" type="string"><![CDATA[
//!     <Brush type="auto_brush" spacing="0.1" angle="0">
//!       <MaskGenerator type="circle" id="default" diameter="40"
//!                      ratio="0.65" hfade="0.89" vfade="0.89" spikes="2"/>
//!     </Brush>]]></param>
//! </Preset>
//! ```
//!
//! Three things about that are easy to get wrong:
//!
//! - **A `param` may or may not use CDATA.** `type="internal"` writes the value
//!   as element text; `type="string"` wraps it. Both are read here.
//! - **`Pressure<Name>` is *not* "pressure drives this".** It is Krita's
//!   historical spelling of "the `<Name>` dynamic is switched on"; which input
//!   drives it is in `<Name>Sensor`. Reading it the obvious way turns every
//!   speed- and angle-driven dynamic into a pressure one — and it gates the
//!   setting's **value** as well as its curve. Krita writes `ScatterValue` at
//!   its default of 1 into every preset whether or not scattering is on, so a
//!   reader that consults the value alone sprays 93 of the 119 presets in the
//!   fetched packs.
//! - **`angle` is in radians** in `brush_definition`, and in degrees nowhere.
//!
//! # Fade is hardness, not softness
//!
//! `MaskGenerator@hfade` is the fraction of the radius that stays **fully
//! opaque**, so `hfade="1"` is a hard-edged disc and `hfade="0"` the softest
//! dab the generator draws. That is the same quantity `dab.wgsl` calls
//! hardness, and it carries across unchanged.
//!
//! This read `1 - hfade` for a while, on the reasonable-sounding grounds that a
//! setting called "fade" ought to be softness. It is not, and reasoning was the
//! wrong way to settle it. A `.kpp` is a PNG whose image is *Krita's own
//! preview of the brush*, so the packs answer the question directly:
//!
//! | Preset | generator | `hfade` | Krita's preview |
//! |---|---|---|---|
//! | GDQuest "Ink Brush" | `default` | 1 | a crisp black line |
//! | Raghukamath "Inkbrush" | `default` | 1 | a crisp black line |
//! | GDQuest "Ink Rough" | `default` | 0.78 | crisp, faintly feathered |
//! | Raghukamath "Basic Render" | `default` | 0.67 | plainly soft |
//! | Deevad "Basic Oval Brush" | `default` | 0.89 | hard-edged |
//! | Deevad "Eraser Kneaded Soft" | `gauss` | 0 | a wide feathered fade |
//! | Deevad "Pencil H Sketch" | `gauss` | 0.15 | soft, faint sketch lines |
//!
//! Every one of those was imported inside out. Three mask generators share the
//! attribute and all three run the same way round: `default`
//! (`KisCircleMaskGenerator`) fades from `hfade` of the radius outwards,
//! `gauss` blurs by `1 - hfade`, and `soft` (`KisCurveMaskGenerator`) ignores
//! `hfade` entirely and states its falloff in `softness_curve`, which
//! [`hardness_from_curve`] reads.
//!
//! # The tip
//!
//! `brush_definition@type` is `auto_brush` — a generated ellipse, which Umber
//! draws exactly — or a *predefined* brush (`png_brush`, `gbr_brush`,
//! `svg_brush`) naming a file. A preset with `embedded_resources` carries that
//! file base64-encoded in `<resources>`; one without expects Krita's resource
//! database to have it, and in a `.bundle` it is in `brushes/`. When neither is
//! true the brush arrives round and [`KppPreset::missing_tip`] names what was
//! wanted, because a stamp brush silently painting round is the failure this
//! whole reader exists to avoid.
//!
//! **A Krita PNG brush is inverted relative to a `.gbr`**: white is no paint
//! and black is full, the opposite of [`TipMask`]'s convention. Reading one the
//! `.gbr` way gives a solid square with a hole in it.
//!
//! # What is dropped
//!
//! - **Every paint engine except `paintbrush` and `colorsmudge`.** `deformbrush`
//!   moves pixels around, `experimentbrush` fills an outline, `hairybrush`
//!   simulates bristles — these are *different programs*, not settings, and a
//!   round dab wearing their name would be pure invention. They are refused by
//!   name rather than approximated.
//! - **Masking brushes** — a second brush multiplied into the first.
//! - **Edge sharpening.** Krita's Sharpness thresholds the finished mask into a
//!   hard, aliased edge; `dab.wgsl` antialiases unconditionally and has nothing
//!   to switch off. It is the whole of a pixel-art brush, so it is named rather
//!   than ignored.
//! - **Paper texture**, **mirrored dabs**, **paint thickness (impasto)**.
//! - **Square and star mask generators**, exactly as [`super::vbr`] drops them.
//! - **Brush-tip randomness and density**, which perturb the generated mask
//!   rather than the dab.
//! - **Flow build-up.** Krita composites each dab, so flow below 1 darkens
//!   where a stroke crosses itself. Umber takes a `max` of coverage and applies
//!   opacity once at commit — the wet-layer design in `CLAUDE.md` — so flow is
//!   folded into stroke opacity instead. Same trade, and the same reason, as
//!   `opaque_linearize` in [`super::mypaint`].
//! - **Auto-spacing.** `useAutoSpacing` asks Krita to derive spacing from the
//!   tip; the recorded `spacing` is used instead of guessing at the formula.
//! - **Non-pressure sensors.** A dynamic driven by `speed`, `fuzzy`,
//!   `drawingangle`, `tilt` or `time` has no input here, with two exceptions
//!   that are read for what they mean rather than as curves: a `drawingangle`
//!   rotation is "this dab follows the stroke", and a `fuzzy` one is
//!   [`Brush::dab_angle_jitter`].

use std::collections::BTreeMap;

use quick_xml::events::Event;

use crate::brush::{Brush, BrushMode};
use crate::curve::ResponseCurve;
use crate::preset::PresetError;
use crate::tip::TipMask;

use super::{gbr, gih};

/// Krita paint engines Umber can render something honest from.
///
/// `paintbrush` is the ordinary stamped dab and `colorsmudge` is the same thing
/// plus canvas pickup, which is exactly Umber's [`Brush::smudge`]. Everything
/// else in Krita is a separate engine.
const SUPPORTED_PAINTOPS: [&str; 2] = ["paintbrush", "colorsmudge"];

/// The largest preset chunk this will inflate.
///
/// The settings themselves are a few kilobytes, but a preset with
/// `embedded_resources` carries its tips base64-encoded *inside* them, and a
/// `.gih` tip runs to a quarter of a megabyte before encoding — Revoy's
/// "Bristle Thick Textured" inflates past two. Sixteen leaves room for a brush
/// built out of several while still refusing a decompression bomb.
const MAX_PRESET_BYTES: usize = 16 << 20;

/// A decoded Krita preset.
#[derive(Clone, Debug)]
pub struct KppPreset {
    pub name: String,
    pub brush: Brush,
    /// The bitmap tip, when the preset carried one or the caller could find it.
    pub tip: Option<TipMask>,
    /// The file name of a predefined tip this preset wanted and nothing could
    /// supply. The brush still paints — round — and the caller says so.
    pub missing_tip: Option<String>,
    pub dropped: Vec<&'static str>,
}

/// Decode a standalone `.kpp`.
///
/// A preset naming a predefined brush it does not embed arrives round; use
/// [`from_kpp_in`] when there is somewhere to look the file up, which for a
/// `.bundle` is its `brushes/` directory.
pub fn from_kpp(bytes: &[u8]) -> Result<KppPreset, PresetError> {
    from_kpp_in(bytes, &|_| None)
}

/// Decode a `.kpp`, resolving a predefined tip through `brushes`.
pub fn from_kpp_in(
    bytes: &[u8],
    brushes: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> Result<KppPreset, PresetError> {
    let xml = preset_chunk(bytes)?;
    let preset = Preset::parse(&xml)?;

    let paintop = preset
        .params
        .get("paintop")
        .map(String::as_str)
        .unwrap_or(&preset.paintop);
    if !SUPPORTED_PAINTOPS.contains(&paintop) {
        return Err(PresetError::Malformed(
            None,
            format!(
                "`{paintop}` is one of Krita's other painting engines, not a brush setting — \
                 Umber has no equivalent to approximate it with"
            ),
        ));
    }
    let smudging = paintop == "colorsmudge";

    let definition = preset
        .params
        .get("brush_definition")
        .ok_or_else(|| malformed("it has no brush_definition"))?;
    let tip_spec = TipSpec::parse(definition)?;

    let mut dropped = Vec::new();
    dropped.extend(tip_spec.dropped.iter().copied());
    if preset.flag("MaskingBrush/Enabled") {
        dropped.push("masking brushes");
    }
    if preset.flag("Texture/Enabled") {
        dropped.push("paper texture");
    }
    if preset.flag("PressureMirror") {
        dropped.push("mirrored dabs");
    }
    if preset.flag("PressurePaintThickness") {
        dropped.push("paint thickness");
    }
    // Krita's Sharpness thresholds the finished mask into a hard, aliased edge.
    // That is not a hardness — `dab.wgsl` antialiases unconditionally, sized
    // from the dab's short axis — and it is the whole of a pixel-art brush:
    // GDQuest's "PixelArt OnePixel" is a one-pixel dab that is *only* one pixel
    // because of this. Naming it is what keeps such a brush out of the shipped
    // library rather than shipping a soft blob under its author's name.
    if preset.flag("PressureSharpness") && preset.number("SharpnessValue").is_some_and(|v| v > 0.0)
    {
        dropped.push("edge sharpening");
    }

    // --- the tip ------------------------------------------------------------
    let mut missing_tip = None;
    let mut tip = None;
    if let Some(file) = &tip_spec.file {
        let raw = preset
            .resources
            .get(file.as_str())
            .cloned()
            .or_else(|| brushes(file));
        match raw {
            Some(raw) => {
                let decoded = decode_tip(file, &raw)?;
                for loss in decoded.dropped {
                    if !dropped.contains(&loss) {
                        dropped.push(loss);
                    }
                }
                tip = Some(decoded.mask);
            }
            None => missing_tip = Some(file.clone()),
        }
    }

    // --- size ---------------------------------------------------------------
    //
    // A generated brush states its diameter; a predefined one is its bitmap
    // scaled. Either way Krita's size dynamic *multiplies* that, so the peak of
    // the size curve is what Umber's `size` has to be — `radius_at` then
    // reproduces Krita at every sample point.
    let base_size = match (&tip, tip_spec.diameter) {
        (Some(mask), _) => mask.width().max(mask.height()) as f32 * tip_spec.scale,
        (None, Some(diameter)) => diameter,
        // A predefined tip that could not be found: its scale is a multiple of
        // a picture nobody has. Umber's default size is a better answer than
        // one derived from nothing.
        (None, None) => Brush::default().size,
    };

    let size = preset.dynamic("Size");
    let opacity = preset.dynamic("Opacity");
    let scatter = preset.dynamic("Scatter");

    let default = Brush::default();
    let (min_size_ratio, size_curve, pressure_size) = size.split(default.min_size_ratio.max(0.01));

    // Krita's opacity and flow multiply: opacity is the stroke's and flow is
    // the dab's. Umber applies opacity once at commit, so the two collapse into
    // one number — see the module docs for why that is the honest trade.
    let flow = preset.number("FlowValue").unwrap_or(1.0).clamp(0.0, 1.0);
    let opacity_peak = preset.number("OpacityValue").unwrap_or(1.0).clamp(0.0, 1.0);

    // --- the rest -----------------------------------------------------------
    let mode = if preset.flag("EraserMode")
        || preset
            .params
            .get("CompositeOp")
            .is_some_and(|op| op == "erase")
    {
        BrushMode::Erase
    } else {
        BrushMode::Paint
    };

    // Krita has spelled the airbrush option two ways. `PaintOpSettings/` is the
    // older one and `AirbrushOption/` the current; both are in the fetched
    // packs — 45 presets use the first, 52 the second — so knowing only one
    // silently imports an airbrush as a distance-driven brush. GDQuest's
    // "Airbrush", which asks for 1000 dabs a second, was exactly that.
    let airbrushing =
        preset.flag("PaintOpSettings/isAirbrushing") || preset.flag("AirbrushOption/isAirbrushing");
    let dabs_per_second = if airbrushing {
        preset
            .number("PaintOpSettings/rate")
            .or_else(|| preset.number("AirbrushOption/rate"))
            .unwrap_or(0.0)
            .clamp(0.0, 300.0)
    } else {
        0.0
    };

    // Krita spells "this dab turns to follow the stroke" as a `drawingangle`
    // sensor on rotation, and "point it anywhere" as a `fuzzy` one. Neither is
    // a curve; both are what the dab *is*. A rake read as a nib is immediately
    // visible in a curve, which is why they are worth this much attention.
    let rotation = preset.sensor_id("Rotation");
    let rotates = preset.flag("PressureRotation");
    let dab_angle_follows_stroke = rotates && rotation.as_deref() == Some("drawingangle");
    let dab_angle_jitter = if rotates && rotation.as_deref() == Some("fuzzy") {
        360.0
    } else {
        0.0
    };

    // A smudging brush picks colour up off the canvas. Krita states the pickup
    // and the deposit separately (`SmudgeRate` and `ColorRate`); Umber has one
    // mix, so the pickup is the one that carries across.
    let (smudge, smudge_radius) = if smudging {
        (
            preset
                .number("SmudgeRateValue")
                .unwrap_or(1.0)
                .clamp(0.0, 1.0),
            preset
                .number("SmudgeRadiusValue")
                .unwrap_or(1.0)
                .clamp(0.25, 8.0),
        )
    } else {
        (0.0, default.smudge_radius)
    };
    if smudging && preset.number("ColorRateValue").is_some_and(|r| r > 0.0) {
        dropped.push("a separate paint-deposit rate");
    }

    let (min_scatter_ratio, scatter_curve, pressure_scatter) = scatter.split(0.0);
    let (_, opacity_curve, pressure_opacity) = opacity.split(0.0);

    // A dynamic that is switched on but driven by something Umber has no input
    // for arrives as a constant. Worth naming: those brushes are the ones that
    // will feel dead rather than wrong.
    if preset.has_foreign_sensor(["Size", "Opacity", "Scatter"]) {
        dropped.push("dynamics driven by speed, tilt or stroke position");
    }

    let brush = Brush {
        size: (base_size * size.peak).clamp(Brush::MIN_SIZE, Brush::MAX_SIZE),
        min_size_ratio,
        size_curve,
        pressure_size,
        // A bitmap tip *replaces* the procedural falloff, so hardness has
        // nothing left to shape and the file states none for a predefined
        // brush either.
        hardness: tip_spec.hardness.unwrap_or(default.hardness),
        opacity: (opacity_peak * flow * opacity.peak).clamp(0.0, 1.0),
        opacity_curve,
        pressure_opacity,
        spacing: tip_spec.spacing.unwrap_or(default.spacing),
        mode,
        dabs_per_second,
        dab_ratio: tip_spec.dab_ratio,
        dab_angle: tip_spec.angle,
        dab_angle_follows_stroke,
        dab_angle_jitter,
        // `ScatterValue` only means anything when Krita's scatter option is
        // switched on: `KisScatterOption::apply` returns the unscattered
        // position when `!isChecked()`, and `isChecked()` is `PressureScatter`
        // — the same "`Pressure<Name>` is the enable flag" rule the module docs
        // open with, which was being applied to the *curve* and not to the
        // value. Krita leaves `ScatterValue` at its default of 1 in a preset
        // that never scatters, so reading it unconditionally turned twenty of
        // the twenty-one shipped Krita brushes into sprays.
        scatter: if preset.flag("PressureScatter") {
            scatter.peak * preset.number("ScatterValue").unwrap_or(0.0)
        } else {
            0.0
        },
        min_scatter_ratio,
        scatter_curve,
        pressure_scatter,
        smudge,
        smudge_radius,
        ..default
    };

    Ok(KppPreset {
        name: preset.name,
        brush,
        tip,
        missing_tip,
        dropped,
    })
}

/// What reading this `.kpp` will throw away.
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    let Ok(preset) = from_kpp(bytes) else {
        return Vec::new();
    };
    let mut out = preset.dropped;
    if preset.missing_tip.is_some() {
        out.push(MISSING_TIP);
    }
    out
}

/// A preset naming a bitmap tip nobody has. The brush arrives round, which is
/// most of a stamp brush gone; both this reader and [`super::bundle`] can
/// produce it, so the sentence is written once.
pub const MISSING_TIP: &str = "bitmap tips stored outside the file";

// ---------------------------------------------------------------------------
// The PNG wrapper
// ---------------------------------------------------------------------------

/// Pull the `preset` text chunk out of a `.kpp`.
///
/// **All three of PNG's text chunks turn up in the wild**, and a reader that
/// knows only one silently rejects a quarter of a real bundle: of Revoy's 46
/// presets, 33 use `zTXt`, 11 use `iTXt` and the remainder `tEXt`. There is no
/// pattern to it — it is whichever Krita felt like on the day — so all three
/// are looked at.
///
/// The two Latin-1 chunks need a further step. `png` hands them back one byte
/// per `char`, because that is what the specification says they hold; Krita
/// writes UTF-8 into them regardless, so the chars go back to bytes and are
/// re-read. Skipping that turns "Aérographe" into "AÃ©rographe" in the picker.
/// `iTXt` really is UTF-8 and must *not* be put through the same step.
fn preset_chunk(bytes: &[u8]) -> Result<String, PresetError> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let reader = decoder.read_info().map_err(|e| {
        PresetError::Malformed(
            None,
            format!("a .kpp is a PNG and this one will not open ({e})"),
        )
    })?;
    let info = reader.info();
    let failed =
        |e| PresetError::Malformed(None, format!("its settings would not decompress ({e})"));

    if let Some(chunk) = info.utf8_text.iter().find(|c| c.keyword == "preset") {
        let mut chunk = chunk.clone();
        chunk
            .decompress_text_with_limit(MAX_PRESET_BYTES)
            .map_err(failed)?;
        return chunk.get_text().map_err(failed);
    }

    let latin1 = if let Some(chunk) = info
        .compressed_latin1_text
        .iter()
        .find(|c| c.keyword == "preset")
    {
        let mut chunk = chunk.clone();
        chunk
            .decompress_text_with_limit(MAX_PRESET_BYTES)
            .map_err(failed)?;
        chunk.get_text().map_err(failed)?
    } else if let Some(chunk) = info
        .uncompressed_latin1_text
        .iter()
        .find(|c| c.keyword == "preset")
    {
        chunk.text.clone()
    } else {
        return Err(malformed(
            "the PNG carries no `preset` chunk, so it is a picture and not a brush",
        ));
    };

    // The chunk's bytes, recovered: `decode_iso_8859_1` gave one char per byte,
    // so this cannot lose anything. If they are valid UTF-8 the writer was
    // Krita smuggling UTF-8 through a Latin-1 chunk; if they are not, it was a
    // writer that meant Latin-1, and the string `png` built is already right.
    // Trying UTF-8 *first* is what matters: the other order mangles every
    // accented brush name in the Revoy bundle.
    let raw: Vec<u8> = latin1.chars().map(|c| c as u8).collect();
    Ok(String::from_utf8(raw).unwrap_or(latin1))
}

// ---------------------------------------------------------------------------
// The settings XML
// ---------------------------------------------------------------------------

struct Preset {
    name: String,
    paintop: String,
    params: BTreeMap<String, String>,
    /// Embedded files, by the name `brush_definition` refers to them by.
    resources: BTreeMap<String, Vec<u8>>,
}

impl Preset {
    fn parse(xml: &str) -> Result<Self, PresetError> {
        let mut reader = quick_xml::Reader::from_str(xml.trim_start_matches('\u{feff}'));
        reader.config_mut().trim_text(false);

        let mut out = Self {
            name: String::new(),
            paintop: String::new(),
            params: BTreeMap::new(),
            resources: BTreeMap::new(),
        };
        // The element currently collecting text, and what to file it under.
        let mut collecting: Option<(bool, String)> = None;
        let mut text = String::new();

        loop {
            match reader.read_event() {
                Ok(Event::Start(e)) => {
                    let attrs = attributes(&e)?;
                    match e.local_name().as_ref() {
                        b"Preset" => {
                            out.name = attrs.get("name").cloned().unwrap_or_default();
                            out.paintop = attrs.get("paintopid").cloned().unwrap_or_default();
                        }
                        b"param" => {
                            text.clear();
                            collecting =
                                Some((false, attrs.get("name").cloned().unwrap_or_default()));
                        }
                        b"resource" => {
                            text.clear();
                            // Referred to by file name, which is what
                            // `brush_definition` records.
                            collecting = Some((
                                true,
                                attrs
                                    .get("filename")
                                    .or_else(|| attrs.get("name"))
                                    .cloned()
                                    .unwrap_or_default(),
                            ));
                        }
                        _ => {}
                    }
                }
                Ok(Event::Text(e)) => {
                    if collecting.is_some() {
                        text.push_str(&String::from_utf8_lossy(&e));
                    }
                }
                // `type="string"` wraps its value in CDATA and `type="internal"`
                // does not, so both events feed the same buffer. Missing either
                // one loses half the settings in every file.
                Ok(Event::CData(e)) => {
                    if collecting.is_some() {
                        text.push_str(&String::from_utf8_lossy(&e));
                    }
                }
                Ok(Event::End(e)) => {
                    let ended = matches!(e.local_name().as_ref(), b"param" | b"resource");
                    if let Some((resource, key)) = collecting.take()
                        && ended
                    {
                        if resource {
                            if let Some(data) = base64(&text) {
                                out.resources.insert(key, data);
                            }
                        } else {
                            out.params.insert(key, text.trim().to_string());
                        }
                    } else if !ended {
                        // A `</resources>` or `</Preset>`: nothing was open.
                    }
                    text.clear();
                }
                Ok(Event::Eof) => break,
                Err(e) => {
                    return Err(PresetError::Malformed(
                        None,
                        format!("its settings are not readable XML ({e})"),
                    ));
                }
                _ => {}
            }
        }

        if out.params.is_empty() {
            return Err(malformed("its `preset` chunk holds no settings"));
        }
        Ok(out)
    }

    fn number(&self, key: &str) -> Option<f32> {
        self.params.get(key)?.parse().ok()
    }

    fn flag(&self, key: &str) -> bool {
        self.params.get(key).is_some_and(|v| v == "true")
    }

    /// Which input drives a dynamic, out of `<Name>Sensor`.
    ///
    /// The sensor is itself a scrap of XML — `<params id="pressure"><curve>…`
    /// — so this reads the `id` off it rather than parsing it properly, which
    /// is enough: the id is the only part Umber can act on.
    fn sensor_id(&self, name: &str) -> Option<String> {
        let sensor = self.params.get(&format!("{name}Sensor"))?;
        let at = sensor.find("id=\"")? + 4;
        let end = sensor[at..].find('"')?;
        Some(sensor[at..at + end].to_string())
    }

    /// One of Krita's curve-driven settings, read the way Umber states a
    /// dynamic.
    ///
    /// `Pressure<Name>` is the *enabled* flag — see the module docs — and the
    /// curve is `<Name>commonCurve` when `<Name>UseSameCurve` says so, which it
    /// almost always does.
    fn dynamic(&self, name: &str) -> Dynamic {
        let enabled = self.flag(&format!("Pressure{name}"));
        let by_pressure = self.sensor_id(name).as_deref() == Some("pressure");
        let live = enabled && by_pressure && self.flag(&format!("{name}UseCurve"));
        if !live {
            return Dynamic::flat();
        }
        let points = self
            .params
            .get(&format!("{name}commonCurve"))
            .and_then(|text| curve_points(text))
            .or_else(|| {
                let sensor = self.params.get(&format!("{name}Sensor"))?;
                let at = sensor.find("<curve>")? + 7;
                let end = sensor[at..].find("</curve>")?;
                curve_points(&sensor[at..at + end])
            });
        match points {
            Some(points) => Dynamic::from_samples(points),
            None => Dynamic::flat(),
        }
    }

    /// Whether any of these dynamics is switched on but driven by an input
    /// Umber does not have.
    fn has_foreign_sensor<const N: usize>(&self, names: [&str; N]) -> bool {
        names.iter().any(|name| {
            self.flag(&format!("Pressure{name}"))
                && self.sensor_id(name).is_some_and(|id| id != "pressure")
        })
    }
}

/// Krita's curve, sampled at the five pressures a [`ResponseCurve`] holds.
struct Dynamic {
    /// The curve's largest output, which multiplies the setting's own value.
    peak: f32,
    samples: [f32; ResponseCurve::N],
    live: bool,
}

impl Dynamic {
    fn flat() -> Self {
        Self {
            peak: 1.0,
            samples: [1.0; ResponseCurve::N],
            live: false,
        }
    }

    fn from_samples(points: Vec<(f32, f32)>) -> Self {
        let mut samples = [0.0f32; ResponseCurve::N];
        for (i, sample) in samples.iter_mut().enumerate() {
            *sample = piecewise(&points, ResponseCurve::x_of(i)).clamp(0.0, 1.0);
        }
        let peak = samples.iter().copied().fold(f32::MIN, f32::max);
        let floor = samples.iter().copied().fold(f32::MAX, f32::min);
        Self {
            peak,
            samples,
            // A curve that never moves is Krita's editor leaving a default
            // behind, not a dynamic — the same judgement `mypaint::span` makes,
            // and for the same reason.
            live: peak > 0.0 && peak - floor > 0.01,
        }
    }

    /// Umber's `(min_ratio, curve, enabled)` triple: the curve carries the
    /// shape and the ratio carries the range, so `peak × (ratio + (1 − ratio) ×
    /// curve(p))` reproduces Krita at all five points.
    fn split(&self, when_flat: f32) -> (f32, ResponseCurve, bool) {
        if !self.live {
            return (when_flat, ResponseCurve::LINEAR, false);
        }
        let floor = self.samples.iter().copied().fold(f32::MAX, f32::min);
        let span = self.peak - floor;
        let mut points = [0.0f32; ResponseCurve::N];
        for (point, sample) in points.iter_mut().zip(self.samples) {
            *point = ((sample - floor) / span).clamp(0.0, 1.0);
        }
        (
            (floor / self.peak).clamp(0.0, 1.0),
            ResponseCurve { points },
            true,
        )
    }
}

/// `0,0;0.5,0.25;1,1;` — Krita's curve, as pairs.
fn curve_points(text: &str) -> Option<Vec<(f32, f32)>> {
    let mut out = Vec::new();
    for pair in text.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (x, y) = pair.split_once(',')?;
        out.push((x.trim().parse().ok()?, y.trim().parse().ok()?));
    }
    (out.len() >= 2).then_some(out)
}

/// Evaluate a piecewise-linear curve, holding its end values outside the range.
fn piecewise(points: &[(f32, f32)], x: f32) -> f32 {
    if points.is_empty() {
        return 0.0;
    }
    if x <= points[0].0 {
        return points[0].1;
    }
    let last = points[points.len() - 1];
    if x >= last.0 {
        return last.1;
    }
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if x <= x1 {
            if (x1 - x0).abs() < f32::EPSILON {
                return y1;
            }
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    last.1
}

/// Krita's `softness_curve`, read as an Umber hardness.
///
/// `id="soft"` is `KisCurveMaskGenerator`, whose shape is a curve from the
/// centre of the dab (`x = 0`) to its edge (`x = 1`) giving the coverage there.
/// Umber's dab is `1 - smoothstep(hardness, 1, d)`, whose half-coverage radius
/// is `(hardness + 1) / 2` — so measuring where the curve crosses a half and
/// inverting that gives the same quantity `hfade` gives for the other two
/// generators, and the two paths stay comparable.
///
/// A curve that never reaches a half is softer than any hardness can express;
/// zero — the softest dab Umber draws — is the honest floor for it, and
/// GDQuest's airbrush, whose curve peaks at 0.4 in the middle, is exactly that
/// brush.
fn hardness_from_curve(text: &str) -> Option<f32> {
    let points = curve_points(text)?;
    let mut half = 0.0f32;
    // 64 steps across the radius: finer than the difference between two
    // adjacent hardnesses anyone can see, and it avoids having to reason about
    // a curve whose control points are not sorted.
    for i in 0..=64 {
        let x = i as f32 / 64.0;
        if piecewise(&points, x) >= 0.5 {
            half = x;
        }
    }
    Some((2.0 * half - 1.0).clamp(0.0, 1.0))
}

// ---------------------------------------------------------------------------
// brush_definition
// ---------------------------------------------------------------------------

/// What `brush_definition` says the dab is made of.
struct TipSpec {
    /// A predefined brush's file name, when the tip is a bitmap.
    file: Option<String>,
    /// How much the bitmap is scaled by.
    scale: f32,
    /// A generated brush's diameter in pixels.
    diameter: Option<f32>,
    dab_ratio: f32,
    /// Degrees, converted from the radians the file states.
    angle: f32,
    spacing: Option<f32>,
    hardness: Option<f32>,
    dropped: Vec<&'static str>,
}

impl TipSpec {
    fn parse(xml: &str) -> Result<Self, PresetError> {
        let mut reader = quick_xml::Reader::from_str(xml);
        let mut out = Self {
            file: None,
            scale: 1.0,
            diameter: None,
            dab_ratio: 1.0,
            angle: 0.0,
            spacing: None,
            hardness: None,
            dropped: Vec::new(),
        };
        let mut seen = false;

        loop {
            let event = reader.read_event().map_err(|e| {
                PresetError::Malformed(
                    None,
                    format!("its brush_definition is not readable XML ({e})"),
                )
            })?;
            let e = match &event {
                Event::Start(e) => e.clone(),
                Event::Empty(e) => e.clone(),
                Event::Eof => break,
                _ => continue,
            };
            let attrs = attributes(&e)?;
            match e.local_name().as_ref() {
                b"Brush" => {
                    seen = true;
                    out.file = attrs.get("filename").filter(|f| !f.is_empty()).cloned();
                    out.scale = attrs.number("scale").unwrap_or(1.0).clamp(0.01, 100.0);
                    // Krita's own spacing slider runs to 10, and one shipped
                    // preset — Raghukamath's "Dots" — sits at 5.12. A ceiling
                    // of 4 pulled its dots 20% closer together than its author
                    // spaced them, which for a brush that *is* its spacing is
                    // the whole brush.
                    out.spacing = attrs.number("spacing").map(|s| s.clamp(0.01, 10.0));
                    // Radians. Krita's rotation runs the same way round as
                    // Umber's, so only the units differ.
                    out.angle = attrs
                        .number("angle")
                        .unwrap_or(0.0)
                        .to_degrees()
                        .rem_euclid(360.0);
                    if attrs.number("randomness").is_some_and(|r| r > 0.0) {
                        out.dropped.push("brush-tip randomness");
                    }
                    if attrs.number("density").is_some_and(|d| d < 1.0) {
                        out.dropped.push("brush-tip density");
                    }
                }
                b"MaskGenerator" => {
                    out.diameter = attrs.number("diameter").map(|d| d.max(1.0));
                    // Krita's ratio scales the *short* axis: 1.0 is a circle
                    // and 0.17 is a chisel six times as long as it is wide.
                    // `Brush::dab_ratio` is long over short, so it inverts.
                    let ratio = attrs.number("ratio").unwrap_or(1.0).clamp(0.05, 20.0);
                    out.dab_ratio = if ratio < 1.0 { 1.0 / ratio } else { ratio };
                    if ratio > 1.0 {
                        // The long axis is now the *other* one, so the diameter
                        // is the short side and the dab is a quarter turn on.
                        out.diameter = out.diameter.map(|d| d * ratio);
                        out.angle = (out.angle + 90.0).rem_euclid(360.0);
                    }
                    // `fade` is **hardness**, not softness, and this read the
                    // wrong way round until it was checked against Krita's own
                    // rendered previews — see the "Fade is hardness" section of
                    // the module docs, which lists the seven presets that
                    // settle it. `hfade` is the fraction of the radius that
                    // stays fully opaque, which is exactly what `dab.wgsl`'s
                    // `smoothstep(hardness, 1.0, d)` means by hardness.
                    //
                    // `id="soft"` is a different generator whose shape is in
                    // `softness_curve`; `hfade` is a leftover field there and
                    // says nothing, so the curve wins where there is one.
                    out.hardness = attrs
                        .get("softness_curve")
                        .and_then(|c| hardness_from_curve(c))
                        .or_else(|| attrs.number("hfade").map(|f| f.clamp(0.0, 1.0)));
                    if attrs.number("spikes").is_some_and(|s| s > 2.5) {
                        out.dropped.push("star-shaped brushes");
                    }
                    if attrs.get("type").is_some_and(|t| t == "rect") {
                        out.dropped.push("square brush shapes");
                    }
                }
                _ => {}
            }
        }

        if !seen {
            return Err(malformed("its brush_definition has no <Brush> in it"));
        }
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// Tips
// ---------------------------------------------------------------------------

struct DecodedTip {
    mask: TipMask,
    dropped: Vec<&'static str>,
}

/// Turn a predefined brush's file into a coverage mask.
///
/// Krita stores the same three formats GIMP does, so this sniffs the contents
/// rather than trusting the extension: `brush_definition` routinely says
/// `type="gbr_brush"` for a file whose name ends `.gih`, and the Revoy bundle
/// is full of both.
fn decode_tip(name: &str, raw: &[u8]) -> Result<DecodedTip, PresetError> {
    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

    if raw.starts_with(PNG_MAGIC) {
        return Ok(DecodedTip {
            mask: tip_from_png(raw)?,
            dropped: Vec::new(),
        });
    }
    // A `.gih` starts with a name line and a count line; a `.gbr` starts with
    // four big-endian words. The two are told apart by whether the first bytes
    // are a plausible header size, which for text they never are.
    if let Ok(pipe) = gih::from_gih(raw) {
        let mut dropped = Vec::new();
        if pipe.animated {
            dropped.push(gih::ANIMATION);
        }
        if pipe.cells.iter().any(|c| c.coloured) {
            dropped.push(gbr::COLOURED);
        }
        let cell = pipe
            .cells
            .into_iter()
            .next()
            .ok_or_else(|| malformed("an empty brush pipe"))?;
        return Ok(DecodedTip {
            mask: cell.tip,
            dropped,
        });
    }
    match gbr::from_gbr(raw) {
        Ok(brush) => Ok(DecodedTip {
            dropped: if brush.coloured {
                vec![gbr::COLOURED]
            } else {
                Vec::new()
            },
            mask: brush.tip,
        }),
        Err(_) => Err(PresetError::Malformed(
            None,
            format!("its tip `{name}` is in a format Umber cannot read"),
        )),
    }
}

/// Decode a Krita PNG brush.
///
/// **Inverted relative to a `.gbr`.** Krita paints where the picture is *dark*,
/// so coverage is `255 - luminance` — the opposite of [`TipMask`]'s convention
/// and of GIMP's. A file with real transparency is a colour stamp instead, and
/// its silhouette is its alpha; those are the only two rules Krita itself has,
/// so neither is a guess.
fn tip_from_png(bytes: &[u8]) -> Result<TipMask, PresetError> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|e| PresetError::Malformed(None, format!("a brush tip would not open ({e})")))?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| malformed("a brush tip is too large to decode"))?;
    let mut buf = vec![0u8; size];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| PresetError::Malformed(None, format!("a brush tip would not decode ({e})")))?;

    let texels = info.width as usize * info.height as usize;
    let luminance = |px: &[u8]| {
        // Rec. 601, which is what Qt's `qGray` uses and therefore what Krita
        // saw when the brush was made.
        ((px[0] as u32 * 11 + px[1] as u32 * 16 + px[2] as u32 * 5) / 32) as u8
    };
    let coverage: Vec<u8> = match info.color_type {
        png::ColorType::Grayscale => buf[..texels].iter().map(|g| 255 - g).collect(),
        png::ColorType::GrayscaleAlpha => {
            let px: Vec<&[u8]> = buf[..texels * 2].chunks_exact(2).collect();
            if px.iter().any(|p| p[1] != 255) {
                px.iter().map(|p| p[1]).collect()
            } else {
                px.iter().map(|p| 255 - p[0]).collect()
            }
        }
        png::ColorType::Rgb => buf[..texels * 3]
            .chunks_exact(3)
            .map(|px| 255 - luminance(px))
            .collect(),
        png::ColorType::Rgba => {
            let px: Vec<&[u8]> = buf[..texels * 4].chunks_exact(4).collect();
            if px.iter().any(|p| p[3] != 255) {
                px.iter().map(|p| p[3]).collect()
            } else {
                px.iter().map(|p| 255 - luminance(p)).collect()
            }
        }
        other => {
            return Err(PresetError::Malformed(
                None,
                format!("a brush tip in {other:?} is not something Krita writes"),
            ));
        }
    };
    // Not padded out to a square: the dab carries the mask's proportions in its
    // own scale, so a tall Krita tip stays tall without spending a texture on
    // the margin. See `TipMask::aspect`.
    TipMask::new(info.width, info.height, coverage)
}

// ---------------------------------------------------------------------------
// Odds and ends
// ---------------------------------------------------------------------------

/// One element's attributes, decoded once.
///
/// A local copy rather than `docimport::container::Attrs`, which is private to
/// the document readers — sharing it would mean making the document import's
/// plumbing public to the brush import, and the two have nothing else in
/// common.
struct Attrs(BTreeMap<String, String>);

impl Attrs {
    fn get(&self, key: &str) -> Option<&String> {
        self.0.get(key)
    }

    fn number(&self, key: &str) -> Option<f32> {
        self.0.get(key)?.trim().parse().ok()
    }
}

fn attributes(e: &quick_xml::events::BytesStart<'_>) -> Result<Attrs, PresetError> {
    let mut out = BTreeMap::new();
    for attr in e.attributes() {
        let attr = attr.map_err(|e| {
            PresetError::Malformed(None, format!("a malformed XML attribute ({e})"))
        })?;
        let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|e| {
                PresetError::Malformed(None, format!("an unreadable XML attribute ({e})"))
            })?;
        out.insert(key, value.into_owned());
    }
    Ok(Attrs(out))
}

/// Decode base64, ignoring whitespace and tolerating missing padding.
///
/// Written here rather than taken from a crate. It is thirty lines, it is used
/// by exactly one caller, and the alternative is a dependency in the *engine*
/// crate — which `CLAUDE.md` asks to be justified by what it saves. This saves
/// nothing.
fn base64(text: &str) -> Option<Vec<u8>> {
    fn sextet(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }

    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for &c in text.as_bytes() {
        if c.is_ascii_whitespace() || c == b'=' {
            continue;
        }
        accumulator = (accumulator << 6) | sextet(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    (!out.is_empty()).then_some(out)
}

fn malformed(message: &str) -> PresetError {
    PresetError::Malformed(None, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap settings XML in a PNG, which is what a `.kpp` is. Built by hand
    /// rather than vendored, the same discipline as the `.gbr` fixtures: the
    /// test then pins the byte layout instead of describing whatever file
    /// happened to be on disk.
    fn kpp(xml: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_ztxt_chunk("preset".to_string(), xml.to_string())
            .expect("text chunk");
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[128]).expect("pixel");
        drop(writer);
        out
    }

    /// The same, with the settings in a chunk of the caller's choosing.
    fn kpp_with(chunk: &str, xml: &str) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        match chunk {
            "tEXt" => encoder.add_text_chunk("preset".to_string(), xml.to_string()),
            "iTXt" => encoder.add_itxt_chunk("preset".to_string(), xml.to_string()),
            _ => encoder.add_ztxt_chunk("preset".to_string(), xml.to_string()),
        }
        .expect("text chunk");
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[128]).expect("pixel");
        drop(writer);
        out
    }

    fn param(name: &str, value: &str) -> String {
        format!("<param name=\"{name}\" type=\"string\"><![CDATA[{value}]]></param>")
    }

    /// The other spelling: `type="internal"` writes the value as element text
    /// rather than wrapping it in CDATA, and every `Pressure<Name>` flag in a
    /// real preset is written this way.
    fn internal(name: &str, value: impl std::fmt::Display) -> String {
        format!("<param type=\"internal\" name=\"{name}\">{value}</param>")
    }

    /// Revoy's "Basic Oval Brush", trimmed to what this importer reads.
    fn oval() -> String {
        format!(
            "<Preset embedded_resources=\"0\" name=\"Basic Oval\" paintopid=\"paintbrush\">{}{}{}{}{}{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.02\" angle=\"5.98648\" \
                 useAutoSpacing=\"0\" density=\"1\" randomness=\"0\"> \
                 <MaskGenerator diameter=\"40\" type=\"circle\" hfade=\"0.89\" ratio=\"0.65\" \
                 id=\"default\" vfade=\"0.89\" spikes=\"2\"/> </Brush>"
            ),
            param("OpacityValue", "1"),
            param("FlowValue", "1"),
            param("OpacitySensor", "<params id=\"pressure\"/>"),
            param("OpacitycommonCurve", "0,0;1,1;"),
            "<param type=\"internal\" name=\"PressureOpacity\">true</param>",
            "<param type=\"internal\" name=\"OpacityUseCurve\">true</param>",
            "<param type=\"internal\" name=\"PressureSize\">false</param>",
            "<param type=\"internal\" name=\"EraserMode\">false</param>",
        )
    }

    #[test]
    fn a_generated_krita_brush_lands_on_umbers_own_ellipse() {
        let preset = from_kpp(&kpp(&oval())).expect("decode");
        assert_eq!(preset.name, "Basic Oval");
        assert_eq!(preset.brush.size, 40.0);
        assert_eq!(preset.brush.spacing, 0.02);
        // Krita's ratio scales the short axis, so it is the reciprocal of
        // Umber's. Reading it straight through would make a chisel a circle.
        assert!((preset.brush.dab_ratio - 1.0 / 0.65).abs() < 1e-4);
        // `hfade` is the opaque fraction of the radius, which is exactly what
        // `dab.wgsl` means by hardness. Revoy's oval brush draws a hard-edged
        // stroke in Krita's own preview of it; `1 - hfade` made it a cloud.
        assert!((preset.brush.hardness - 0.89).abs() < 1e-5);
        // Radians in the file, degrees in the brush.
        assert!(
            (preset.brush.dab_angle - 343.0).abs() < 0.5,
            "{}",
            preset.brush.dab_angle
        );
        assert!(preset.brush.pressure_opacity);
        assert!(!preset.brush.pressure_size);
        assert!(preset.tip.is_none());
        assert!(preset.dropped.is_empty());
    }

    /// The mistake this reader exists not to make. `PressureSize` is Krita's
    /// spelling of "the Size dynamic is switched on"; the sensor says what
    /// drives it. Reading the flag as "pressure drives size" turns every
    /// speed-driven brush into a pressure-driven one.
    #[test]
    fn a_dynamic_driven_by_something_else_is_not_read_as_pressure() {
        let with = |sensor: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"20\" type=\"circle\" ratio=\"1\" hfade=\"0\"/></Brush>"
                ),
                param("SizeSensor", &format!("<params id=\"{sensor}\"/>")),
                param("SizecommonCurve", "0,0;1,1;"),
                "<param type=\"internal\" name=\"PressureSize\">true</param>",
                "<param type=\"internal\" name=\"SizeUseCurve\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode")
        };

        let by_pressure = with("pressure");
        assert!(by_pressure.brush.pressure_size);
        assert!(by_pressure.brush.min_size_ratio < 0.01);

        let by_speed = with("speed");
        assert!(!by_speed.brush.pressure_size);
        assert!(
            by_speed
                .dropped
                .contains(&"dynamics driven by speed, tilt or stroke position"),
            "{:?}",
            by_speed.dropped
        );
    }

    /// A pressure curve has to come back through Umber's own `radius_at`.
    #[test]
    fn the_size_curve_reproduces_kritas_at_every_sample() {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"100\" type=\"circle\" ratio=\"1\" hfade=\"0\"/></Brush>"
            ),
            param("SizeSensor", "<params id=\"pressure\"/>"),
            param("SizecommonCurve", "0,0.2;1,0.8;"),
            "<param type=\"internal\" name=\"PressureSize\">true</param>",
            "<param type=\"internal\" name=\"SizeUseCurve\">true</param>",
        );
        let brush = from_kpp(&kpp(&xml)).expect("decode").brush;
        // Krita's diameter times the curve's peak.
        assert!((brush.size - 80.0).abs() < 0.01, "size {}", brush.size);
        for i in 0..=4 {
            let p = i as f32 / 4.0;
            let expected = 100.0 * (0.2 + 0.6 * p) / 2.0;
            assert!(
                (brush.radius_at(p) - expected).abs() < 0.1,
                "at {p}: expected {expected}, got {}",
                brush.radius_at(p)
            );
        }
    }

    /// A predefined tip that nothing can supply must not arrive as a silent
    /// round brush — that is the failure the whole reader is written against.
    #[test]
    fn a_missing_predefined_tip_is_named_rather_than_painted_round_in_silence() {
        let xml = format!(
            "<Preset name=\"Stamp\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"png_brush\" filename=\"bristle.png\" spacing=\"0.05\" \
                 angle=\"0\" scale=\"0.5\"/>"
            ),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert_eq!(preset.missing_tip.as_deref(), Some("bristle.png"));
        assert!(preset.tip.is_none());
        // The spacing it did carry still arrives.
        assert_eq!(preset.brush.spacing, 0.05);
    }

    /// Krita paints where the picture is dark. Reading a PNG brush the `.gbr`
    /// way gives a solid square with a hole in it.
    #[test]
    fn an_embedded_png_tip_is_read_the_way_krita_reads_it() {
        // A 2x1 greyscale tip: black then white.
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 2, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[0, 255]).expect("data");
        }
        let xml = format!(
            "<Preset embedded_resources=\"1\" name=\"Stamp\" paintopid=\"paintbrush\">\
             <resources><resource name=\"t\" filename=\"t.png\" type=\"brushes\">\
             <![CDATA[{}]]></resource></resources>{}</Preset>",
            encode_base64(&png),
            param(
                "brush_definition",
                "<Brush type=\"png_brush\" filename=\"t.png\" spacing=\"0.05\" \
                 angle=\"0\" scale=\"2\"/>"
            ),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(preset.missing_tip.is_none());
        let tip = preset.tip.expect("a mask came with it");
        // Kept at its own proportions rather than padded out to a square — the
        // dab carries the aspect — and black is full paint.
        assert_eq!((tip.width(), tip.height()), (2, 1));
        assert_eq!(tip.at(0, 0), 255, "black should be full coverage");
        assert_eq!(tip.at(1, 0), 0, "white should be none");
        // The size follows the bitmap through its scale.
        assert_eq!(preset.brush.size, 4.0);
    }

    /// A caller with the file elsewhere — a `.bundle`'s `brushes/` — must be
    /// able to supply it.
    #[test]
    fn a_tip_can_be_resolved_from_outside_the_file() {
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[0]).expect("data");
        }
        let xml = format!(
            "<Preset name=\"Stamp\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"png_brush\" filename=\"outside.png\" spacing=\"0.05\" \
                 angle=\"0\" scale=\"1\"/>"
            ),
        );
        let preset = from_kpp_in(&kpp(&xml), &|name| {
            (name == "outside.png").then(|| png.clone())
        })
        .expect("decode");
        assert!(preset.missing_tip.is_none());
        assert_eq!(preset.tip.expect("mask").at(0, 0), 255);
    }

    /// A different painting engine is refused by name. A round dab wearing
    /// `deformbrush`'s name would be invention, and the standing rule is that
    /// subtly wrong pixels are worse than a refusal.
    #[test]
    fn another_painting_engine_is_refused_and_says_which() {
        let xml = "<Preset name=\"Warp\" paintopid=\"deformbrush\">\
                   <param type=\"internal\" name=\"x\">1</param></Preset>";
        let err = from_kpp(&kpp(xml)).expect_err("refused");
        assert!(err.to_string().contains("deformbrush"), "{err}");
    }

    #[test]
    fn a_smudging_preset_arrives_as_one() {
        let xml = format!(
            "<Preset name=\"Blender\" paintopid=\"colorsmudge\">{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.09\" angle=\"0\">\
                 <MaskGenerator diameter=\"72\" type=\"circle\" ratio=\"0.5\" hfade=\"1\"/></Brush>"
            ),
            param("SmudgeRateValue", "0.8"),
            param("SmudgeRadiusValue", "1.5"),
        );
        let preset = from_kpp(&kpp(&xml)).expect("decode");
        assert!(preset.brush.smudges());
        assert!((preset.brush.smudge - 0.8).abs() < 1e-5);
        assert!((preset.brush.smudge_radius - 1.5).abs() < 1e-5);
        assert_eq!(preset.brush.dab_ratio, 2.0);
    }

    #[test]
    fn an_eraser_arrives_as_an_eraser() {
        let xml = format!(
            "<Preset name=\"Eraser\" paintopid=\"paintbrush\">{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                 <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"0.5\"/></Brush>"
            ),
            "<param type=\"internal\" name=\"EraserMode\">true</param>",
        );
        assert_eq!(from_kpp(&kpp(&xml)).unwrap().brush.mode, BrushMode::Erase);
    }

    #[test]
    fn features_umber_cannot_render_are_named() {
        let xml = format!(
            "<Preset name=\"Rich\" paintopid=\"paintbrush\">{}{}{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\" randomness=\"0.4\" \
                 density=\"0.5\"><MaskGenerator diameter=\"30\" type=\"rect\" ratio=\"1\" \
                 hfade=\"0.5\" spikes=\"5\"/></Brush>"
            ),
            "<param type=\"internal\" name=\"MaskingBrush/Enabled\">true</param>",
            "<param type=\"internal\" name=\"Texture/Enabled\">true</param>",
        );
        let dropped = from_kpp(&kpp(&xml)).expect("decode").dropped;
        for expected in [
            "brush-tip randomness",
            "brush-tip density",
            "star-shaped brushes",
            "square brush shapes",
            "masking brushes",
            "paper texture",
        ] {
            assert!(
                dropped.contains(&expected),
                "{expected} missing: {dropped:?}"
            );
        }
    }

    #[test]
    fn a_rake_and_a_fringe_are_told_apart() {
        let with = |sensor: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"0.2\" hfade=\"0\"/></Brush>"
                ),
                param("RotationSensor", &format!("<params id=\"{sensor}\"/>")),
                "<param type=\"internal\" name=\"PressureRotation\">true</param>",
            );
            from_kpp(&kpp(&xml)).expect("decode").brush
        };
        assert!(with("drawingangle").dab_angle_follows_stroke);
        assert_eq!(with("fuzzy").dab_angle_jitter, 360.0);
        assert!(!with("fuzzy").dab_angle_follows_stroke);
    }

    /// All three of PNG's text chunks turn up in one real pack, and a reader
    /// that knows only `zTXt` silently rejects a quarter of it. This is not a
    /// hypothetical: eleven of Revoy's forty-six presets are `iTXt`, and they
    /// were the eleven that failed before this was fixed.
    #[test]
    fn the_settings_are_found_in_any_of_pngs_three_text_chunks() {
        for chunk in ["zTXt", "tEXt", "iTXt"] {
            let preset =
                from_kpp(&kpp_with(chunk, &oval())).unwrap_or_else(|e| panic!("{chunk}: {e}"));
            assert_eq!(preset.name, "Basic Oval", "{chunk}");
            assert_eq!(preset.brush.size, 40.0, "{chunk}");
        }
    }

    /// `tEXt` and `zTXt` are defined as Latin-1 and Krita writes UTF-8 into
    /// them anyway, so both spellings of an accented name have to come out
    /// right. Reading the UTF-8 one as Latin-1 gives "AÃ©rographe" in the
    /// picker; reading the Latin-1 one as UTF-8 gives a replacement character.
    #[test]
    fn an_accented_name_survives_whichever_way_it_was_written() {
        let xml = oval().replace("Basic Oval", "Aérographe fin");

        // What Krita does: UTF-8 bytes in a chunk that claims Latin-1. `png`'s
        // encoder writes one byte per char, so the fixture has to hand it the
        // mojibake spelling for the bytes to come out right.
        let smuggled: String = xml.bytes().map(|b| b as char).collect();
        for chunk in ["zTXt", "tEXt"] {
            let preset =
                from_kpp(&kpp_with(chunk, &smuggled)).unwrap_or_else(|e| panic!("{chunk}: {e}"));
            assert_eq!(preset.name, "Aérographe fin", "{chunk}");
        }

        // A writer that really did mean Latin-1 must not be mangled either.
        for chunk in ["zTXt", "tEXt"] {
            let preset =
                from_kpp(&kpp_with(chunk, &xml)).unwrap_or_else(|e| panic!("{chunk}: {e}"));
            assert_eq!(preset.name, "Aérographe fin", "{chunk}");
        }

        // `iTXt` is UTF-8 by definition and must skip the recovery entirely.
        let preset = from_kpp(&kpp_with("iTXt", &xml)).expect("iTXt");
        assert_eq!(preset.name, "Aérographe fin");
    }

    #[test]
    fn nonsense_is_an_error_rather_than_a_panic() {
        assert!(from_kpp(b"not a png").is_err());
        // A PNG with no preset chunk is a picture, not a brush.
        let mut plain = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut plain, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[1]).expect("data");
        }
        assert!(from_kpp(&plain).is_err());
        assert!(from_kpp(&kpp("<Preset name=\"X\"/>")).is_err());
        assert!(from_kpp(&kpp("not xml at all <<<")).is_err());
        assert!(dropped_features(b"rubbish").is_empty());
    }

    /// A `brush_definition` around one mask generator, with everything the
    /// reader needs and nothing else.
    fn with_generator(generator: &str) -> Vec<u8> {
        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                &format!(
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\" \
                     density=\"1\" randomness=\"0\">{generator}</Brush>"
                )
            ),
        );
        kpp(&xml)
    }

    /// The polarity, pinned against the numbers the shipped packs actually
    /// carry. Every value below is copied out of a real preset, and every one
    /// of them was imported inside out until this was checked against Krita's
    /// own preview images — see the module docs for the table.
    #[test]
    fn fade_is_hardness_and_not_softness() {
        let hardness = |generator: &str| {
            from_kpp(&with_generator(generator))
                .expect("decode")
                .brush
                .hardness
        };

        // GDQuest "Ink Brush" and Raghukamath "Inkbrush": both draw a crisp
        // black line, and both used to import at hardness 0.0 — a cloud.
        assert!(
            (hardness(
                "<MaskGenerator id=\"default\" type=\"circle\" diameter=\"30\" \
                 ratio=\"1\" hfade=\"1\" vfade=\"1\" spikes=\"2\"/>"
            ) - 1.0)
                .abs()
                < 1e-5
        );
        // Deevad "Eraser Kneaded Soft": a wide feathered fade, and the name
        // says so. It used to import at hardness 1.0.
        assert!(
            hardness(
                "<MaskGenerator id=\"gauss\" type=\"circle\" diameter=\"250\" \
                 ratio=\"1\" hfade=\"0\" vfade=\"0\" spikes=\"2\"/>"
            ) < 1e-5
        );
        // Raghukamath "Basic Render": plainly soft in its preview, and between
        // the two extremes rather than at one of them.
        let render = hardness(
            "<MaskGenerator id=\"default\" type=\"circle\" diameter=\"95.81\" \
             ratio=\"1\" hfade=\"0.67\" vfade=\"0.67\" spikes=\"2\"/>",
        );
        assert!((render - 0.67).abs() < 1e-5, "{render}");
    }

    /// `id="soft"` is a different generator: its shape is the `softness_curve`
    /// and its `hfade` is a leftover the editor wrote and never reads. Reading
    /// the field would make GDQuest's airbrush — whose curve never exceeds 0.4
    /// even at the centre — a hard disc.
    #[test]
    fn a_softness_curve_beats_the_fade_field_beside_it() {
        // GDQuest "Airbrush", verbatim.
        let airbrush = from_kpp(&with_generator(
            "<MaskGenerator id=\"soft\" type=\"circle\" diameter=\"440\" ratio=\"1\" \
             hfade=\"0\" vfade=\"0\" spikes=\"2\" \
             softness_curve=\"0,0.39911;0.429719,0.118523;1,0;\"/>",
        ))
        .expect("decode");
        assert!(
            airbrush.brush.hardness < 1e-5,
            "{}",
            airbrush.brush.hardness
        );

        // Raghukamath "Basic": full coverage in the middle falling to nothing
        // at the rim, with `hfade="1"` sitting beside it saying "hard".
        let basic = from_kpp(&with_generator(
            "<MaskGenerator id=\"soft\" type=\"circle\" diameter=\"20\" ratio=\"1\" \
             hfade=\"1\" vfade=\"1\" spikes=\"2\" \
             softness_curve=\"0,1;0.562249,0.721362;1,0;\"/>",
        ))
        .expect("decode");
        assert!(
            basic.brush.hardness > 0.2 && basic.brush.hardness < 0.6,
            "{}",
            basic.brush.hardness
        );
    }

    /// `ScatterValue` sits at Krita's default of 1 in a preset that never
    /// scatters, so the enable flag is the whole of the answer. Reading the
    /// value alone gave GDQuest's "Ink Brush" a five-radius spray.
    #[test]
    fn scatter_needs_kritas_own_switch_as_well_as_its_value() {
        let with = |on: bool, value: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                     </Brush>"
                ),
                param("ScatterValue", value),
                param("ScatterSensor", "<params id=\"pressure\"/>"),
                internal("PressureScatter", on),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush
        };
        // GDQuest "Ink Brush": switched off, value left at 5.
        assert_eq!(with(false, "5").scatter, 0.0);
        // Raghukamath "Dots": switched on, and it really does scatter.
        assert!((with(true, "0.1").scatter - 0.1).abs() < 1e-5);
    }

    /// Krita has spelled the airbrush option two ways and both are in the
    /// fetched packs. Knowing one silently imports an airbrush as an ordinary
    /// distance-driven brush.
    #[test]
    fn both_of_kritas_airbrush_spellings_are_read() {
        let with = |prefix: &str, rate: &str| {
            let switch = internal(&format!("{prefix}/isAirbrushing"), true);
            let speed = param(&format!("{prefix}/rate"), rate);
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{switch}{speed}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"30\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                     </Brush>"
                ),
            );
            from_kpp(&kpp(&xml)).expect("decode").brush.dabs_per_second
        };
        assert_eq!(with("PaintOpSettings", "50"), 50.0);
        // GDQuest "Airbrush" asks for 1000 a second; the ceiling is Umber's.
        assert_eq!(with("AirbrushOption", "1000"), 300.0);
    }

    /// Krita's Sharpness thresholds the mask into a hard, aliased edge, which
    /// is the whole of a pixel-art brush and something Umber cannot do. It has
    /// to be *named*, or such a preset ships as a soft blob under its author's
    /// name.
    #[test]
    fn edge_sharpening_is_named_rather_than_quietly_dropped() {
        let with = |on: bool, value: &str| {
            let xml = format!(
                "<Preset name=\"T\" paintopid=\"paintbrush\">{}{}{}</Preset>",
                param(
                    "brush_definition",
                    "<Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
                     <MaskGenerator diameter=\"1\" type=\"circle\" ratio=\"1\" hfade=\"1\" \
                     id=\"gauss\"/></Brush>"
                ),
                internal("PressureSharpness", on),
                param("SharpnessValue", value),
            );
            from_kpp(&kpp(&xml)).expect("decode").dropped
        };
        // GDQuest "PixelArt OnePixel".
        assert!(with(true, "1").contains(&"edge sharpening"));
        // Switched on with nothing behind it changes no pixel.
        assert!(!with(true, "0").contains(&"edge sharpening"));
        assert!(!with(false, "1").contains(&"edge sharpening"));
    }

    /// Raghukamath's "Dots" spaces its dabs 5.12 diameters apart, and the
    /// spacing *is* the brush. A ceiling of 4 pulled them 20% closer.
    #[test]
    fn kritas_whole_spacing_range_survives() {
        let brush = from_kpp(&with_generator(
            "<MaskGenerator id=\"gauss\" type=\"circle\" diameter=\"26.89\" \
             ratio=\"1\" hfade=\"1\" vfade=\"1\" spikes=\"2\"/>",
        ))
        .expect("decode");
        assert_eq!(brush.brush.spacing, 0.1);

        let xml = format!(
            "<Preset name=\"T\" paintopid=\"paintbrush\">{}</Preset>",
            param(
                "brush_definition",
                "<Brush type=\"auto_brush\" spacing=\"5.12\" angle=\"0\">\
                 <MaskGenerator diameter=\"26.89\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
                 </Brush>"
            ),
        );
        assert_eq!(from_kpp(&kpp(&xml)).expect("decode").brush.spacing, 5.12);
    }

    #[test]
    fn base64_round_trips_and_refuses_rubbish() {
        for original in [
            &b""[..],
            b"a",
            b"ab",
            b"abc",
            b"abcd",
            &[0u8, 255, 128, 3][..],
        ] {
            let encoded = encode_base64(original);
            match base64(&encoded) {
                Some(back) => assert_eq!(back, original),
                None => assert!(original.is_empty()),
            }
        }
        // Whitespace is how a long resource is wrapped in the file.
        assert_eq!(base64("aGVs\n bG8=").unwrap(), b"hello");
        assert!(base64("not base64 !!").is_none());
    }

    fn encode_base64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let mut word = 0u32;
            for (i, b) in chunk.iter().enumerate() {
                word |= (*b as u32) << (16 - 8 * i);
            }
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[((word >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }
}
