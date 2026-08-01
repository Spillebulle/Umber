//! Redraw the pictures in `docs/images/`.
//!
//! ```sh
//! cargo run -p umber-app --example docs-images
//! ```
//!
//! Run by hand when the interface changes, like `examples/build-bitmaps.rs` in
//! `umber-core`; the results are committed. Everything it does lives in
//! `umber_app::docshot`, which is inside the crate because the interface it
//! photographs is private to it — this is the handle.
//!
//! Needs a GPU adapter for the interface shots. Without one it writes the banner
//! and says which pictures it skipped, rather than committing a blank file.

fn main() {
    env_logger::init();

    // The workspace root, from this crate's manifest. Taking it from the current
    // directory instead would put the images somewhere different depending on
    // where cargo was invoked from, and these are committed files.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..");

    if let Err(e) = umber_app::docshot::generate(&root) {
        eprintln!("docs-images: {e}");
        std::process::exit(1);
    }
}
