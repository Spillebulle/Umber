//! Release versions, and the tags that carry them.
//!
//! Umber's version lives in one place — `[workspace.package]` in the root
//! manifest — and a release is the annotated tag `v<version>`; `tools/release.ps1`
//! pushes nothing else, and `crates/umber-desktop/tests/release.rs` fails CI if
//! the manifest and the changelog disagree about it.
//!
//! Comparing those as text is the classic way to get an updater wrong. In every
//! lexical ordering `"0.0.10"` sorts *before* `"0.0.9"`, so the tenth patch
//! release would look older than the ninth and the update would never be
//! offered. The three parts are therefore parsed into numbers and compared as
//! numbers.

use std::fmt;

/// A `major.minor.patch` version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    // Field order *is* the comparison order: a derived `Ord` compares the
    // fields in declaration order, which is exactly semantic-version
    // precedence. Reordering these silently changes what "newer" means.
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    /// The version this build was compiled as.
    ///
    /// `CARGO_PKG_VERSION` comes from the workspace manifest, and the release
    /// tests already pin that to the changelog, so a build which passes CI
    /// cannot have a version this fails to parse. The fallback exists only so
    /// that a mistake here is a version that never offers an update rather than
    /// a panic on start-up.
    pub fn current() -> Self {
        Self::parse(env!("CARGO_PKG_VERSION")).unwrap_or(Self {
            major: 0,
            minor: 0,
            patch: 0,
        })
    }

    /// Parse `1.2.3`. Exactly three all-digit parts, and nothing else.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('.');
        let major = number(parts.next()?)?;
        let minor = number(parts.next()?)?;
        let patch = number(parts.next()?)?;
        // `1.2.3.4` is not a version this project produces, and treating it as
        // `1.2.3` would silently accept a tag nobody here wrote.
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// The version a release tag names, or `None` if the tag is not one of ours.
    ///
    /// Anything that is not exactly `v<major>.<minor>.<patch>` is **ignored
    /// rather than guessed at**. A repository accumulates tags nobody here
    /// wrote — `nightly`, `v2-beta`, a bare `0.1.0` — and misreading one as a
    /// version is how an updater comes to offer people a release that does not
    /// exist, or to decide that a build newer than anything published is out of
    /// date.
    ///
    /// A pre-release such as `v0.2.0-rc.1` is deliberately in that group. The
    /// release script never makes one, and a stable installation should not be
    /// walked onto a candidate build by an automatic check.
    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::parse(tag.strip_prefix('v')?)
    }
}

/// One part of a version.
///
/// Hand-checked rather than left to `u64::from_str`, which accepts a leading
/// `+` — so `1.+2.3` would parse — and because `split` yields an empty string
/// for the middle of `1..2`, which parses as nothing at all.
fn number(part: &str) -> Option<u64> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    part.parse().ok()
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
        }
    }

    #[test]
    fn a_version_is_compared_as_numbers_not_as_text() {
        // The whole reason this type exists: every lexical ordering puts
        // "0.0.10" before "0.0.9", so a string comparison would decide the
        // tenth patch release was older than the ninth and never offer it.
        assert!(v(0, 0, 10) > v(0, 0, 9));
        assert!("0.0.10" < "0.0.9", "the trap this guards against");
        assert!(v(0, 10, 0) > v(0, 9, 0));
        assert!(v(10, 0, 0) > v(9, 0, 0));
        // Precedence runs major, then minor, then patch.
        assert!(v(1, 0, 0) > v(0, 99, 99));
        assert!(v(0, 2, 0) > v(0, 1, 99));
        assert_eq!(v(1, 2, 3), v(1, 2, 3));
    }

    #[test]
    fn the_tags_the_release_script_pushes_parse() {
        assert_eq!(Version::from_tag("v0.0.1"), Some(v(0, 0, 1)));
        assert_eq!(Version::from_tag("v0.0.10"), Some(v(0, 0, 10)));
        assert_eq!(Version::from_tag("v12.34.56"), Some(v(12, 34, 56)));
    }

    #[test]
    fn a_tag_that_is_not_ours_is_ignored_rather_than_misparsed() {
        // Every one of these can appear in a repository that has ever had a
        // second contributor, a CI experiment or a moved tag. None of them may
        // become a version, because a version is what decides whether the user
        // is told to update.
        for tag in [
            "nightly",
            "latest",
            "v2-beta",
            "0.1.0",       // no `v`
            "v0.1",        // two parts
            "v0.1.0.1",    // four
            "v0.1.0-rc.1", // a pre-release; the script never makes one
            "v0.1.0+build",
            "vv0.1.0",
            "v0.1.x",
            "v 0.1.0",
            "v+1.0.0",
            "v-1.0.0",
            "v1..0",
            "v",
            "",
        ] {
            assert_eq!(Version::from_tag(tag), None, "{tag:?} was parsed");
        }
    }

    #[test]
    fn a_version_round_trips_through_its_own_display() {
        for version in [v(0, 0, 1), v(0, 0, 10), v(1, 20, 300)] {
            assert_eq!(Version::parse(&version.to_string()), Some(version));
        }
    }

    #[test]
    fn this_build_knows_its_own_version() {
        // If this ever fails, the manifest holds something that is not a plain
        // three-part version and the whole comparison is running on the
        // fallback — which would silently never offer an update.
        assert_eq!(
            Version::current(),
            Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version parses"),
        );
    }
}
