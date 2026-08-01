//! The shipped pattern bitmaps, embedded.
//!
//! **Generated** by `cargo run -p umber-core --example build-bitmaps` and
//! `--example build-brush-library`, from the files in `../../../assets/patterns`.
//! `include_bytes!` needs a literal path, so the set of shipped bitmaps has
//! to be source; writing it from the directory listing is what keeps the two
//! from disagreeing, and either generator rewrites the whole table so that
//! running one on its own cannot leave it naming a file that is not there.
//!
//! Do not edit by hand.

/// Name and 8-bit greyscale PNG, sorted by name.
pub(crate) const PATTERNS: &[(&str, &[u8])] = &[
    (
        "canvas",
        include_bytes!("../../../assets/patterns/canvas.png"),
    ),
    ("grit", include_bytes!("../../../assets/patterns/grit.png")),
    (
        "tooth",
        include_bytes!("../../../assets/patterns/tooth.png"),
    ),
];
