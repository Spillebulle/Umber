//! The shipped tip bitmaps, embedded.
//!
//! **Generated** by `cargo run -p umber-core --example build-bitmaps`,
//! from the files in `../assets/tips`. `include_bytes!` needs a literal
//! path, so the set of shipped bitmaps has to be source; writing it from the
//! directory listing is what keeps the two from disagreeing.
//!
//! Do not edit by hand.

/// Name and 8-bit greyscale PNG, sorted by name.
pub(crate) const TIPS: &[(&str, &[u8])] = &[(
    "umber-stipple",
    include_bytes!("../assets/tips/umber-stipple.png"),
)];
