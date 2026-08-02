//! Who the operating system thinks Umber is.
//!
//! Distinct from the *icons*, which are covered in three other places and were
//! not enough on their own:
//!
//! * `crates/umber-desktop/build.rs` compiles an `RT_GROUP_ICON` into the
//!   executable — what Explorer draws on the file and what the Start Menu
//!   shortcut uses.
//! * `logo::window_icon` is `ICON_SMALL`, the title bar's.
//! * `logo::taskbar_icon` is `ICON_BIG`, which is what the taskbar button and
//!   Alt-Tab draw.
//!
//! All three were in place and the taskbar still showed the generic paper icon.
//! The missing piece is identity rather than artwork: Windows groups taskbar
//! buttons by **AppUserModelID**, and a process that never sets one is given a
//! derived one belonging to whatever launched it. Running from a terminal, from
//! Cargo, or from an installer's shortcut therefore produced three different
//! identities, only some of which resolve to anything with an icon.
//!
//! [`claim_identity`] takes one explicitly. It has to run **before the first
//! window exists** — the shell reads the identity when the button is created,
//! and setting it afterwards changes nothing about a button already on screen.
//!
//! The Linux half of the same question is the window's **app id**, set at window
//! creation in `app.rs`, because Wayland ignores window icons entirely and
//! matches the app id against an installed `.desktop` file instead.

/// The identity the taskbar groups Umber's windows under.
///
/// The same string as the Linux application id and the Flatpak app id, and
/// therefore also the name of the installed `.desktop` file and of the icons
/// beside it. One name for the application across every platform is worth more
/// than a prettier one on each: this is the string that has to match the
/// packaging, and a second spelling of it is a mismatch waiting to happen.
///
/// Microsoft asks for `Company.Product`, which this satisfies while also being
/// the reverse-DNS form the freedesktop world wants.
pub const APP_ID: &str = "io.github.spillebulle.umber";

/// Tell Windows which application this process is, before any window exists.
///
/// Failure is deliberately ignored. An unset identity is what every previous
/// build shipped with, so the worst case is the behaviour Umber already had —
/// and refusing to start a painting application because the taskbar might group
/// its button oddly would be an absurd trade.
#[cfg(windows)]
pub fn claim_identity() {
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    // Win32 wants UTF-16 with a terminator; the constant is ASCII, so this is
    // exact rather than lossy.
    let wide: Vec<u16> = APP_ID.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: the pointer is to a NUL-terminated UTF-16 buffer that outlives
    // the call, which is the whole of this function's contract.
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if hr < 0 {
        log::debug!("could not set the application id: HRESULT {hr:#x}");
    }
}

#[cfg(not(windows))]
pub fn claim_identity() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identity has to be the same string the packaging installs under, or
    /// the desktop entry and the running window describe two applications and
    /// the icon is looked up under a name nothing provides.
    ///
    /// Pinned against the files themselves rather than against a copy of the
    /// string, so renaming one and not the other fails here rather than in a
    /// package nobody opens until it is released.
    #[test]
    fn the_application_id_matches_what_the_packaging_installs() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("packaging");

        let desktop = root.join(format!("{APP_ID}.desktop"));
        assert!(
            desktop.is_file(),
            "no desktop entry named for the app id: {}",
            desktop.display()
        );

        let flatpak = std::fs::read_to_string(root.join("linux").join(format!("{APP_ID}.yml")))
            .expect("the Flatpak manifest is named for the app id too");
        assert!(
            flatpak.contains(&format!("app-id: {APP_ID}")),
            "the Flatpak manifest declares a different app id"
        );
    }

    /// Wayland matches the window's app id to the desktop entry's *basename*,
    /// and X11 matches `StartupWMClass` to the window's class. Both are set in
    /// `app.rs` from this module; this pins the half that lives in the file.
    #[test]
    fn the_desktop_entry_names_an_icon_and_a_window_class() {
        let entry = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packaging")
                .join(format!("{APP_ID}.desktop")),
        )
        .expect("desktop entry");
        assert!(
            entry.contains(&format!("Icon={APP_ID}")),
            "the icon is looked up by app id"
        );
        assert!(
            entry.contains("StartupWMClass=umber"),
            "X11's class must match what the window sets"
        );
    }
}
