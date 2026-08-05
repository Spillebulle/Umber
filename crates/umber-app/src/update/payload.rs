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
