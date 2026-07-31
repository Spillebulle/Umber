//! Embeds the Windows executable icon.
//!
//! `Window::set_window_icon` only reaches the running process's own window. It
//! does nothing for the icon Explorer draws on the file, the one pinned to the
//! taskbar, or the one shown before the process has started — those come from
//! an `RT_GROUP_ICON` resource compiled into the executable, which is what this
//! script adds.
//!
//! The `.ico` is generated, not drawn: see the `regenerate_icons` test in
//! `crates/umber-app/src/logo.rs`. Changing the mark and re-running it is all
//! that is needed for the icon here to follow.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let icon = PathBuf::from(manifest).join("../../assets/icons/umber.ico");
    println!("cargo::rerun-if-changed={}", icon.display());
    embed_icon(&icon);
}

/// Host-gated rather than target-gated, because Cargo resolves
/// `[target.…build-dependencies]` against the *host* triple: `winresource` only
/// exists as a dependency when the build script itself is compiled for Windows.
/// It in turn checks `CARGO_CFG_TARGET_OS` and does nothing when the target is
/// not Windows, so a Windows host cross-compiling elsewhere stays clean too.
#[cfg(windows)]
fn embed_icon(icon: &Path) {
    if !icon.exists() {
        println!(
            "cargo::warning=no {} — the executable will have the default icon",
            icon.display()
        );
        return;
    }

    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy());

    // The resource compiler is part of the Windows SDK, and a machine without
    // it is a machine that can still perfectly well build and run Umber. An
    // icon is cosmetic; refusing to produce a binary over one is not a trade
    // worth making, so this warns and carries on. The warning is loud enough
    // to notice in CI, where the SDK is always present.
    if let Err(e) = res.compile() {
        println!("cargo::warning=could not embed the Windows icon: {e}");
    }
}

#[cfg(not(windows))]
fn embed_icon(_icon: &Path) {}
