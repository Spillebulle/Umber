//! Reading brushes that other applications wrote.
//!
//! Every importer lands on [`crate::Brush`], which is a parametric round brush
//! and nothing more. Formats richer than that lose the parts Umber cannot
//! render, and each importer documents exactly what it drops — see
//! [`mypaint`] for the shape those notes take. An importer that quietly
//! discarded half a brush would be worse than one that refuses the file.

pub mod mypaint;

use std::path::Path;

use crate::preset::{self, BrushPreset, PresetError};

/// Read every brush in a file, whatever format it is in.
///
/// The format is chosen by extension. Sniffing the contents was tempting —
/// `.myb` is JSON and an Umber library is RON, so they are trivially
/// distinguishable — but a user who renames a file has told us something, and
/// guessing past that produces confusing failures much later.
pub fn read_file(path: &Path) -> Result<Vec<BrushPreset>, PresetError> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();

    match extension.as_str() {
        "myb" => {
            let text = preset::read_to_string(path)?;
            let brush = mypaint::from_myb(&text).map_err(|e| e.at(path))?;
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            let name = display_name(&stem);
            Ok(vec![BrushPreset {
                id: format!("mypaint/{}", preset::slug(&name)),
                name,
                category: "Imported".to_string(),
                credit: None,
                brush,
            }])
        }
        "ron" => {
            let text = preset::read_to_string(path)?;
            preset::parse_library(&text).map_err(|e| e.at(path))
        }
        _ => Err(PresetError::UnknownFormat(path.to_path_buf())),
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
}
