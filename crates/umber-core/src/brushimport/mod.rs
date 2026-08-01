//! Reading brushes that other applications wrote.
//!
//! Every importer lands on [`crate::Brush`], which is a parametric round brush
//! and nothing more. Formats richer than that lose the parts Umber cannot
//! render, and each importer documents exactly what it drops — see
//! [`mypaint`] for the shape those notes take. An importer that quietly
//! discarded half a brush would be worse than one that refuses the file.

pub mod gbr;
pub mod mypaint;

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
            }])
        }
        "gbr" => {
            let bytes = preset::read_bytes(path)?;
            let brush = gbr::from_gbr(&bytes).map_err(|e| e.at(path))?;
            // The format has a name field of its own, but packs routinely leave
            // it empty — and when they do fill it in, it is often the file name
            // with the extension still attached, which would show as
            // "Chalk.gbr" in the picker.
            let embedded = brush.name.trim();
            let embedded = embedded
                .strip_suffix(".gbr")
                .or_else(|| embedded.strip_suffix(".GBR"))
                .unwrap_or(embedded);
            let name = if embedded.is_empty() {
                display_name(&stem)
            } else {
                display_name(embedded)
            };
            let (parameters, tip) = gbr::to_brush(brush);
            Ok(vec![Imported {
                preset: preset_for("gbr", name, parameters),
                tip: Some(tip),
            }])
        }
        "ron" => {
            let text = preset::read_to_string(path)?;
            let presets = preset::parse_library(&text).map_err(|e| e.at(path))?;
            // A `.ron` is text, so any tip it names is not in the file. The
            // library decides what to do with the dangling reference, because
            // only it knows whether it already holds a mask by that name.
            Ok(presets
                .into_iter()
                .map(|preset| Imported { preset, tip: None })
                .collect())
        }
        _ => Err(PresetError::UnknownFormat(path.to_path_buf())),
    }
}

/// Wrap a converted brush up as a preset.
///
/// Filed by style, the same way the shipped library is, so a brush you import
/// lands among its own kind instead of in a bin called "Imported" that grows
/// without order.
fn preset_for(source: &str, name: String, brush: Brush) -> BrushPreset {
    let category = crate::style::classify(&name, &brush).to_string();
    BrushPreset {
        id: format!("{source}/{}", preset::slug(&name)),
        name,
        category,
        credit: None,
        brush,
        tip: None,
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

    match extension.as_str() {
        "myb" => preset::read_to_string(path)
            .ok()
            .and_then(|text| mypaint::unsupported_features(&text).ok())
            .unwrap_or_default(),
        "gbr" => preset::read_bytes(path)
            .map(|bytes| gbr::dropped_features(&bytes))
            .unwrap_or_default(),
        // An Umber library holds Umber brushes; there is nothing in it that
        // Umber cannot render.
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
        if c == '_' || c == '-' {
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
    }

    #[test]
    fn an_unknown_extension_is_refused_by_name() {
        let err = read_file(Path::new("somewhere/nice.kpp")).unwrap_err();
        assert!(matches!(err, PresetError::UnknownFormat(_)));
        // The message must name the file, or a failed drag-and-drop of twenty
        // files tells the user nothing.
        assert!(err.to_string().contains("nice.kpp"), "{err}");
    }

    #[test]
    fn a_missing_file_is_an_io_error_not_a_panic() {
        let err = read_file(Path::new("definitely/not/here.myb")).unwrap_err();
        assert!(matches!(err, PresetError::Io { .. }));
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
        let Imported { preset, tip } = &found[0];
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
