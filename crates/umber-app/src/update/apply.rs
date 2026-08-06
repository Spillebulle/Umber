//! Putting a downloaded release in place.
//!
//! Three shapes, one per installation Umber owns (see [`super::install`]):
//!
//! * **Portable** — the archive holds one binary; lift it out and put it where
//!   the running one is.
//! * **AppImage** — the download *is* the application; replace the file
//!   `$APPIMAGE` names.
//! * **MSI** — write the package down and start Umber's own updater over it.
//!   Umber does not touch Program Files itself: the installer owns those files
//!   and keeps a record of them. It used to hand `msiexec` the package and get
//!   out of the way, which showed a Windows installer nobody had asked for and
//!   left the artist to start Umber again themselves; `super::installer` has
//!   what replaced that and why it has to be a second process.
//!
//! The awkward part is Windows, where a running executable cannot be deleted or
//! written to — but *can* be renamed, because the lock is on the file's data,
//! not on its directory entry. So the swap is rename-then-replace: the running
//! binary is moved aside to `umber.exe.old`, the new one takes its name, and
//! the next start deletes the leftover ([`sweep_previous_binary`]). If the
//! second rename fails the first is undone, so a failed update leaves a working
//! Umber rather than none at all.
//!
//! On Unix a plain `rename` is enough and is atomic: the running process holds
//! the old inode open and carries on, while the name points at the new file
//! from that moment.

use super::flow::Stage;
use super::install::InstallKind;
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

/// A downloaded binary can be large, but not this large. A tarball claiming a
/// gigabyte of `umber` is a decompression bomb or a mistake, and either way is
/// not something to hold in memory.
const MAX_BINARY: u64 = 512 * 1024 * 1024;

/// What happened, and therefore what the user has to do next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The new build is in place and runs the next time Umber is started.
    Restart,
    /// An installer is now running and needs Umber to close.
    Installer,
}

/// Put a downloaded release in place.
///
/// `bytes` is exactly what the release carried, already length-checked against
/// what the API reported.
///
/// `report` is told which stage is beginning, so the dialog's bar names what is
/// happening rather than sitting on one label from the end of the download to
/// the end of the install. The stages differ per installation kind and that is
/// the honest reading: there is no archive to unpack in an AppImage, and the
/// MSI is not installed by Umber at all — it is handed over.
pub fn apply(
    kind: &InstallKind,
    asset_name: &str,
    bytes: &[u8],
    version: &str,
    report: &dyn Fn(Stage),
) -> Result<Applied, String> {
    match kind {
        InstallKind::Msi => {
            report(Stage::HandingOver);
            hand_to_msiexec(asset_name, bytes, version)?;
            Ok(Applied::Installer)
        }

        // One file, and the download is it. No archive to open.
        InstallKind::AppImage(path) => {
            report(Stage::Installing);
            swap_in(path, bytes)?;
            Ok(Applied::Restart)
        }

        InstallKind::Portable => {
            let exe = std::env::current_exe()
                .map_err(|e| format!("Umber could not find its own program file: {e}"))?;
            report(Stage::Unpacking);
            let binary = if asset_name.ends_with(".zip") {
                binary_from_zip(bytes, "umber.exe")?
            } else {
                binary_from_tar_gz(bytes, "umber")?
            };
            report(Stage::Installing);
            swap_in(&exe, &binary)?;
            Ok(Applied::Restart)
        }

        // Never reached: nothing is downloaded for these. Kept as a refusal
        // rather than an `unreachable!`, because the one thing that must not
        // happen here is writing over a package manager's files.
        InstallKind::Managed(_) | InstallKind::Unknown => Err(
            "This installation is not Umber's to replace. Take the new build from \
             the releases page."
                .to_string(),
        ),
    }
}

/// Replace the file at `path` with `bytes`, keeping the old one recoverable
/// until the swap has succeeded.
fn swap_in(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let new = suffixed(path, ".new");
    // Written beside the target rather than into a temporary directory: a
    // rename is only atomic — and on Windows only *possible* — within one
    // volume, and a portable copy can be on a different drive from `%TEMP%`.
    write_executable(&new, bytes)?;

    if cfg!(windows) {
        let old = suffixed(path, ".old");
        // A leftover from a previous update that the next start failed to
        // sweep. Renaming onto an existing name fails on Windows.
        let _ = std::fs::remove_file(&old);
        if let Err(e) = std::fs::rename(path, &old) {
            let _ = std::fs::remove_file(&new);
            return Err(format!(
                "Umber could not move its own program file aside: {e}\n\n\
                 Nothing was changed."
            ));
        }
        if let Err(e) = std::fs::rename(&new, path) {
            // Put the running binary's name back before reporting. A half-done
            // swap that leaves nothing at `umber.exe` is the one outcome that
            // costs the user their installation.
            let _ = std::fs::rename(&old, path);
            let _ = std::fs::remove_file(&new);
            return Err(format!(
                "Umber could not put the new build in place: {e}\n\n\
                 The version you are running was left as it was."
            ));
        }
        return Ok(());
    }

    // Unix: one rename, and it is atomic. The running process keeps the old
    // inode open, so nothing is disturbed until Umber is next started.
    std::fs::rename(&new, path).map_err(|e| {
        let _ = std::fs::remove_file(&new);
        format!(
            "Umber could not put the new build in place: {e}\n\n\
             The version you are running was left as it was."
        )
    })
}

/// Write a file and make sure it can be run.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(path, bytes).map_err(|e| {
        format!(
            "Umber could not write {}: {e}\n\n\
             This copy may be somewhere it is not allowed to change itself.",
            path.display(),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // An archive member's mode is not carried across by `fs::write`, and a
        // binary without the execute bit is an update that silently bricks the
        // installation.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Umber could not make {} executable: {e}", path.display()))?;
    }
    Ok(())
}

/// `path` with `suffix` appended to the whole name.
///
/// Appended rather than substituted: `Path::with_extension(".new")` on
/// `umber.exe` gives `umber.new`, which is a *different program's* name on a
/// system that has one, and loses the `.exe` Windows needs to run it.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Delete the binary a previous update moved aside.
///
/// Called once at start-up. On Windows the old file cannot be deleted while it
/// is running, so the swap leaves it behind and this is the first moment it can
/// go. Failure is ignored: a stale `umber.exe.old` is untidy, not broken, and
/// an application that refused to start over one would be worse.
pub fn sweep_previous_binary() {
    if !cfg!(windows) {
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    for leftover in [suffixed(&exe, ".old"), suffixed(&exe, ".new")] {
        if leftover.exists() && std::fs::remove_file(&leftover).is_ok() {
            log::info!("removed {} from a previous update", leftover.display());
        }
    }
}

/// Start the build that is now at Umber's own path, so the caller can exit into
/// it.
///
/// Only ever called after [`Applied::Restart`], which means the swap succeeded
/// and the new binary is at exactly the name the running one was started from —
/// so `current_exe` is the new build, not the old one, on every platform.
///
/// **The failure has to be recoverable**, which is why this reports rather than
/// exiting itself: an update that could not start the new copy must leave the
/// old one running, not leave the user with no Umber at all. `app.rs` exits only
/// on `Ok`.
///
/// On Windows the displaced `umber.exe.old` is still locked by *this* process
/// while the new one starts, so the sweep the new process runs may find it
/// undeletable and leave it. That is the untidy case [`sweep_previous_binary`]
/// already describes, and it is cleared by the start after next; it is not worth
/// delaying a restart to avoid.
pub fn relaunch() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("Umber could not find its own program file: {e}"))?;
    // No arguments carried over. Umber's are a document to open, and reopening
    // whatever was on the command line an hour ago is not what "restart" means
    // — the session's own documents are the autosave's business.
    std::process::Command::new(&exe)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            format!(
                "Umber could not start the new version: {e}\n\n\
                 It is installed at {}, and runs the next time you start Umber.",
                exe.display(),
            )
        })
}

/// Lift one file out of a zip.
fn binary_from_zip(bytes: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    let reader = std::io::Cursor::new(bytes);
    let mut archive =
        zip::ZipArchive::new(reader).map_err(|e| format!("The download is not a zip file: {e}"))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| format!("The download could not be read: {e}"))?;
        // `enclosed_name` rather than `name`: it refuses an entry whose path
        // escapes the archive root. Nothing is unpacked to disk here, so a
        // traversal could not reach anything — but comparing against a name
        // that has already been rejected as unsafe is a habit worth keeping.
        let Some(path) = entry.enclosed_name() else {
            continue;
        };
        if path.file_name().is_some_and(|n| n == wanted) {
            let size = entry.size();
            return read_capped(&mut entry, size);
        }
    }
    Err(format!("The download does not contain {wanted}."))
}

/// Lift one file out of a gzipped tar.
fn binary_from_tar_gz(bytes: &[u8], wanted: &str) -> Result<Vec<u8>, String> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|e| format!("The download is not a tar archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("The download could not be read: {e}"))?;
        let size = entry.header().size().unwrap_or(0);
        let path = entry
            .path()
            .map_err(|e| format!("The download holds an unreadable file name: {e}"))?
            .into_owned();
        if path.file_name().is_some_and(|n| n == wanted) {
            return read_capped(&mut entry, size);
        }
    }
    Err(format!("The download does not contain {wanted}."))
}

/// Read an archive member, refusing one that claims to be absurdly large.
///
/// The declared size is checked *before* allocating and the read is capped
/// again afterwards, because a header can lie: the point of the cap is that a
/// hostile or corrupt archive cannot make Umber allocate until it dies.
fn read_capped(reader: &mut impl Read, declared: u64) -> Result<Vec<u8>, String> {
    if declared > MAX_BINARY {
        return Err("The download claims to hold a file far larger than Umber.".to_string());
    }
    let mut out = Vec::with_capacity(declared.min(64 * 1024 * 1024) as usize);
    reader
        .take(MAX_BINARY + 1)
        .read_to_end(&mut out)
        .map_err(|e| format!("The download could not be unpacked: {e}"))?;
    if out.len() as u64 > MAX_BINARY {
        return Err("The download unpacks to far more than Umber's own size.".to_string());
    }
    Ok(out)
}

/// Write the package somewhere the installer can reach it, and start Umber's
/// own updater over it.
///
/// **`msiexec` is no longer run from here and no longer shows its interface.**
/// It used to be given the package and left to it, which meant somebody who
/// asked Umber for an update watched a Windows installer they had not asked for
/// and then had to start Umber again themselves. `installwin::spawn` puts a
/// copy of this executable in the same directory and starts it with
/// `--install-update`; that process draws Umber's own window, waits for this
/// one to close, runs `msiexec` silently and starts the new build. See
/// [`super::installer`] for why it has to be a second process.
///
/// What has not changed: the package is written first and the installer owns
/// Program Files. Umber still does not touch those files itself.
fn hand_to_msiexec(asset_name: &str, bytes: &[u8], version: &str) -> Result<(), String> {
    // The asset's own name, with anything that is not part of a file name
    // stripped. It comes from the release API rather than from the user, but it
    // is about to become a path and a command-line argument.
    let name: String = asset_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let name = if name.ends_with(".msi") {
        name
    } else {
        "umber-update.msi".to_string()
    };

    let path = std::env::temp_dir().join(name);
    std::fs::write(&path, bytes).map_err(|e| {
        format!(
            "Umber could not write the installer to {}: {e}",
            path.display()
        )
    })?;

    super::installwin::spawn(&path, version).map_err(|e| {
        format!(
            "{e}\n\n\
             The package was saved to {} and can be installed by opening it.",
            path.display(),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own, removed on the way out.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            // Process id as well as name: the name alone collides between
            // concurrent worktrees, and the wipe below then takes another
            // run's scratch with it.
            let dir = std::env::temp_dir()
                .join(format!("umber-update-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create the scratch directory");
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_suffix_is_added_to_the_whole_name_not_swapped_for_the_extension() {
        // `with_extension` would give `umber.new`, losing the `.exe` Windows
        // needs to run the file and colliding with any other `umber.*` beside
        // it.
        assert_eq!(
            suffixed(Path::new("/a/umber.exe"), ".new"),
            PathBuf::from("/a/umber.exe.new"),
        );
        assert_eq!(
            suffixed(Path::new("/a/umber"), ".old"),
            PathBuf::from("/a/umber.old"),
        );
    }

    #[test]
    fn a_swap_replaces_the_file_and_leaves_nothing_half_written() {
        let scratch = Scratch::new("swap");
        let target = scratch.path("umber.exe");
        std::fs::write(&target, b"old build").expect("write the old build");

        swap_in(&target, b"new build").expect("the swap succeeds");

        assert_eq!(std::fs::read(&target).expect("read back"), b"new build");
        assert!(
            !suffixed(&target, ".new").exists(),
            "the staging file must not survive a successful swap",
        );
        // On Windows the displaced file is left for the next start to sweep,
        // because a running binary cannot be deleted. Everywhere else the
        // rename replaced it outright.
        if !cfg!(windows) {
            assert!(!suffixed(&target, ".old").exists());
        }
    }

    #[test]
    fn a_swap_over_a_leftover_from_a_previous_update_still_works() {
        // Windows cannot rename onto an existing name, so an `umber.exe.old`
        // that the last start failed to remove would otherwise stop every
        // future update.
        let scratch = Scratch::new("leftover");
        let target = scratch.path("umber.exe");
        std::fs::write(&target, b"old build").expect("write the old build");
        std::fs::write(suffixed(&target, ".old"), b"older still").expect("write the leftover");

        swap_in(&target, b"new build").expect("the swap succeeds");
        assert_eq!(std::fs::read(&target).expect("read back"), b"new build");
    }

    #[test]
    fn a_zip_gives_up_its_binary_and_says_so_when_it_has_none() {
        let mut buf = Vec::new();
        {
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let stored: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            zip.start_file("umber-1.0.0-x86_64-pc-windows-msvc/README.md", stored)
                .expect("start README");
            std::io::Write::write_all(&mut zip, b"not the binary").expect("write README");
            zip.start_file("umber-1.0.0-x86_64-pc-windows-msvc/umber.exe", stored)
                .expect("start the binary");
            std::io::Write::write_all(&mut zip, b"MZ the new build").expect("write the binary");
            zip.finish().expect("finish the zip");
        }

        assert_eq!(
            binary_from_zip(&buf, "umber.exe").expect("the binary is found"),
            b"MZ the new build",
        );
        // A release that shipped the wrong archive must be a refusal, not an
        // empty file written over the running one.
        assert!(binary_from_zip(&buf, "umber").is_err());
        assert!(binary_from_zip(b"not a zip at all", "umber.exe").is_err());
    }

    #[test]
    fn a_tarball_gives_up_its_binary_and_says_so_when_it_has_none() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut tar = tar::Builder::new(&mut gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, "umber-1.0.0/LICENSE", &b"GPL3"[..])
                .expect("append the licence");
            let payload = b"\x7fELF the new build";
            let mut header = tar::Header::new_gnu();
            header.set_size(payload.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, "umber-1.0.0/umber", &payload[..])
                .expect("append the binary");
            tar.finish().expect("finish the tar");
        }
        let bytes = gz.finish().expect("finish the gzip");

        assert_eq!(
            binary_from_tar_gz(&bytes, "umber").expect("the binary is found"),
            b"\x7fELF the new build",
        );
        assert!(binary_from_tar_gz(&bytes, "umber.exe").is_err());
        assert!(binary_from_tar_gz(b"not a tarball", "umber").is_err());
    }

    #[test]
    fn a_managed_installation_is_refused_even_with_bytes_in_hand() {
        // The last guard. Everything upstream declines to download for these,
        // and this is what makes a mistake upstream a refusal rather than a
        // package manager's files being overwritten.
        for kind in [
            InstallKind::Managed(super::super::install::Manager::Dpkg),
            InstallKind::Managed(super::super::install::Manager::Flatpak),
            InstallKind::Unknown,
        ] {
            assert!(apply(&kind, "umber-1.0.0-x64.msi", b"anything", "1.0.0", &|_| {}).is_err());
        }
    }

    #[test]
    fn a_portable_update_names_its_two_stages_in_order() {
        // The bar's labels come from here, so the order they arrive in is the
        // order they happen in — and a refusal must report neither, rather than
        // announcing an unpack it never began.
        use std::cell::RefCell;
        let seen = RefCell::new(Vec::new());
        // A zip with nothing in it: far enough to report the unpack, not far
        // enough to touch a file.
        let mut buf = Vec::new();
        {
            let zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            zip.finish().expect("finish the zip");
        }
        assert!(
            apply(
                &InstallKind::Portable,
                "umber-1.0.0.zip",
                &buf,
                "1.0.0",
                &|stage| {
                    seen.borrow_mut().push(stage);
                }
            )
            .is_err(),
            "an archive with no binary in it is a refusal",
        );
        assert_eq!(seen.into_inner(), vec![Stage::Unpacking]);
    }
}
