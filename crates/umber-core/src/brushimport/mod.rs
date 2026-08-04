//! Reading brushes that other applications wrote.
//!
//! Every importer lands on [`crate::Brush`] and, where the format has one, a
//! [`TipMask`]. Formats richer than that lose the parts Umber cannot render,
//! and each importer documents exactly what it drops — see [`mypaint`] for the
//! shape those notes take. An importer that quietly discarded half a brush
//! would be worse than one that refuses the file.
//!
//! | Extension | From | Reader |
//! |---|---|---|
//! | `.myb` | MyPaint | [`mypaint`] |
//! | `.gbr`, `.gpb` | GIMP, Krita | [`gbr`] |
//! | `.gih` | GIMP animated brush | [`gih`] |
//! | `.vbr` | GIMP parametric brush | [`vbr`] |
//! | `.kpp` | Krita paintop preset | [`kpp`] |
//! | `.bundle` | Krita resource bundle | [`bundle`] |
//! | `.abr` | Photoshop | [`abr`] |
//! | `.sut`, `.sutg` | Clip Studio Paint | [`clipstudio`] |
//! | `.ron` | an Umber library | [`crate::preset::parse_library`] |
//!
//! Three of those are **containers** — a `.gih` holds a sequence of stamps, a
//! `.bundle` holds a whole pack and a `.sutg` holds a group of sub-tools — so
//! [`read_file`] returns a `Vec`, and the caller has to report "twenty brushes
//! arrived" as readily as "one did".

pub mod abr;
pub mod bundle;
pub mod clipstudio;
pub mod csmaterial;
pub mod gbr;
pub mod gih;
pub mod kpp;
pub mod mypaint;
pub mod vbr;

use std::path::Path;

use crate::brush::Brush;
use crate::preset::{self, BrushPreset, PresetError};
use crate::tip::TipMask;

/// One brush out of a file, and the bitmap tip it stamps if it has one.
///
/// A pair rather than a field on [`BrushPreset`] because the preset stores the
/// tip's *name*, and the name is not known until the library has somewhere to
/// put the mask — see [`crate::preset::UserLibrary::save`].
#[derive(Debug)]
pub struct Imported {
    pub preset: BrushPreset,
    pub tip: Option<TipMask>,
    /// The paper the brush paints through, where the format carries its own
    /// picture and this reader could resolve it.
    ///
    /// Beside the tip and for the same reason — the preset stores a *name*, and
    /// the name is not known until the library has somewhere to put the tile.
    /// [`clipstudio`] and [`kpp`] produce one; the rest of the formats have no
    /// paper in them at all.
    pub paper: Option<TipMask>,
    /// What this *particular* brush lost on the way in.
    ///
    /// [`dropped_features`] answers the same question for a whole file, which
    /// is what the import notice needs — twenty files should not produce twenty
    /// notices. A container cannot use it: a `.bundle` of forty-six brushes
    /// reports the union of everything any of them dropped, and the library
    /// generator has to be able to keep the thirty that dropped nothing.
    pub dropped: Vec<&'static str>,
}

/// Read every brush in a file, whatever format it is in.
///
/// The format is chosen by extension. Sniffing the contents was tempting —
/// `.myb` is JSON and an Umber library is RON, so they are trivially
/// distinguishable — but a user who renames a file has told us something, and
/// guessing past that produces confusing failures much later.
pub fn read_file(path: &Path) -> Result<Vec<Imported>, PresetError> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    match extension.as_str() {
        "myb" => {
            let text = preset::read_to_string(path)?;
            let brush = mypaint::from_myb(&text).map_err(|e| e.at(path))?;
            let name = display_name(&stem);
            Ok(vec![Imported {
                preset: preset_for("mypaint", name, brush),
                tip: None,
                paper: None,
                dropped: mypaint::unsupported_features(&text).unwrap_or_default(),
            }])
        }
        // `.gpb` is a `.gbr` with a colour pattern stapled on; the same reader
        // handles both, and both keep their colour now — so neither has
        // anything left to report.
        "gbr" | "gpb" => {
            let bytes = preset::read_bytes(path)?;
            let brush = gbr::from_gbr(&bytes).map_err(|e| e.at(path))?;
            let name = embedded_name(&brush.name, &stem);
            let (parameters, tip) = gbr::to_brush(brush);
            Ok(vec![Imported {
                preset: preset_for("gbr", name, parameters),
                tip: Some(tip),
                paper: None,
                dropped: Vec::new(),
            }])
        }
        // A pipe is a container: every cell arrives as its own brush, because
        // Umber binds one tip per stroke and so cannot rotate through them.
        // See the module docs in `gih`.
        "gih" => {
            let bytes = preset::read_bytes(path)?;
            let pipe = gih::from_gih(&bytes).map_err(|e| e.at(path))?;
            let base = embedded_name(&pipe.name, &stem);
            let many = pipe.cells.len() > 1;
            let animated = pipe.animated;
            let angular = pipe.angular;
            Ok(pipe
                .cells
                .into_iter()
                .enumerate()
                .map(|(i, cell)| {
                    // Numbered only when there is more than one, so a one-cell
                    // pipe does not arrive as "Bark 1".
                    let name = if many {
                        format!("{base} {}", i + 1)
                    } else {
                        base.clone()
                    };
                    let mut dropped = Vec::new();
                    if angular {
                        dropped.push(gih::ANGULAR);
                    } else if animated {
                        dropped.push(gih::ANIMATION);
                    }
                    // A coloured cell no longer loses anything: the same `.gbr`
                    // reader carries the colour across, so the only thing a pipe
                    // drops is the sequence itself.
                    let (parameters, tip) = gbr::to_brush(cell);
                    Imported {
                        preset: preset_for("gih", name, parameters),
                        tip: Some(tip),
                        paper: None,
                        dropped,
                    }
                })
                .collect())
        }
        "vbr" => {
            let text = preset::read_to_string(path)?;
            let decoded = vbr::from_vbr(&text).map_err(|e| e.at(path))?;
            let name = embedded_name(&decoded.name, &stem);
            Ok(vec![Imported {
                preset: preset_for("vbr", name, decoded.brush),
                tip: None,
                paper: None,
                dropped: vbr::dropped_features(&text),
            }])
        }
        "kpp" => {
            let bytes = preset::read_bytes(path)?;
            let decoded = kpp::from_kpp_in(
                &bytes,
                &kpp::Sidecar {
                    brushes: &sibling_brushes(path),
                    patterns: &sibling_patterns(path),
                },
            )
            .map_err(|e| e.at(path))?;
            let name = embedded_name(&decoded.name, &stem);
            let mut dropped = decoded.dropped;
            if decoded.missing_tip.is_some() {
                dropped.push(kpp::MISSING_TIP);
            }
            Ok(vec![Imported {
                preset: preset_for("kpp", name, decoded.brush),
                tip: decoded.tip,
                paper: decoded.paper,
                dropped,
            }])
        }
        // The other container, and the one that carries its own attribution:
        // a bundle's `meta.xml` names the author and the licence, which is what
        // `BrushPreset::credit` is for.
        "bundle" => {
            let bytes = preset::read_bytes(path)?;
            let contents = bundle::from_bundle(&bytes).map_err(|e| e.at(path))?;
            Ok(contents
                .brushes
                .into_iter()
                .map(|decoded| {
                    let name = embedded_name(&decoded.name, &stem);
                    let mut preset = preset_for("krita", name, decoded.brush);
                    preset.credit = contents.credit.clone();
                    let mut dropped = decoded.dropped;
                    if decoded.missing_tip.is_some() {
                        dropped.push(kpp::MISSING_TIP);
                    }
                    Imported {
                        preset,
                        tip: decoded.tip,
                        paper: decoded.paper,
                        dropped,
                    }
                })
                .collect())
        }
        "abr" => {
            let bytes = preset::read_bytes(path)?;
            let file = abr::from_abr(&bytes).map_err(|e| e.at(path))?;
            let base = display_name(&stem);
            let many = file.brushes.len() > 1;
            // A `.abr` states its losses per file, not per brush: a computed
            // brush that was skipped is not *this* stamp's problem, and the
            // missing descriptor is every stamp's.
            let dropped = abr::dropped_features(&bytes);
            Ok(file
                .brushes
                .into_iter()
                .enumerate()
                .map(|(i, brush)| {
                    let name = match (&brush.name, many) {
                        (Some(name), _) => display_name(name),
                        (None, true) => format!("{base} {}", i + 1),
                        (None, false) => base.clone(),
                    };
                    let (parameters, tip) = abr::to_brush(brush);
                    Imported {
                        preset: preset_for("abr", name, parameters),
                        tip: Some(tip),
                        paper: None,
                        dropped: dropped.clone(),
                    }
                })
                .collect())
        }
        // The third container, and the only one whose single-brush and
        // whole-group forms are the same file with one node in it instead of
        // fifteen — so both extensions land on one reader.
        "sut" | "sutg" => {
            let bytes = preset::read_bytes(path)?;
            let file = clipstudio::from_sut(&bytes).map_err(|e| e.at(path))?;
            let base = display_name(&stem);
            let many = file.tools.len() > 1;
            Ok(file
                .tools
                .into_iter()
                .enumerate()
                .map(|(i, tool)| {
                    // A sub-tool carries its own name and a group's are what
                    // the artist reads in the palette. Numbered off the file
                    // only where a node was saved without one.
                    let name = if tool.name.is_empty() {
                        if many {
                            format!("{base} {}", i + 1)
                        } else {
                            base.clone()
                        }
                    } else {
                        display_name(&tool.name)
                    };
                    let mut dropped = tool.dropped;
                    for loss in &file.dropped {
                        if !dropped.contains(loss) {
                            dropped.push(loss);
                        }
                    }
                    Imported {
                        preset: preset_for("clipstudio", name, tool.brush),
                        tip: tool.tip,
                        paper: tool.paper,
                        dropped,
                    }
                })
                .collect())
        }
        "ron" => {
            let text = preset::read_to_string(path)?;
            let presets = preset::parse_library(&text).map_err(|e| e.at(path))?;
            // A `.ron` is text, so any tip it names is not in the file. The
            // library decides what to do with the dangling reference, because
            // only it knows whether it already holds a mask by that name.
            Ok(presets
                .into_iter()
                .map(|preset| Imported {
                    preset,
                    tip: None,
                    paper: None,
                    // An Umber library holds Umber brushes; there is nothing in
                    // it that Umber cannot render.
                    dropped: Vec::new(),
                })
                .collect())
        }
        _ => Err(PresetError::UnknownFormat(path.to_path_buf())),
    }
}

/// Wrap a converted brush up as a preset.
///
/// Filed by style, the same way the shipped library is, so the brush has a home
/// among its own kind whatever else happens to it. That is its `category`, and
/// it is deliberately not the whole answer: [`crate::preset::UserLibrary::
/// import_file`] puts what it reads in a collection called "Imported" as well,
/// because twenty brushes filed correctly across six collections are twenty
/// brushes somebody has to go and find. The style is what they fall back to the
/// moment they are moved out of there.
fn preset_for(source: &str, name: String, brush: Brush) -> BrushPreset {
    let category = crate::style::classify(&name, &brush).to_string();
    BrushPreset {
        id: format!("{source}/{}", preset::slug(&name)),
        name,
        category,
        collection: None,
        credit: None,
        brush,
        tip: None,
        paper: None,
    }
}

/// Where to look for the bitmap tip a loose `.kpp` names.
///
/// Krita's own resource layout — and every pack distributed as a directory
/// rather than a `.bundle`, which is GDQuest's and Raghukamath's — puts the
/// presets in `paintoppresets/` and their tips in a `brushes/` beside it. A
/// preset read straight off disk would otherwise arrive round, and that is
/// thirty of GDQuest's fifty brushes.
///
/// Deliberately three fixed places rather than a search: a reader that hunted
/// the filesystem for a file name a stranger supplied is a way to read things
/// nobody meant to offer.
fn sibling_brushes(path: &Path) -> impl Fn(&str) -> Option<Vec<u8>> {
    sibling_files(path, "brushes")
}

/// The same three places for a paper pattern, in `patterns/`.
///
/// A separate directory rather than a wider search, for exactly the reason
/// above: a `.kpp` names its pattern by a file name a stranger wrote, and Krita
/// keeps its own patterns and its own brush tips in two directories that a
/// bundle mirrors. Looking for a pattern among the tips would let a preset name
/// one and be handed the other.
fn sibling_patterns(path: &Path) -> impl Fn(&str) -> Option<Vec<u8>> {
    sibling_files(path, "patterns")
}

fn sibling_files(path: &Path, directory: &'static str) -> impl Fn(&str) -> Option<Vec<u8>> {
    let here = path.parent().map(Path::to_path_buf);
    move |wanted: &str| {
        // A file name, never a path: `../../etc/passwd` in a preset must not
        // reach outside the pack.
        if wanted.contains(['/', '\\']) || wanted.is_empty() {
            return None;
        }
        let here = here.as_ref()?;
        [
            here.join(wanted),
            here.join(directory).join(wanted),
            here.join("..").join(directory).join(wanted),
        ]
        .into_iter()
        .find_map(|candidate| std::fs::read(candidate).ok())
    }
}

/// The name to show: the file's own where it has one, otherwise the file name.
///
/// Most bitmap formats have a name field and packs routinely leave it empty —
/// and when they do fill it in, it is often the file name with the extension
/// still attached, which would show as "Chalk.gbr" in the picker.
fn embedded_name(embedded: &str, stem: &str) -> String {
    let embedded = embedded.trim();
    let trimmed = embedded
        .rsplit_once('.')
        .filter(|(_, extension)| {
            ["gbr", "gpb", "gih", "vbr", "kpp", "abr", "png"]
                .contains(&extension.to_ascii_lowercase().as_str())
        })
        .map_or(embedded, |(stem, _)| stem);
    if trimmed.is_empty() {
        display_name(stem)
    } else {
        display_name(trimmed)
    }
}

/// What reading this file will have to throw away, named.
///
/// The shipped library is built by a script that *refuses* a brush depending on
/// anything Umber cannot render, so nothing dishonest is ever shipped. A brush
/// the user imports themselves cannot be refused on the same grounds — it is
/// theirs, and a usable approximation beats a rejection — but it must not
/// arrive pretending to be the brush it came from. This is what lets the caller
/// say which part did not survive.
///
/// Deliberately best-effort: an unreadable file returns nothing here and fails
/// properly in [`read_file`], so a file is never reported on twice.
pub fn dropped_features(path: &Path) -> Vec<&'static str> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    let text = || preset::read_to_string(path).unwrap_or_default();
    let bytes = || preset::read_bytes(path).unwrap_or_default();

    match extension.as_str() {
        "myb" => mypaint::unsupported_features(&text()).unwrap_or_default(),
        "gbr" | "gpb" => gbr::dropped_features(&bytes()),
        "gih" => gih::dropped_features(&bytes()),
        "vbr" => vbr::dropped_features(&text()),
        "kpp" => {
            // Resolved the same way the read will resolve it, or a preset
            // whose tip — or whose pattern — *is* beside it would be reported
            // as missing one.
            let raw = bytes();
            let Ok(preset) = kpp::from_kpp_in(
                &raw,
                &kpp::Sidecar {
                    brushes: &sibling_brushes(path),
                    patterns: &sibling_patterns(path),
                },
            ) else {
                return Vec::new();
            };
            let mut out = preset.dropped;
            if preset.missing_tip.is_some() {
                out.push(kpp::MISSING_TIP);
            }
            out
        }
        "bundle" => bundle::dropped_features(&bytes()),
        "abr" => abr::dropped_features(&bytes()),
        "sut" | "sutg" => clipstudio::dropped_features(&bytes()),
        // An Umber library holds Umber brushes; there is nothing in it that
        // Umber cannot render.
        _ => Vec::new(),
    }
}

/// Brushes inside a *container* that could not be read at all, with the reason.
///
/// A file that fails whole comes back as the `Err` of [`read_file`], and every
/// caller already reports that. A container is the case with nowhere to put the
/// answer: one preset written by a paint engine Umber does not have must not
/// take the other forty-five with it, so [`bundle::from_bundle`] collects the
/// refusal and carries on — and `read_file`'s `bundle` arm then drops it,
/// because its return type has room for brushes and not for the ones that were
/// not brushes. Six of the thirteen presets the fetched packs write in another
/// engine are inside a `.bundle`, so the library generator's refusal table was
/// counting seven of them and reading as though the rest did not exist.
///
/// A sibling of [`dropped_features`] rather than a wider `read_file` **because
/// the wider return type is the more expensive answer for the same fact**: it
/// touches all nine arms and every caller, for a question only one arm can ever
/// answer non-empty, and every one of those edits is a chance to lose something
/// that is currently working. This is best-effort in exactly the way
/// `dropped_features` is — an unreadable file answers nothing here and fails
/// properly in `read_file`, so nothing is reported on twice — and it costs a
/// **second full decode of the archive**, every preset's PNG and every tip in
/// it, which is a price the library generator pays once per run and nothing on
/// a drawing path pays at all.
///
/// One boundary it does not reach: a bundle in which *every* preset is refused
/// makes [`bundle::from_bundle`] fail whole, so there is nothing to return
/// here. That case is not silent — `read_file` reports it, and its message
/// carries the count and one of the reasons — but it is a count rather than a
/// list.
pub fn refusals(path: &Path) -> Vec<String> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "bundle" => preset::read_bytes(path)
            .ok()
            .and_then(|bytes| bundle::from_bundle(&bytes).ok())
            .map(|contents| contents.refused)
            .unwrap_or_default(),
        // Every other reader either yields a brush or fails whole. A `.gih`
        // and a `.sutg` are containers too and neither has a partial failure
        // to report: a pipe's cells all come out of one decode, and a
        // sub-tool group's tools are approximated rather than refused.
        _ => Vec::new(),
    }
}

/// Turn a brush file's stem into something worth showing in a picker.
///
/// Brush packs name files for the filesystem — `8B_Pencil#1`, `coarse_bulk_1`,
/// `blend+paint` — and a list of two hundred of those is unreadable. This only
/// splits and capitalises; it deliberately does not try to expand abbreviations
/// or drop the trailing numbers, because those distinguish real variants.
pub fn display_name(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut start_of_word = true;
    for c in stem.chars() {
        // Runs collapse, and a space counts as one of them. Krita's own names
        // are written "c1) Pencil H Sketch - deevad 25.01", so a separator with
        // spaces either side would otherwise leave three in a row.
        if c == '_' || c == '-' || c.is_whitespace() {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
                start_of_word = true;
            }
        } else if start_of_word {
            out.extend(c.to_uppercase());
            start_of_word = false;
        } else {
            out.push(c);
        }
    }
    let trimmed = out.trim();
    if trimmed.is_empty() {
        "Brush".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_stems_become_readable_names() {
        assert_eq!(display_name("8B_Pencil#1"), "8B Pencil#1");
        assert_eq!(display_name("coarse_bulk_1"), "Coarse Bulk 1");
        assert_eq!(display_name("blend+paint"), "Blend+paint");
        assert_eq!(display_name("charcoal"), "Charcoal");
        assert_eq!(display_name("__"), "Brush");
        assert_eq!(display_name(""), "Brush");
        // Krita states its own names with spaces around the separator, so a
        // run of them has to collapse to one — "Sketch  Deevad" otherwise.
        assert_eq!(
            display_name("c1) Pencil H Sketch - deevad 25.01"),
            "C1) Pencil H Sketch Deevad 25.01"
        );
        assert_eq!(display_name("  spaced   out  "), "Spaced Out");
    }

    #[test]
    fn an_unknown_extension_is_refused_by_name() {
        // `.tpl` is Photoshop's tool preset, which is a whole tool rather than
        // a brush — one of the extensions deliberately not claimed.
        let err = read_file(Path::new("somewhere/nice.tpl")).unwrap_err();
        assert!(matches!(err, PresetError::UnknownFormat(_)));
        // The message must name the file, or a failed drag-and-drop of twenty
        // files tells the user nothing.
        assert!(err.to_string().contains("nice.tpl"), "{err}");
    }

    #[test]
    fn a_missing_file_is_an_io_error_not_a_panic() {
        let err = read_file(Path::new("definitely/not/here.myb")).unwrap_err();
        assert!(matches!(err, PresetError::Io { .. }));
    }

    /// A container's own refusals have to be reachable, or the shipped
    /// library's report counts a preset refused on its own and passes over the
    /// identical one inside an archive.
    ///
    /// This is the case `read_file` structurally cannot answer: it returns the
    /// brushes, and a preset written by another paint engine is not one.
    #[test]
    fn a_bundle_reports_the_presets_it_could_not_read() {
        let kpp = |xml: &str| {
            let mut out = Vec::new();
            let mut encoder = png::Encoder::new(&mut out, 1, 1);
            encoder.set_color(png::ColorType::Grayscale);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .add_ztxt_chunk("preset".to_string(), xml.to_string())
                .expect("chunk");
            let mut writer = encoder.write_header().expect("header");
            writer.write_image_data(&[9]).expect("data");
            drop(writer);
            out
        };
        let paintbrush = kpp("<Preset name=\"Ink\" paintopid=\"paintbrush\">\
             <param name=\"brush_definition\" type=\"string\"><![CDATA[\
             <Brush type=\"auto_brush\" spacing=\"0.1\" angle=\"0\">\
             <MaskGenerator diameter=\"20\" type=\"circle\" ratio=\"1\" hfade=\"1\"/>\
             </Brush>]]></param></Preset>");
        let warp = kpp("<Preset name=\"Warp\" paintopid=\"deformbrush\">\
             <param type=\"internal\" name=\"x\">1</param></Preset>");

        let mut archive = Vec::new();
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut archive));
            let options = zip::write::SimpleFileOptions::default();
            for (name, data) in [
                ("mimetype", &b"application/x-krita-resourcebundle"[..]),
                ("paintoppresets/a.kpp", &paintbrush),
                ("paintoppresets/b.kpp", &warp),
            ] {
                writer.start_file(name, options).expect("entry");
                writer.write_all(data).expect("write");
            }
            writer.finish().expect("finish");
        }

        let dir = std::env::temp_dir().join(format!("umber-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("pack.bundle");
        std::fs::write(&path, &archive).expect("write");

        let found = read_file(&path).expect("read");
        let refused = refusals(&path);
        std::fs::remove_dir_all(&dir).ok();

        // The good one still arrives; the other one is not lost in silence.
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].preset.name, "Ink");
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(refused[0].contains("deformbrush"), "{refused:?}");

        // Best-effort, exactly as `dropped_features` is: nothing is reported
        // on twice, and a file that fails whole fails through `read_file`.
        assert!(refusals(Path::new("definitely/not/here.bundle")).is_empty());
        assert!(refusals(Path::new("somewhere/nice.kpp")).is_empty());
    }

    /// The point of the whole exercise: a GIMP brush picked in the file dialog
    /// has to arrive as something that paints. Built by hand rather than taken
    /// from a pack — no `.gbr` collection states its licence inside the
    /// download in a way this project will vendor, see `docs/brush-sources.md`.
    #[test]
    fn a_gimp_brush_arrives_as_a_preset_with_its_mask() {
        let dir = std::env::temp_dir().join(format!("umber-gbr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("chalk_stamp.gbr");

        // 4x4, version 2, 8-bit mask, no embedded name.
        let mut file = Vec::new();
        for field in [29u32, 2, 4, 4, 1] {
            file.extend_from_slice(&field.to_be_bytes());
        }
        file.extend_from_slice(b"GIMP");
        file.extend_from_slice(&30u32.to_be_bytes());
        file.push(0);
        file.extend((0..16).map(|i| i * 16));
        std::fs::write(&path, &file).expect("write");

        let found = read_file(&path).expect("read");
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(found.len(), 1);
        let Imported {
            preset,
            tip,
            paper,
            dropped,
        } = &found[0];
        // A `.gbr` is a stamp and nothing else — there is no paper in the
        // format at all, so this is the shape every reader but Clip Studio's
        // has.
        assert!(paper.is_none());
        // A plain 8-bit `.gbr` is exactly what Umber stamps, so nothing about
        // it is an approximation and the import has nothing to apologise for.
        assert!(dropped.is_empty(), "{dropped:?}");
        // The file has no name of its own, so the file name is what shows.
        assert_eq!(preset.name, "Chalk Stamp");
        assert_eq!(preset.id, "gbr/chalk-stamp");
        assert_eq!(preset.brush.size, 4.0);
        assert_eq!(preset.brush.spacing, 0.30);
        // Filed by the mark it makes, like everything else in the library.
        assert_eq!(preset.category, crate::style::Style::CHARCOAL);
        // The reference is filled in by the library, which is what decides
        // where the mask is stored.
        assert!(preset.tip.is_none());
        let tip = tip.as_ref().expect("a mask came with it");
        assert_eq!((tip.width(), tip.height()), (4, 4));
    }
}
