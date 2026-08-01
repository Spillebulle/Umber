//! The `include_bytes!` tables that carry Umber's shipped bitmaps.
//!
//! Shared by `build-bitmaps.rs`, which draws the papers and Umber's own stamp,
//! and `build-brush-library.rs`, which writes the brush packs' masks. Both end
//! by rewriting a table from a directory listing, and the two must agree to the
//! byte: a table written one way and rewritten the other would show up as a
//! diff on a generated file that nothing changed.
//!
//! Not an example of its own — it has no `main`, and Cargo only treats
//! `examples/*.rs` and `examples/*/main.rs` as examples — so it is included
//! with `#[path]` by the two that are.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// Rewrite an `include_bytes!` table from a directory listing.
///
/// The table *is* the listing, so a bitmap that is not in the directory is not
/// in the binary and one that is cannot be forgotten. Sorted, because
/// `read_dir` order is whatever the filesystem feels like and this file is
/// committed.
pub fn write_table(path: &Path, what: &str, constant: &str, include_prefix: &str, dir: &Path) {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .expect("read directory")
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".png"))
        .collect();
    names.sort();

    let mut out = String::new();
    writeln!(
        out,
        "//! The shipped {what} bitmaps, embedded.\n\
         //!\n\
         //! **Generated** by `cargo run -p umber-core --example build-bitmaps` and\n\
         //! `--example build-brush-library`, from the files in `{include_prefix}`.\n\
         //! `include_bytes!` needs a literal path, so the set of shipped bitmaps has\n\
         //! to be source; writing it from the directory listing is what keeps the two\n\
         //! from disagreeing, and either generator rewrites the whole table so that\n\
         //! running one on its own cannot leave it naming a file that is not there.\n\
         //!\n\
         //! Do not edit by hand.\n\n\
         /// Name and 8-bit greyscale PNG, sorted by name.\n\
         pub(crate) const {constant}: &[(&str, &[u8])] = &["
    )
    .expect("write");
    for name in &names {
        let stem = name.trim_end_matches(".png");
        writeln!(
            out,
            "    (\"{stem}\", include_bytes!(\"{include_prefix}/{name}\")),"
        )
        .expect("write");
    }
    writeln!(out, "];").expect("write");

    std::fs::write(path, out).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    println!("{} <- {} entries", path.display(), names.len());
}

/// The workspace root, from this crate's manifest directory.
///
/// `build-brush-library.rs` finds it by walking up from the working directory
/// instead, because it may be run from anywhere in the tree; this is the
/// version for a generator that only ever runs under `cargo run -p`.
#[allow(
    dead_code,
    reason = "one of the two including generators uses each half"
)]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}
