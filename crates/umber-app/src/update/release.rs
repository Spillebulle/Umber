//! What GitHub says about the releases of this repository, and which of a
//! release's assets belongs to this machine.
//!
//! The reply is read with serde rather than by scanning for substrings, and
//! every field Umber does not use is dropped on the floor — a release payload
//! carries a hundred keys and Umber needs five.
//!
//! **Nothing here builds a download URL.** The address of an asset is taken
//! from the reply verbatim, and checked to be `https`. Constructing one from a
//! version number would mean Umber deciding where to fetch a binary from, which
//! is precisely the decision that should belong to the release the API just
//! described.

use super::install::{Arch, InstallKind, Os};
use super::version::Version;
use serde::Deserialize;

/// The repository, as shown to the user.
pub const REPOSITORY: &str = "https://github.com/Spillebulle/umber";

/// Where somebody goes to fetch a build by hand — which is the answer for
/// every installation Umber may not update itself.
pub const RELEASES_PAGE: &str = "https://github.com/Spillebulle/umber/releases";

/// The releases of this repository, newest first.
///
/// The list endpoint rather than `/releases/latest`, deliberately. `latest`
/// applies GitHub's own idea of which release is newest, which is by
/// publication date and takes no view on the tag; asking for the list lets
/// [`newest`] ignore every tag that is not `v<semver>` — including tags a
/// future maintainer may push for something else entirely — and then compare
/// what is left as versions.
pub const API: &str = "https://api.github.com/repos/Spillebulle/umber/releases?per_page=20";

/// One file attached to a release.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    /// The size GitHub recorded when the asset was uploaded. The download is
    /// checked against it — see `super::fetch`.
    pub size: u64,
    pub browser_download_url: String,
}

impl Asset {
    /// Whether this is an address Umber is willing to fetch from.
    ///
    /// Umber does not sign its releases, so the transport is the whole of the
    /// guarantee: TLS to a host GitHub controls, and then a length that matches
    /// what the API said. A plain-`http` URL would throw away even that, so an
    /// asset carrying one is treated as though it were not there.
    pub fn is_fetchable(&self) -> bool {
        self.browser_download_url.starts_with("https://")
    }
}

/// A release, reduced to what an update needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    /// The release's own page, for the reader who would rather do it by hand.
    pub page: String,
    /// `CHANGELOG.md`'s section for this version, which is what the workflow
    /// publishes as the release body.
    pub notes: String,
    pub assets: Vec<Asset>,
}

impl Release {
    /// The asset this machine wants, if the release carries one.
    ///
    /// `None` means the release exists but has nothing for this combination —
    /// a Flatpak-only rebuild, an architecture that was not published that
    /// time, or an installation kind Umber does not update itself. The caller
    /// says so rather than inventing a name.
    pub fn asset_for(&self, kind: &InstallKind, os: Os, arch: Arch) -> Option<&Asset> {
        let wanted = wanted_asset(kind, os, arch)?;
        self.assets
            .iter()
            .find(|asset| asset.is_fetchable() && wanted.matches(&asset.name))
    }
}

/// The shape of file an installation is updated from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    /// `umber-<version>-<arch>.msi`, handed to `msiexec`.
    Msi { suffix: &'static str },
    /// `umber-<version>-<target>.zip`, holding `umber.exe`.
    WindowsZip { suffix: &'static str },
    /// `umber-<version>-<target>.tar.gz`, holding `umber`.
    Tarball { suffix: &'static str },
    /// `Umber-<version>-<arch>.AppImage` — the whole application, one file.
    AppImage { suffix: &'static str },
}

impl AssetKind {
    /// Whether an asset name is the one wanted.
    ///
    /// Matched on the suffix the release workflow builds the name from, not on
    /// the whole name: the version is in the middle of every one of them, and
    /// writing it out here would mean this code deciding what the next release
    /// is called.
    pub fn matches(self, name: &str) -> bool {
        let suffix = match self {
            Self::Msi { suffix }
            | Self::WindowsZip { suffix }
            | Self::Tarball { suffix }
            | Self::AppImage { suffix } => suffix,
        };
        name.ends_with(suffix)
    }
}

/// Which asset an installation of this kind, on this platform, is updated from.
///
/// The suffixes are the ones `.github/workflows/release.yml` produces:
/// `umber-${version}-${target}.{zip,tar.gz}` from the Stage step,
/// `umber-${version}-${arch}.msi` from Build MSI, and
/// `Umber-${version}-${arch}.AppImage` from `packaging/linux/build-packages.sh`.
/// If a name there changes, this is the other half of the change.
pub fn wanted_asset(kind: &InstallKind, os: Os, arch: Arch) -> Option<AssetKind> {
    match kind {
        InstallKind::Msi => Some(AssetKind::Msi {
            // The MSI is named for the WiX architecture, which spells x86-64
            // `x64`, not for the Rust target triple.
            suffix: match arch {
                Arch::X86_64 => "-x64.msi",
                Arch::Aarch64 => "-arm64.msi",
            },
        }),

        InstallKind::AppImage(_) => Some(AssetKind::AppImage {
            suffix: match arch {
                Arch::X86_64 => "-x86_64.AppImage",
                Arch::Aarch64 => "-aarch64.AppImage",
            },
        }),

        InstallKind::Portable => match os {
            Os::Windows => Some(AssetKind::WindowsZip {
                suffix: match arch {
                    Arch::X86_64 => "-x86_64-pc-windows-msvc.zip",
                    Arch::Aarch64 => "-aarch64-pc-windows-msvc.zip",
                },
            }),
            // One universal binary covers both slices, so the architecture does
            // not appear in the name. See the release workflow's build matrix
            // for why there is no per-architecture macOS job.
            Os::Mac => Some(AssetKind::Tarball {
                suffix: "-universal-apple-darwin.tar.gz",
            }),
            Os::Linux => Some(AssetKind::Tarball {
                suffix: match arch {
                    Arch::X86_64 => "-x86_64-unknown-linux-gnu.tar.gz",
                    Arch::Aarch64 => "-aarch64-unknown-linux-gnu.tar.gz",
                },
            }),
        },

        // A package manager's, or somewhere Umber cannot place itself. There is
        // no asset to want: the answer is the releases page and the manager's
        // own command.
        InstallKind::Managed(_) | InstallKind::Unknown => None,
    }
}

/// What the API returns, as far as Umber reads it.
#[derive(Deserialize)]
struct RawRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    /// A draft is visible only to people who can edit the repository, and is
    /// not something to offer anybody.
    #[serde(default)]
    draft: bool,
    /// Belt and braces: [`Version::from_tag`] already refuses a pre-release
    /// tag, but a release marked pre-release under an ordinary tag is still not
    /// one to walk a stable installation onto.
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

/// The newest published release in an API reply, ignoring everything that is
/// not a plain `v<semver>` tag.
///
/// Returns `None` when the reply holds no release Umber recognises, which is
/// the honest answer for a repository whose first release has not been cut.
pub fn newest(json: &str) -> Result<Option<Release>, serde_json::Error> {
    let raw: Vec<RawRelease> = serde_json::from_str(json)?;
    Ok(raw
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease)
        .filter_map(|r| {
            let version = Version::from_tag(&r.tag_name)?;
            Some(Release {
                version,
                page: if r.html_url.is_empty() {
                    RELEASES_PAGE.to_string()
                } else {
                    r.html_url
                },
                notes: r.body.unwrap_or_default(),
                tag: r.tag_name,
                assets: r.assets,
            })
        })
        // Newest by version, not by the order the API happened to list them:
        // GitHub sorts by creation date, and a release re-cut after a bad
        // workflow run is created later than the version above it.
        .max_by_key(|r| r.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A reply shaped like GitHub's, cut down to the fields Umber reads plus a
    /// few it must ignore. Everything awkward is deliberately in here: a draft,
    /// a pre-release, a tag that is not ours, an out-of-order list and an asset
    /// on plain http.
    const FIXTURE: &str = r#"[
      {
        "tag_name": "v0.0.9",
        "name": "Umber v0.0.9",
        "draft": false,
        "prerelease": false,
        "html_url": "https://github.com/Spillebulle/umber/releases/tag/v0.0.9",
        "body": "Added\n- Nothing yet.\n",
        "author": { "login": "Spillebulle", "id": 1 },
        "assets": [
          {
            "name": "umber-0.0.9-x64.msi",
            "size": 41,
            "content_type": "application/octet-stream",
            "browser_download_url": "https://github.com/Spillebulle/umber/releases/download/v0.0.9/umber-0.0.9-x64.msi"
          }
        ]
      },
      {
        "tag_name": "v0.0.10",
        "draft": false,
        "prerelease": false,
        "html_url": "https://github.com/Spillebulle/umber/releases/tag/v0.0.10",
        "body": "The tenth.\n",
        "assets": [
          { "name": "umber-0.0.10-x64.msi", "size": 10, "browser_download_url": "https://github.com/x/umber-0.0.10-x64.msi" },
          { "name": "umber-0.0.10-arm64.msi", "size": 11, "browser_download_url": "https://github.com/x/umber-0.0.10-arm64.msi" },
          { "name": "umber-0.0.10-x86_64-pc-windows-msvc.zip", "size": 12, "browser_download_url": "https://github.com/x/umber-0.0.10-x86_64-pc-windows-msvc.zip" },
          { "name": "umber-0.0.10-aarch64-pc-windows-msvc.zip", "size": 13, "browser_download_url": "https://github.com/x/umber-0.0.10-aarch64-pc-windows-msvc.zip" },
          { "name": "umber-0.0.10-universal-apple-darwin.tar.gz", "size": 14, "browser_download_url": "https://github.com/x/umber-0.0.10-universal-apple-darwin.tar.gz" },
          { "name": "umber-0.0.10-x86_64-unknown-linux-gnu.tar.gz", "size": 15, "browser_download_url": "https://github.com/x/umber-0.0.10-x86_64-unknown-linux-gnu.tar.gz" },
          { "name": "umber-0.0.10-aarch64-unknown-linux-gnu.tar.gz", "size": 16, "browser_download_url": "https://github.com/x/umber-0.0.10-aarch64-unknown-linux-gnu.tar.gz" },
          { "name": "Umber-0.0.10-x86_64.AppImage", "size": 17, "browser_download_url": "https://github.com/x/Umber-0.0.10-x86_64.AppImage" },
          { "name": "Umber-0.0.10-aarch64.AppImage", "size": 18, "browser_download_url": "https://github.com/x/Umber-0.0.10-aarch64.AppImage" },
          { "name": "umber_0.0.10_amd64.deb", "size": 19, "browser_download_url": "https://github.com/x/umber_0.0.10_amd64.deb" },
          { "name": "umber-0.0.10-1.x86_64.rpm", "size": 20, "browser_download_url": "https://github.com/x/umber-0.0.10-1.x86_64.rpm" },
          { "name": "umber-0.0.10-x86_64.flatpak", "size": 21, "browser_download_url": "https://github.com/x/umber-0.0.10-x86_64.flatpak" },
          { "name": "umber-0.0.10-1-x86_64.pkg.tar.zst", "size": 22, "browser_download_url": "https://github.com/x/umber-0.0.10-1-x86_64.pkg.tar.zst" }
        ]
      },
      {
        "tag_name": "v0.1.0",
        "draft": true,
        "prerelease": false,
        "html_url": "https://github.com/Spillebulle/umber/releases/tag/v0.1.0",
        "assets": []
      },
      {
        "tag_name": "v0.2.0-rc.1",
        "draft": false,
        "prerelease": true,
        "html_url": "https://github.com/Spillebulle/umber/releases/tag/v0.2.0-rc.1",
        "assets": []
      },
      {
        "tag_name": "nightly",
        "draft": false,
        "prerelease": false,
        "html_url": "https://github.com/Spillebulle/umber/releases/tag/nightly",
        "assets": []
      },
      {
        "tag_name": "v9.9.9",
        "draft": false,
        "prerelease": true,
        "html_url": "https://github.com/Spillebulle/umber/releases/tag/v9.9.9",
        "assets": []
      }
    ]"#;

    fn newest_of(json: &str) -> Release {
        newest(json)
            .expect("the fixture parses")
            .expect("the fixture holds a release")
    }

    #[test]
    fn the_newest_release_is_found_by_version_not_by_list_order() {
        // 0.0.9 is listed first and 0.0.10 second, exactly as a repository that
        // re-cut a release would report them. Text ordering would also pick the
        // wrong one of these two.
        let release = newest_of(FIXTURE);
        assert_eq!(release.tag, "v0.0.10");
        assert_eq!(release.version, Version::parse("0.0.10").expect("parses"));
        assert!(release.notes.contains("The tenth."));
        assert_eq!(
            release.page,
            "https://github.com/Spillebulle/umber/releases/tag/v0.0.10",
        );
    }

    #[test]
    fn drafts_prereleases_and_foreign_tags_are_all_ignored() {
        // v0.1.0 is a draft, v0.2.0-rc.1 and v9.9.9 are pre-releases, and
        // `nightly` is not a version at all. Every one of them is newer than
        // v0.0.10 by some reading, and none may be offered.
        let release = newest_of(FIXTURE);
        assert_eq!(release.tag, "v0.0.10");
    }

    #[test]
    fn a_repository_with_no_release_umber_recognises_yields_nothing() {
        assert_eq!(newest("[]").expect("empty list parses"), None);
        let only_junk = r#"[{"tag_name":"nightly","draft":false,"prerelease":false,"assets":[]}]"#;
        assert_eq!(newest(only_junk).expect("parses"), None);
    }

    #[test]
    fn a_reply_that_is_not_a_release_list_is_an_error_not_a_guess() {
        // What GitHub sends for a rate limit or a missing repository: an object
        // with a message, not an array. Reading that as "no update" would be
        // indistinguishable from being up to date.
        let refusal = r#"{"message":"API rate limit exceeded","documentation_url":"https://..."}"#;
        assert!(newest(refusal).is_err());
        assert!(newest("not json at all").is_err());
    }

    /// Every installation kind, on every platform it can occur on, against the
    /// full asset list of a real release.
    #[test]
    fn each_platform_and_architecture_picks_its_own_asset() {
        let release = newest_of(FIXTURE);
        let appimage = InstallKind::AppImage(PathBuf::from("/home/a/Umber.AppImage"));
        let cases: [(&InstallKind, Os, Arch, &str); 9] = [
            (
                &InstallKind::Msi,
                Os::Windows,
                Arch::X86_64,
                "umber-0.0.10-x64.msi",
            ),
            (
                &InstallKind::Msi,
                Os::Windows,
                Arch::Aarch64,
                "umber-0.0.10-arm64.msi",
            ),
            (
                &InstallKind::Portable,
                Os::Windows,
                Arch::X86_64,
                "umber-0.0.10-x86_64-pc-windows-msvc.zip",
            ),
            (
                &InstallKind::Portable,
                Os::Windows,
                Arch::Aarch64,
                "umber-0.0.10-aarch64-pc-windows-msvc.zip",
            ),
            // Both macOS architectures take the one universal binary.
            (
                &InstallKind::Portable,
                Os::Mac,
                Arch::X86_64,
                "umber-0.0.10-universal-apple-darwin.tar.gz",
            ),
            (
                &InstallKind::Portable,
                Os::Mac,
                Arch::Aarch64,
                "umber-0.0.10-universal-apple-darwin.tar.gz",
            ),
            (
                &InstallKind::Portable,
                Os::Linux,
                Arch::X86_64,
                "umber-0.0.10-x86_64-unknown-linux-gnu.tar.gz",
            ),
            (
                &appimage,
                Os::Linux,
                Arch::X86_64,
                "Umber-0.0.10-x86_64.AppImage",
            ),
            (
                &appimage,
                Os::Linux,
                Arch::Aarch64,
                "Umber-0.0.10-aarch64.AppImage",
            ),
        ];
        for (kind, os, arch, expected) in cases {
            let asset = release
                .asset_for(kind, os, arch)
                .unwrap_or_else(|| panic!("{kind:?} on {os:?}/{arch:?} found nothing"));
            assert_eq!(asset.name, expected, "{kind:?} on {os:?}/{arch:?}");
        }
    }

    #[test]
    fn the_linux_arm_tarball_is_not_confused_with_the_windows_one() {
        // Both end in the architecture; only the full target triple separates
        // them, which is why the suffixes carry it.
        let release = newest_of(FIXTURE);
        let asset = release
            .asset_for(&InstallKind::Portable, Os::Linux, Arch::Aarch64)
            .expect("an aarch64 Linux tarball");
        assert_eq!(asset.name, "umber-0.0.10-aarch64-unknown-linux-gnu.tar.gz");
    }

    #[test]
    fn a_package_managers_installation_is_offered_no_asset_at_all() {
        // The .deb, the .rpm, the Flatpak bundle and the Arch package are all
        // in the fixture's asset list. None of them may be selected: fetching
        // one would be the first half of writing over a package manager's
        // files.
        let release = newest_of(FIXTURE);
        for kind in [
            InstallKind::Managed(super::super::install::Manager::Dpkg),
            InstallKind::Managed(super::super::install::Manager::Rpm),
            InstallKind::Managed(super::super::install::Manager::Pacman),
            InstallKind::Managed(super::super::install::Manager::Flatpak),
            InstallKind::Unknown,
        ] {
            assert_eq!(
                release.asset_for(&kind, Os::Linux, Arch::X86_64),
                None,
                "{kind:?}",
            );
        }
    }

    #[test]
    fn an_asset_that_is_not_on_https_is_treated_as_absent() {
        // Umber does not sign its releases, so the transport is the whole of
        // the guarantee. An asset offered over plain http throws that away.
        let json = r#"[{
          "tag_name": "v1.0.0", "draft": false, "prerelease": false,
          "html_url": "https://github.com/Spillebulle/umber/releases/tag/v1.0.0",
          "assets": [
            { "name": "umber-1.0.0-x64.msi", "size": 1,
              "browser_download_url": "http://github.com/x/umber-1.0.0-x64.msi" }
          ]
        }]"#;
        let release = newest_of(json);
        assert_eq!(
            release.asset_for(&InstallKind::Msi, Os::Windows, Arch::X86_64),
            None,
        );
    }

    #[test]
    fn a_release_with_no_asset_for_this_machine_is_not_an_error() {
        // A rebuild that published only the Flatpak, say. The release is real;
        // there is simply nothing here to install from, and the caller says so
        // rather than inventing a file name.
        let json = r#"[{
          "tag_name": "v1.0.0", "draft": false, "prerelease": false,
          "html_url": "https://github.com/Spillebulle/umber/releases/tag/v1.0.0",
          "assets": [
            { "name": "umber-1.0.0-x86_64.flatpak", "size": 1,
              "browser_download_url": "https://github.com/x/umber-1.0.0-x86_64.flatpak" }
          ]
        }]"#;
        let release = newest_of(json);
        assert_eq!(release.version, Version::parse("1.0.0").expect("parses"));
        assert_eq!(
            release.asset_for(&InstallKind::Portable, Os::Windows, Arch::X86_64),
            None,
        );
    }

    #[test]
    fn a_release_body_that_is_absent_reads_as_empty_rather_than_failing() {
        // GitHub sends `"body": null` for a release published with no notes.
        // The workflow always has notes, but the reader must not depend on a
        // field it does not control.
        let json = r#"[{"tag_name":"v1.0.0","draft":false,"prerelease":false,
                        "html_url":"https://x","body":null,"assets":[]}]"#;
        assert_eq!(newest_of(json).notes, "");
    }
}
