//! Krita resource bundles (`.bundle`).
//!
//! A bundle is a ZIP holding a whole brush pack:
//!
//! ```text
//! mimetype                      application/x-krita-resourcebundle
//! meta.xml                      author, title, licence
//! preview.png
//! paintoppresets/*.kpp          the brushes
//! brushes/*.png|.gbr|.gih       the tips they name
//! patterns/*                    paper textures, which Umber has no use for yet
//! ```
//!
//! Two things make it worth a reader of its own rather than "unzip it first".
//!
//! - **The tips are in a sibling directory.** A `.kpp` inside a bundle names
//!   `bristle.png` and does not embed it, so pulling one preset out on its own
//!   gives a brush that paints round. Reading the container is what lets
//!   [`super::kpp::from_kpp_in`] find the file.
//! - **`meta.xml` is where the licence and the author live**, and Umber's whole
//!   attribution story runs on [`Credit`]. The Revoy bundle states
//!   `<meta:license>CC-0</meta:license>` and `<dc:author>David Revoy
//!   (Deevad)</dc:author>` inside the download, which is exactly what
//!   `docs/brush-sources.md` asks a pack to do before it can ship.
//!
//! # What is dropped
//!
//! Whatever the presets inside drop, collected and de-duplicated, plus:
//!
//! - **Patterns.** A bundle's `patterns/` are paper textures for Krita's
//!   texture option, and the dab pass has no grain channel — the same reason
//!   `docs/brush-sources.md` is not fetching texture packs yet.
//! - **Tags.** A bundle carries Krita's own grouping; Umber files a brush by
//!   the mark it makes (`crate::style`), deliberately, because a library sorted
//!   by pack puts the pencils in six places.

use std::collections::BTreeMap;
use std::io::{Cursor, Read};

use quick_xml::events::Event;

use crate::preset::{Credit, PresetError};

use super::kpp::{self, KppPreset};

/// Largest entry this will inflate. Bundle tips run to a megabyte or so; a
/// 20 KB entry claiming to expand to 40 GB is the classic ZIP attack, and
/// `docimport::container` guards against it the same way.
const MAX_ENTRY_BYTES: u64 = 64 << 20;

/// Refuse a bundle with more presets than any real pack has. Revoy's is 46.
const MAX_PRESETS: usize = 4096;

/// Everything a bundle held that Umber can use.
#[derive(Debug)]
pub struct BundleContents {
    /// From `meta.xml`, ready to hang on every preset in the bundle.
    pub credit: Option<Credit>,
    /// The bundle's own title, used to name the collection when nothing else
    /// does.
    pub title: String,
    pub brushes: Vec<KppPreset>,
    /// Presets that could not be read at all, with the reason. A bundle of
    /// forty brushes must not be lost to one written by a paint engine Umber
    /// does not have.
    pub refused: Vec<String>,
    pub dropped: Vec<&'static str>,
}

/// Read a Krita resource bundle.
pub fn from_bundle(bytes: &[u8]) -> Result<BundleContents, PresetError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| PresetError::Malformed(None, format!("it is not a readable archive ({e})")))?;

    // The extension is only a hint. A `.bundle` that is really a `.zip` of
    // loose files should say so clearly rather than fail on a missing
    // `paintoppresets/`.
    if let Some(mimetype) = entry(&mut zip, "mimetype")? {
        let found = String::from_utf8_lossy(&mimetype).trim().to_string();
        if found != "application/x-krita-resourcebundle" {
            return Err(PresetError::Malformed(
                None,
                format!("its mimetype is `{found}`, not a Krita resource bundle"),
            ));
        }
    }

    let meta = entry(&mut zip, "meta.xml")?;
    let meta = meta.as_deref().map(parse_meta).unwrap_or_default();

    // Every name first: the archive cannot be walked and read at the same time.
    let names: Vec<String> = zip.file_names().map(str::to_string).collect();
    let mut presets: Vec<String> = names
        .iter()
        .filter(|n| n.starts_with("paintoppresets/") && n.to_ascii_lowercase().ends_with(".kpp"))
        .cloned()
        .collect();
    // ZIP order is whatever the writer felt like; a stable list is what makes
    // an import reproducible and its ids predictable.
    presets.sort();
    presets.truncate(MAX_PRESETS);

    if presets.is_empty() {
        return Err(PresetError::Malformed(
            None,
            "it has no `paintoppresets/` in it, so it holds no brushes".to_string(),
        ));
    }

    // The tips, read once and shared: two brushes cut from one stamp is the
    // normal case, and re-inflating the file per preset would be the cost
    // `BrushPreset::tip` naming rather than carrying a mask exists to avoid.
    let mut tips: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for name in names.iter().filter(|n| n.starts_with("brushes/")) {
        if let Some(data) = entry(&mut zip, name)?
            && let Some(file) = name.rsplit('/').next()
        {
            tips.insert(file.to_string(), data);
        }
    }
    let has_patterns = names.iter().any(|n| n.starts_with("patterns/"));

    let mut out = BundleContents {
        credit: meta.credit(),
        title: meta.title,
        brushes: Vec::with_capacity(presets.len()),
        refused: Vec::new(),
        dropped: Vec::new(),
    };
    if has_patterns {
        out.dropped.push("paper textures");
    }

    for name in &presets {
        let Some(raw) = entry(&mut zip, name)? else {
            continue;
        };
        let file = name.rsplit('/').next().unwrap_or(name);
        match kpp::from_kpp_in(&raw, &|wanted| tips.get(wanted).cloned()) {
            Ok(mut preset) => {
                if preset.name.trim().is_empty() {
                    preset.name = super::display_name(file.trim_end_matches(".kpp"));
                }
                let mut losses = preset.dropped.clone();
                if preset.missing_tip.is_some() {
                    losses.push(kpp::MISSING_TIP);
                }
                for loss in losses {
                    if !out.dropped.contains(&loss) {
                        out.dropped.push(loss);
                    }
                }
                out.brushes.push(preset);
            }
            // One brush written by an engine Umber does not have must not take
            // the other forty-five with it.
            Err(e) => out.refused.push(format!("{file}: {e}")),
        }
    }

    if out.brushes.is_empty() {
        return Err(PresetError::Malformed(
            None,
            format!(
                "none of its {} brushes could be read — {}",
                presets.len(),
                out.refused
                    .first()
                    .map_or("no reason given", String::as_str)
            ),
        ));
    }
    Ok(out)
}

/// What reading this `.bundle` will throw away.
pub fn dropped_features(bytes: &[u8]) -> Vec<&'static str> {
    from_bundle(bytes).map(|b| b.dropped).unwrap_or_default()
}

/// Read one entry whole, or `None` if it is not there.
fn entry(
    zip: &mut zip::ZipArchive<Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<Vec<u8>>, PresetError> {
    let mut file = match zip.by_name(name) {
        Ok(f) => f,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => {
            return Err(PresetError::Malformed(
                None,
                format!("`{name}` could not be read ({e})"),
            ));
        }
    };
    if file.size() > MAX_ENTRY_BYTES {
        return Err(PresetError::Malformed(
            None,
            format!("`{name}` claims to be {} bytes", file.size()),
        ));
    }
    let mut out = Vec::with_capacity(file.size().min(1 << 20) as usize);
    // `take` as well as the declared size: the header is a claim and the
    // stream can be longer than it says.
    file.by_ref()
        .take(MAX_ENTRY_BYTES + 1)
        .read_to_end(&mut out)
        .map_err(|e| {
            PresetError::Malformed(None, format!("`{name}` could not be decompressed ({e})"))
        })?;
    if out.len() as u64 > MAX_ENTRY_BYTES {
        return Err(PresetError::Malformed(
            None,
            format!("`{name}` is larger than Umber will read"),
        ));
    }
    Ok(Some(out))
}

#[derive(Default)]
struct Meta {
    title: String,
    author: String,
    licence: String,
    website: String,
}

impl Meta {
    /// The bundle's own statement of who made it and on what terms.
    ///
    /// `None` when it says neither, because a [`Credit`] with two empty fields
    /// is worse than no credit at all — the browser would print a blank line
    /// where the attribution goes.
    fn credit(&self) -> Option<Credit> {
        if self.author.is_empty() && self.licence.is_empty() {
            return None;
        }
        Some(Credit {
            author: self.author.clone(),
            // Krita writes CC's own names rather than SPDX ids, and "CC-0" is
            // the one that shows up. Normalising it means the browser's
            // "does this need attribution?" test recognises it.
            licence: normalise_licence(&self.licence),
            source: self.website.clone(),
        })
    }
}

/// Krita's `meta.xml` — namespaced elements with the value as their text.
fn parse_meta(xml: &[u8]) -> Meta {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut out = Meta::default();
    let mut field: Option<&'static str> = None;
    let mut text = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                text.clear();
                field = match e.local_name().as_ref() {
                    b"title" => Some("title"),
                    b"author" | b"creator" | b"initial-creator" => Some("author"),
                    b"license" => Some("licence"),
                    b"website" => Some("website"),
                    _ => None,
                };
            }
            Ok(Event::Text(e)) => {
                if field.is_some() {
                    text.push_str(&String::from_utf8_lossy(&e));
                }
            }
            Ok(Event::End(_)) => {
                let value = text.trim().to_string();
                // First writer wins: `dc:author` comes before `cd:creator` and
                // is the one Krita's own dialog fills in.
                if !value.is_empty()
                    && let Some(name) = field.take()
                {
                    let slot = match name {
                        "title" => &mut out.title,
                        "author" => &mut out.author,
                        "licence" => &mut out.licence,
                        _ => &mut out.website,
                    };
                    if slot.is_empty() {
                        *slot = value;
                    }
                }
                field = None;
                text.clear();
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Krita writes Creative Commons' marketing names; Umber records SPDX ids.
///
/// Only the exact spellings bundles actually use are translated. Anything else
/// is passed through unchanged rather than guessed at, because the failure
/// direction of a wrong guess here is a licence breach.
fn normalise_licence(stated: &str) -> String {
    match stated.trim().to_ascii_uppercase().replace(' ', "").as_str() {
        "CC-0" | "CC0" | "CC01.0" | "CC-0-1.0" | "PUBLICDOMAIN" => "CC0-1.0".to_string(),
        "CC-BY" | "CCBY" | "CC-BY-4.0" | "CCBY4.0" => "CC-BY-4.0".to_string(),
        "CC-BY-SA" | "CC-BY-SA-4.0" => "CC-BY-SA-4.0".to_string(),
        _ => stated.trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    /// Build a bundle by hand. Same discipline as everywhere else here: no
    /// vendored archive, so the test pins the layout rather than describing
    /// whatever happened to be downloaded.
    fn bundle(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut out));
            for (name, data) in entries {
                writer
                    .start_file(*name, SimpleFileOptions::default())
                    .expect("entry");
                writer.write_all(data).expect("write");
            }
            writer.finish().expect("finish");
        }
        out
    }

    /// A `.kpp` naming a tip it does not carry — the case that only works
    /// because the container is read.
    fn preset_referring_to(tip: &str) -> Vec<u8> {
        let xml = format!(
            "<Preset name=\"Bristle\" paintopid=\"paintbrush\">\
             <param name=\"brush_definition\" type=\"string\"><![CDATA[\
             <Brush type=\"png_brush\" filename=\"{tip}\" spacing=\"0.05\" angle=\"0\" \
             scale=\"1\"/>]]></param></Preset>"
        );
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_ztxt_chunk("preset".to_string(), xml)
            .expect("chunk");
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[9]).expect("data");
        drop(writer);
        out
    }

    fn grey_png(value: u8) -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("header");
        writer.write_image_data(&[value]).expect("data");
        drop(writer);
        out
    }

    const META: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
        <meta:meta><meta:generator>Krita</meta:generator>\
        <dc:author>David Revoy (Deevad)</dc:author>\
        <dc:title>Deevad 25.01</dc:title>\
        <meta:license>CC-0</meta:license>\
        <meta:website>www.davidrevoy.com</meta:website></meta:meta>";

    /// The point of reading the container at all: a preset finds the tip that
    /// is beside it rather than arriving round.
    #[test]
    fn a_preset_finds_the_tip_in_the_bundles_brushes_directory() {
        let file = bundle(&[
            ("mimetype", b"application/x-krita-resourcebundle"),
            ("meta.xml", META.as_bytes()),
            ("brushes/bristle.png", &grey_png(0)),
            ("paintoppresets/a.kpp", &preset_referring_to("bristle.png")),
        ]);
        let contents = from_bundle(&file).expect("read");

        assert_eq!(contents.brushes.len(), 1);
        let brush = &contents.brushes[0];
        assert!(brush.missing_tip.is_none(), "the tip was not found");
        assert_eq!(brush.tip.as_ref().expect("mask").at(0, 0), 255);
        assert!(contents.refused.is_empty());
    }

    /// The licence and the author are inside the download, which is what
    /// `docs/brush-sources.md` asks a pack for before it can ship.
    #[test]
    fn the_bundles_own_metadata_becomes_the_credit() {
        let file = bundle(&[
            ("mimetype", b"application/x-krita-resourcebundle"),
            ("meta.xml", META.as_bytes()),
            ("paintoppresets/a.kpp", &preset_referring_to("nothing.png")),
        ]);
        let contents = from_bundle(&file).expect("read");
        let credit = contents.credit.expect("a credit");
        assert_eq!(credit.author, "David Revoy (Deevad)");
        // Krita writes Creative Commons' own name; Umber records SPDX.
        assert_eq!(credit.licence, "CC0-1.0");
        assert_eq!(credit.source, "www.davidrevoy.com");
        assert_eq!(contents.title, "Deevad 25.01");
    }

    /// A tip that is not in the bundle either is still reported, not silently
    /// swapped for a round dab.
    #[test]
    fn a_tip_the_bundle_does_not_hold_is_still_reported() {
        let file = bundle(&[
            ("mimetype", b"application/x-krita-resourcebundle"),
            ("paintoppresets/a.kpp", &preset_referring_to("absent.png")),
        ]);
        let contents = from_bundle(&file).expect("read");
        assert_eq!(
            contents.brushes[0].missing_tip.as_deref(),
            Some("absent.png")
        );
    }

    /// One brush written by a paint engine Umber does not have must not take
    /// the rest of the pack with it.
    #[test]
    fn a_refused_preset_does_not_lose_the_bundle() {
        let mut deform = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut deform, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_ztxt_chunk(
                    "preset".to_string(),
                    "<Preset name=\"Warp\" paintopid=\"deformbrush\">\
                     <param type=\"internal\" name=\"x\">1</param></Preset>"
                        .to_string(),
                )
                .expect("chunk");
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[1]).expect("data");
        }
        let file = bundle(&[
            ("mimetype", b"application/x-krita-resourcebundle"),
            ("brushes/bristle.png", &grey_png(0)),
            ("paintoppresets/a.kpp", &preset_referring_to("bristle.png")),
            ("paintoppresets/b.kpp", &deform),
        ]);
        let contents = from_bundle(&file).expect("read");
        assert_eq!(contents.brushes.len(), 1);
        assert_eq!(contents.refused.len(), 1);
        assert!(
            contents.refused[0].contains("deformbrush"),
            "{:?}",
            contents.refused
        );
    }

    #[test]
    fn patterns_are_reported_as_dropped() {
        let file = bundle(&[
            ("mimetype", b"application/x-krita-resourcebundle"),
            ("patterns/paper.png", &grey_png(200)),
            ("paintoppresets/a.kpp", &preset_referring_to("x.png")),
        ]);
        assert!(dropped_features(&file).contains(&"paper textures"));
    }

    #[test]
    fn a_zip_that_is_not_a_bundle_is_refused_by_name() {
        let file = bundle(&[("mimetype", b"application/zip"), ("a.txt", b"hello")]);
        let err = from_bundle(&file).expect_err("refused");
        assert!(err.to_string().contains("application/zip"), "{err}");

        let empty = bundle(&[("mimetype", b"application/x-krita-resourcebundle")]);
        assert!(from_bundle(&empty).is_err());
        assert!(from_bundle(b"not a zip").is_err());
        assert!(dropped_features(b"not a zip").is_empty());
    }

    #[test]
    fn licence_names_that_are_not_recognised_pass_through_unchanged() {
        // The failure direction of a wrong guess here is a licence breach.
        assert_eq!(normalise_licence("CC-0"), "CC0-1.0");
        assert_eq!(normalise_licence("CC-BY"), "CC-BY-4.0");
        assert_eq!(normalise_licence("Ask me first"), "Ask me first");
        assert_eq!(normalise_licence(""), "");
    }
}
