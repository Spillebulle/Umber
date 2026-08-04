//! The shipped tip bitmaps, embedded.
//!
//! **Generated** by `cargo run -p umber-core --example build-bitmaps` and
//! `--example build-brush-library`, from the files in `../assets/tips`.
//! `include_bytes!` needs a literal path, so the set of shipped bitmaps has
//! to be source; writing it from the directory listing is what keeps the two
//! from disagreeing, and either generator rewrites the whole table so that
//! running one on its own cannot leave it naming a file that is not there.
//!
//! Do not edit by hand.

/// Name and 8-bit greyscale PNG, sorted by name.
pub(crate) const TIPS: &[(&str, &[u8])] = &[
    (
        "deevad-c2-mechanical-pencil-detail-deevad-25-01",
        include_bytes!("../assets/tips/deevad-c2-mechanical-pencil-detail-deevad-25-01.png"),
    ),
    (
        "deevad-c4-bristles-lineart-deevad-25-01",
        include_bytes!("../assets/tips/deevad-c4-bristles-lineart-deevad-25-01.png"),
    ),
    (
        "deevad-c5-thin-brush-hard-edge-textured-deevad-25-01",
        include_bytes!("../assets/tips/deevad-c5-thin-brush-hard-edge-textured-deevad-25-01.png"),
    ),
    (
        "deevad-d-glazing-round-deevad-25-01",
        include_bytes!("../assets/tips/deevad-d-glazing-round-deevad-25-01.png"),
    ),
    (
        "deevad-f-rough-rake-textured-deevad-25-01",
        include_bytes!("../assets/tips/deevad-f-rough-rake-textured-deevad-25-01.png"),
    ),
    (
        "deevad-f-thick-dry-canvas-deevad-25-01",
        include_bytes!("../assets/tips/deevad-f-thick-dry-canvas-deevad-25-01.png"),
    ),
    (
        "deevad-i-glazing-round-mix-deevad-25-01",
        include_bytes!("../assets/tips/deevad-i-glazing-round-mix-deevad-25-01.png"),
    ),
    (
        "deevad-k-blender-rake-smudge-deevad-25-01",
        include_bytes!("../assets/tips/deevad-k-blender-rake-smudge-deevad-25-01.png"),
    ),
    (
        "deevad-y-textured-big-sponge-deevad-25-01",
        include_bytes!("../assets/tips/deevad-y-textured-big-sponge-deevad-25-01.png"),
    ),
    (
        "gdquest-block",
        include_bytes!("../assets/tips/gdquest-block.png"),
    ),
    (
        "gdquest-gdquest-cloud-medium",
        include_bytes!("../assets/tips/gdquest-gdquest-cloud-medium.png"),
    ),
    (
        "gdquest-gdquest-shadow-directional-dark",
        include_bytes!("../assets/tips/gdquest-gdquest-shadow-directional-dark.png"),
    ),
    (
        "gdquest-leaves-patch-1",
        include_bytes!("../assets/tips/gdquest-leaves-patch-1.png"),
    ),
    (
        "raghukamath-pack01-drybrush",
        include_bytes!("../assets/tips/raghukamath-pack01-drybrush.png"),
    ),
    (
        "raghukamath-pack01-drybrush2",
        include_bytes!("../assets/tips/raghukamath-pack01-drybrush2.png"),
    ),
    (
        "raghukamath-pack01-fx",
        include_bytes!("../assets/tips/raghukamath-pack01-fx.png"),
    ),
    (
        "raghukamath-pack01-sponze-dry",
        include_bytes!("../assets/tips/raghukamath-pack01-sponze-dry.png"),
    ),
    (
        "umber-stipple",
        include_bytes!("../assets/tips/umber-stipple.png"),
    ),
];
