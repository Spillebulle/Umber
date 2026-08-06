//! A package carried on the end of the executable that installs it.
//!
//! `umber-setup.exe` is Umber's own binary with an MSI concatenated onto it and
//! a sixteen-byte footer saying how long the MSI is. Run with `--install` it
//! reads its own file, lifts the package back out and installs it through the
//! same window an update uses — so somebody installing Umber for the first time
//! sees Umber's interface rather than Windows Installer's.
//!
//! ```text
//! +-------------------+
//! |  umber.exe (PE)   |
//! +-------------------+
//! |  umber-x.y.z.msi  |
//! +-------------------+
//! | UMBRPKG | len u64 |   <- 16 bytes, little-endian
//! +-------------------+
//! ```
//!
//! ## Why the length is at the *end*
//!
//! Because the start is not ours. A PE file's length is whatever the linker
//! made it and it changes with every build, so an offset written down anywhere
//! but the very end would have to be patched after the fact. Reading backwards
//! from the end of the file needs to know nothing about what is in front of the
//! package — which is also what makes this survive the executable being rebuilt
//! at a different size.
//!
//! Windows loads a PE by its headers rather than by the file's length, so bytes
//! after the last section are simply ignored: `umber-setup.exe` runs as Umber
//! does. This is the same trick every self-extracting archive uses.
//!
//! ## What it is not
//!
//! **It is not a signature and must not be described as one.** The magic says
//! "something appended this deliberately", which distinguishes a payload from a
//! file that happens to end in the right eight bytes; it says nothing whatever
//! about where the package came from. Umber does not sign its releases — see
//! `update`'s standing rule — and a length check is not authenticity here any
//! more than it is on a download.
//!
//! It is also **not compression**. An MSI is already a compressed cabinet, so a
//! second pass would cost build time and startup time to save very little.

/// What marks a payload. Eight bytes, so a file ending in a plausible-looking
/// length is not mistaken for one.
const MAGIC: &[u8; 8] = b"UMBRPKG\0";

/// Magic plus a `u64`.
const FOOTER: usize = MAGIC.len() + 8;

/// The largest package this will read back out.
///
/// Umber's MSI is about ten megabytes. The bound is not about the real file: it
/// is what stops a corrupt or hostile footer claiming four exabytes and being
/// believed all the way into a slice.
const MAX_PACKAGE: u64 = 512 * 1024 * 1024;

/// The package carried by `bytes`, if there is one.
///
/// `None` for a plain executable, which is the ordinary case — `umber.exe`
/// itself is one — and for anything that does not add up. Every length here
/// comes off the end of a file somebody could have edited, so each is checked
/// against what is actually there rather than trusted.
pub fn read(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < FOOTER {
        return None;
    }
    let footer = &bytes[bytes.len() - FOOTER..];
    if &footer[..MAGIC.len()] != MAGIC {
        return None;
    }
    let len = u64::from_le_bytes(footer[MAGIC.len()..].try_into().ok()?);
    if len == 0 || len > MAX_PACKAGE {
        return None;
    }
    let len = usize::try_from(len).ok()?;
    // The package sits immediately before the footer, so the file has to be at
    // least that long *plus* whatever executable it was appended to. A claimed
    // length that would start before the beginning is a footer to disbelieve.
    let end = bytes.len().checked_sub(FOOTER)?;
    let start = end.checked_sub(len)?;
    if start == 0 {
        // A file that is *only* a package and a footer is not an installer:
        // there would be nothing to run. Refused rather than half-believed.
        return None;
    }
    Some(&bytes[start..end])
}

/// Whether the file at `path` carries a package, read from its last sixteen
/// bytes alone.
///
/// **This is what makes a binary the installer, and an argument is not.** A
/// setup executable is double-clicked, so it is launched with no command line
/// at all — `--install` only ever arrives when something spawns it deliberately,
/// which nothing does. Deciding on the payload instead means the file that
/// carries a package installs it and the file that does not runs as Umber,
/// which is the only distinction there actually is between them.
///
/// Cheap on purpose, because it runs on **every** ordinary start: a seek and a
/// sixteen-byte read, never the whole executable. [`read`] does the real
/// validation once the window is up and can say so; the worst this can do is
/// send a corrupt setup file to a window that reports the corruption, which is
/// better than the silence it replaces.
///
/// `false` for anything it cannot read. A file that will not open is not an
/// installer, and refusing to start Umber over it would be the wrong way round.
pub fn carried_by(path: &std::path::Path) -> bool {
    use std::io::{Read, Seek, SeekFrom};

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return false;
    };
    if len < FOOTER as u64 {
        return false;
    }
    if file.seek(SeekFrom::End(-(FOOTER as i64))).is_err() {
        return false;
    }
    let mut footer = [0u8; FOOTER];
    if file.read_exact(&mut footer).is_err() {
        return false;
    }
    if &footer[..MAGIC.len()] != MAGIC {
        return false;
    }
    let Ok(claimed) = footer[MAGIC.len()..].try_into().map(u64::from_le_bytes) else {
        return false;
    };
    // The same three refusals [`read`] makes, so the two cannot disagree about
    // what counts as a payload: a zero or absurd length, and one that would
    // leave no executable in front of the package.
    claimed != 0 && claimed <= MAX_PACKAGE && claimed + (FOOTER as u64) < len
}

/// Build the bytes of a setup executable.
///
/// The one place the format is written, used by `examples/make-setup.rs` and
/// read back by [`read`] — so the builder and the reader cannot drift, which is
/// the rule `docformat` states as "there must never be a second ORA reader".
pub fn append(executable: &[u8], package: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(executable.len() + package.len() + FOOTER);
    out.extend_from_slice(executable);
    out.extend_from_slice(package);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(package.len() as u64).to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch file of this test's own. Keyed by process id as well as by
    /// name: several worktrees run `cargo test` at once here, and a fixed path
    /// is the same file in every checkout.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("umber-payload-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    /// **The signal that makes a binary the installer.**
    ///
    /// Setup is double-clicked, so it is launched with no command line at all
    /// and `installer::parse` answers `None` for it. For a while that was the
    /// whole of the dispatch, so `umber-setup.exe` started as the application
    /// and the installer was unreachable — the thing this pins is that the
    /// payload, and not an argument, is what decides.
    #[test]
    fn a_file_carrying_a_package_is_recognised_by_its_last_bytes_alone() {
        let exe = b"MZ this is a program".to_vec();
        let package: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();

        let setup = scratch("setup.exe");
        std::fs::write(&setup, append(&exe, &package)).expect("write the setup file");
        assert!(carried_by(&setup), "a setup binary is not recognised");

        // And the ordinary case, which is every launch of Umber itself.
        let plain = scratch("umber.exe");
        std::fs::write(&plain, &exe).expect("write the plain file");
        assert!(!carried_by(&plain), "a plain executable looks like setup");

        // The two answers agree with `read`'s, which is what stops the cheap
        // probe and the real unpack disagreeing about what a payload is.
        assert!(read(&append(&exe, &package)).is_some());
        assert!(read(&exe).is_none());

        // A file that is only a package and a footer has no executable to run,
        // and both refuse it. Same three refusals, stated once each.
        let headless = append(b"", &package);
        let headless_path = scratch("headless.exe");
        std::fs::write(&headless_path, &headless).expect("write the headless file");
        assert!(!carried_by(&headless_path));
        assert!(read(&headless).is_none());

        // A length nothing could satisfy, which is the hostile-footer case.
        let mut liar = append(&exe, &package);
        let n = liar.len();
        liar[n - 8..].copy_from_slice(&u64::MAX.to_le_bytes());
        let liar_path = scratch("liar.exe");
        std::fs::write(&liar_path, &liar).expect("write the liar file");
        assert!(!carried_by(&liar_path));
        assert!(read(&liar).is_none());

        let _ = std::fs::remove_dir_all(setup.parent().expect("the scratch directory"));
    }

    #[test]
    fn a_package_comes_back_exactly_as_it_went_in() {
        let exe = b"MZ this is a program".to_vec();
        let package: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let setup = append(&exe, &package);
        assert_eq!(read(&setup), Some(package.as_slice()));
        // And the executable in front of it is untouched, which is what lets
        // the setup binary still run.
        assert_eq!(&setup[..exe.len()], exe.as_slice());
    }

    /// The ordinary case: `umber.exe` has no payload and must not be read as
    /// though it had one.
    #[test]
    fn a_plain_executable_carries_nothing() {
        assert_eq!(read(b"MZ an ordinary program"), None);
        assert_eq!(read(b""), None);
        assert_eq!(read(&[0u8; 8]), None);
    }

    /// **Every length is checked against the file that is actually there.**
    /// The footer is the last thing anybody could edit and the first thing that
    /// would be, so a claim that does not fit is refused rather than sliced.
    #[test]
    fn a_footer_that_does_not_add_up_is_refused() {
        let exe = b"MZ program".to_vec();
        let package = b"package".to_vec();
        let good = append(&exe, &package);

        // Longer than the file it is on the end of.
        let mut lying = good.clone();
        let n = lying.len();
        lying[n - 8..].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(read(&lying), None);

        // Long enough to swallow the executable as well, leaving nothing to
        // run — which is not an installer whatever else it is.
        let mut greedy = good.clone();
        let n = greedy.len();
        let all = (greedy.len() - FOOTER) as u64;
        greedy[n - 8..].copy_from_slice(&all.to_le_bytes());
        assert_eq!(read(&greedy), None);

        // Empty.
        let mut empty = good.clone();
        let n = empty.len();
        empty[n - 8..].copy_from_slice(&0u64.to_le_bytes());
        assert_eq!(read(&empty), None);

        // Absurd, and refused before it reaches a `usize` conversion that
        // would succeed on a 64-bit machine.
        let mut huge = good.clone();
        let n = huge.len();
        huge[n - 8..].copy_from_slice(&(MAX_PACKAGE + 1).to_le_bytes());
        assert_eq!(read(&huge), None);
    }

    /// The magic is what tells a payload from a file that happens to end in
    /// eight bytes that look like a length.
    #[test]
    fn a_file_that_merely_ends_in_a_number_is_not_a_payload() {
        let mut lookalike = b"MZ program and some data".to_vec();
        lookalike.extend_from_slice(b"NOTMAGIC");
        lookalike.extend_from_slice(&4u64.to_le_bytes());
        assert_eq!(read(&lookalike), None);
    }
}
