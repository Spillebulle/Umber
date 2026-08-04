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
//! That was still not enough, and the fifth piece is the other end of the same
//! wire: **the installed shortcut has to declare the same id.** Microsoft's
//! rule for an explicit AppUserModelID is that it "must also be assigned to all
//! running windows or processes, **shortcuts**, and file associations", and the
//! shortcut is where the taskbar looks — setting `System.AppUserModel.ID` on it
//! is what "allows the taskbar to identify the proper shortcut to pin and
//! ensures that windows belonging to the process are appropriately associated
//! with that taskbar button", after which "the command line, icon, and text of
//! the shortcut" supply the button's own. Umber claimed an identity that
//! nothing installed on the machine owned, so the button had no shortcut to
//! take any of that from. `packaging/windows/umber.wxs` now sets it through
//! `MsiShortcutProperty`, which is the mechanism Microsoft names for an
//! installer, and that in turn is why the Start Menu shortcut had to stop being
//! *advertised*: an advertised shortcut carries no properties, and it also took
//! its icon from the Icon table, where a row named without a file extension is
//! the generic document page. `the_start_menu_shortcut_declares_the_same_
//! application_id` reads the value back out of the packaging.
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
///
/// macOS is the one platform that reads it from nowhere: an application there
/// is identified by its bundle, so nothing in this build refers to the constant
/// and CI's `-D warnings` calls it dead. It is still *the* name — the tests
/// below pin it against the packaging on every platform — so the allowance is
/// narrowed to macOS rather than the constant being moved behind a `cfg` that
/// would make those tests platform-specific too.
#[cfg_attr(target_os = "macos", allow(dead_code))]
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

    fn packaging() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging")
    }

    fn wxs() -> String {
        std::fs::read_to_string(packaging().join("windows").join("umber.wxs"))
            .expect("the Windows installer's authoring")
    }

    /// Every value of `attribute` in `text`, in order.
    fn attributes<'a>(text: &'a str, attribute: &str) -> Vec<&'a str> {
        let opener = format!("{attribute}=\"");
        let mut found = Vec::new();
        let mut rest = text;
        while let Some(at) = rest.find(&opener) {
            rest = &rest[at + opener.len()..];
            match rest.find('"') {
                Some(end) => {
                    found.push(&rest[..end]);
                    rest = &rest[end..];
                }
                None => break,
            }
        }
        found
    }

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

    /// The Windows half of the same rule, and the one that was missing.
    ///
    /// `claim_identity` hands the process an id; the shortcut is what tells the
    /// shell which installed application that id belongs to. Two spellings of
    /// it is a taskbar button associated with nothing, which is the whole of
    /// the bug — so this reads the value out of the packaging rather than
    /// comparing a copy of the string.
    #[test]
    fn the_start_menu_shortcut_declares_the_same_application_id() {
        let wxs = wxs();
        // Sliced to the `<ShortcutProperty …/>` element itself rather than to a
        // fixed run of bytes: the comment above the element mentions the key by
        // name, and a fixed window would run on into the `RegistryValue` below
        // and read *its* `Value` if the attributes were ever reordered.
        let at = wxs
            .find("<ShortcutProperty")
            .expect("the Start Menu shortcut must carry an AppUserModelID");
        let end = at + wxs[at..].find('>').expect("an unterminated element");
        let element = &wxs[at..end];
        assert!(
            element.contains("Key=\"System.AppUserModel.ID\""),
            "the only shortcut property is not the application id"
        );
        assert!(
            attributes(element, "Value").first() == Some(&APP_ID),
            "the shortcut declares an application id this process never claims"
        );
    }

    /// An advertised shortcut cannot carry that property at all — MSI's
    /// `MsiShortcutProperty` table applies to real `.lnk` files only — and it
    /// takes its icon from the Icon table rather than from the executable,
    /// which is the other half of what shipped wrong. Both symptoms come back
    /// together the moment somebody sets this to yes.
    ///
    /// The three assertions are one claim in three parts, because the negative
    /// on its own is satisfied by a file with no shortcut in it: there is a
    /// shortcut, it points at the executable rather than at an icon of its
    /// own, and nothing in the file is advertised.
    #[test]
    fn the_start_menu_shortcut_is_a_real_one() {
        let wxs = wxs();
        let at = wxs.find("<Shortcut ").expect("there is no Start Menu shortcut");
        let end = at + wxs[at..].find('>').expect("an unterminated element");
        let element = &wxs[at..end];

        assert!(
            element.contains("Target=\"[#UmberExe]\""),
            "the shortcut does not point at the executable, so it cannot take \
             the executable's icon"
        );
        assert!(
            !element.contains("Icon="),
            "a shortcut that names an Icon row takes it from the Icon table, \
             which cannot serve a .ico to a shortcut at all"
        );
        assert!(
            !wxs.contains("Advertise=\"yes\""),
            "an advertised shortcut can carry no AppUserModelID and takes its \
             icon from the Icon table"
        );
    }

    /// Windows Installer streams each Icon row out to a file named with its Id
    /// and then asks the shell to identify that file by name, so an Id with no
    /// extension is a file nothing can identify. WiX's own `Shortcut.xsd` says
    /// the identifier "should have the same extension as the file that it
    /// points at"; the Icon table's rule for shortcuts is stronger still. That
    /// this is what drew the blank page in Add/Remove Programs is inference
    /// rather than documented — but the extension costs nothing.
    #[test]
    fn every_installer_icon_is_named_with_a_file_extension() {
        let wxs = wxs();
        let ids: Vec<&str> = wxs
            .match_indices("<Icon ")
            .map(|(at, _)| &wxs[at..])
            .filter_map(|element| attributes(element, "Id").first().copied())
            .collect();
        assert!(!ids.is_empty(), "the installer declares no icon at all");
        for id in ids {
            assert!(
                id.ends_with(".ico") || id.ends_with(".exe"),
                "Icon id {id:?} has no extension, so the shell cannot identify \
                 the file the installer extracts it to"
            );
        }
    }

    /// Every file the installer names has to be one the release workflow
    /// actually stages, or `wix build` fails at tag time with the tag already
    /// pushed — which is the moment `ci.yml`'s packaging job exists to avoid.
    #[test]
    fn the_release_workflow_stages_every_asset_the_installer_names() {
        let wxs = wxs();
        let workflow = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.github/workflows/release.yml"),
        )
        .expect("the release workflow");

        let mut rest = wxs.as_str();
        let mut named = 0;
        while let Some(at) = rest.find("$(var.AssetDir)\\") {
            rest = &rest[at + "$(var.AssetDir)\\".len()..];
            let end = rest.find('"').expect("an unterminated SourceFile");
            let file = &rest[..end];
            assert!(
                workflow.contains(file),
                "the installer wants {file}, which the release workflow never \
                 puts in the asset directory"
            );
            named += 1;
            rest = &rest[end..];
        }
        assert!(
            named >= 4,
            "only {named} assets found — did the paths change?"
        );
    }

    /// Not a name this module owns, but a one-line invariant that any edit to
    /// the shortcut arrangement above sits next to. Umber installs per-machine,
    /// so the installer is elevated: without this the "Start Umber" checkbox
    /// runs Umber as the elevated account and every preference, brush and
    /// autosave it writes lands in that profile instead of the user's.
    #[test]
    fn the_installer_still_starts_umber_as_the_user() {
        assert!(wxs().contains("Impersonate=\"yes\""));
    }
}
