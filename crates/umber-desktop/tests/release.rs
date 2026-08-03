//! Guards on the things a release depends on being true.
//!
//! `CHANGELOG.md` is not documentation that can be left to rot: the release
//! workflow publishes the section for the tag being built, verbatim, as the
//! GitHub release's notes. A missing section is therefore a release with no
//! notes, and a stale one is worse — it describes the wrong build. Both are
//! discovered at tag time, when the tag is already pushed, unless something
//! catches them earlier. This is that something, and it runs on every CI push.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is crates/umber-desktop.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the workspace root is two levels up")
        .to_path_buf()
}

fn changelog() -> String {
    let path = repo_root().join("CHANGELOG.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The body of one version's section: everything between its heading and the
/// next `## `, or the end of the file.
///
/// Shared with `tools/release.ps1` and `tools/release.sh` by convention rather
/// than by code — they are a different language and cannot call this — so the
/// rule is deliberately the simplest one that can be stated in a sentence.
fn section(text: &str, version: &str) -> Option<String> {
    let mut lines = text.lines();
    let heading = lines.by_ref().find(|line| {
        line.strip_prefix("## ")
            .is_some_and(|rest| rest == version || rest.starts_with(&format!("{version} ")))
    })?;
    let _ = heading;
    let body: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    Some(body.join("\n").trim().to_string())
}

#[test]
fn the_changelog_describes_this_version() {
    let version = env!("CARGO_PKG_VERSION");
    let text = changelog();
    let body = section(&text, version).unwrap_or_else(|| {
        panic!(
            "CHANGELOG.md has no `## {version}` section.\n\
             The version in Cargo.toml was bumped without writing the notes for \
             it, or the notes were written under a different heading. The release \
             workflow publishes that section as the release's notes, so there is \
             nothing to publish."
        )
    });
    assert!(
        !body.is_empty(),
        "the `## {version}` section of CHANGELOG.md is empty"
    );
    assert!(
        body.lines().any(|l| l.trim_start().starts_with("- ")),
        "the `## {version}` section of CHANGELOG.md has no bullet points, and \
         the release notes are supposed to be a list of what the release brings"
    );
}

/// The newest section must be the one being built. A release cut from a
/// changelog whose top entry is the *previous* version publishes the previous
/// version's notes, which is a subtler failure than publishing none.
#[test]
fn this_version_is_the_newest_entry() {
    let version = env!("CARGO_PKG_VERSION");
    let text = changelog();
    let first = text
        .lines()
        .find_map(|line| line.strip_prefix("## "))
        .expect("CHANGELOG.md has no version sections at all");
    let named = first.split_whitespace().next().unwrap_or(first);
    assert_eq!(
        named, version,
        "CHANGELOG.md's newest section is `{named}` but this build is \
         `{version}` — add the new section above the old ones"
    );
}

/// Every file the release publishes, as the README has to spell it.
///
/// **This is the third statement of these names and the other two are named
/// here**, because a download link that 404s is a worse first impression than
/// no link at all. `.github/workflows/release.yml` produces them and
/// `umber_app::update::release::wanted_asset` matches the four kinds the
/// updater fetches; this is the whole set, including the packages a package
/// manager owns and the updater therefore never asks for. Changing a name in
/// the workflow means changing it in both of the others.
///
/// `{v}` stands for the version. The spellings are deliberately inconsistent
/// because the tools are: WiX names the architecture `x64`, dpkg wants
/// `umber_0.0.5_amd64.deb`, rpm adds its release number, and the AppImage is
/// capitalised after the application rather than the crate.
const ASSETS: &[&str] = &[
    "umber-{v}-x64.msi",
    "umber-{v}-arm64.msi",
    "umber-{v}-universal-apple-darwin.tar.gz",
    "umber_{v}_amd64.deb",
    "umber_{v}_arm64.deb",
    "umber-{v}-1.x86_64.rpm",
    "umber-{v}-1.aarch64.rpm",
    "umber-bin-{v}-1-x86_64.pkg.tar.zst",
    "Umber-{v}-x86_64.AppImage",
    "Umber-{v}-aarch64.AppImage",
    "umber-{v}-x86_64.flatpak",
    "umber-{v}-x86_64-pc-windows-msvc.zip",
    "umber-{v}-aarch64-pc-windows-msvc.zip",
    "umber-{v}-x86_64-unknown-linux-gnu.tar.gz",
    "umber-{v}-aarch64-unknown-linux-gnu.tar.gz",
];

fn readme() -> String {
    let path = repo_root().join("README.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The README links to each file of *this* version's release.
///
/// A per-file download link cannot be written once and left alone: GitHub's
/// permanent `releases/latest/download/<name>` form needs the filename to be
/// the same every release, and Umber's carry the version — which is worth
/// keeping, because it is what tells you months later which build you have in
/// your downloads folder. So the links name a version, and this is what stops
/// them naming last one's. The window where they lead nowhere is between the
/// version bump and the workflow publishing, which is the same window the
/// changelog's own section has.
#[test]
fn the_readme_links_to_every_file_of_this_release() {
    let version = env!("CARGO_PKG_VERSION");
    let text = readme();
    // The sentence above the table says which release it is, and is checked
    // with the links rather than left to be noticed: a table of 0.0.6 files
    // under the words "Umber 0.0.5" is the kind of wrong that gets believed.
    assert!(
        text.contains(&format!("**Umber {version}.**")),
        "README.md's Installing section does not say `**Umber {version}.**`, so \
         it names a different release from the one it links to"
    );
    for asset in ASSETS {
        let name = asset.replace("{v}", version);
        let link =
            format!("https://github.com/Spillebulle/umber/releases/download/v{version}/{name}");
        assert!(
            text.contains(&link),
            "README.md does not link to {name}.\n\
             Expected: {link}\n\
             The version was bumped without the download table following it, or \
             the workflow's name for this asset changed and the table did not."
        );
    }
}

/// And links to *nothing else* under that path.
///
/// The half above cannot see a link that is left behind: a row for a package
/// that is no longer built, or one still naming the previous version because it
/// was missed when the others were updated, both pass it. This one reads every
/// release link the README actually has and refuses any that is not in the set.
#[test]
fn the_readme_has_no_download_link_that_is_not_a_file_of_this_release() {
    let version = env!("CARGO_PKG_VERSION");
    let text = readme();
    let wanted: Vec<String> = ASSETS.iter().map(|a| a.replace("{v}", version)).collect();
    const PREFIX: &str = "https://github.com/Spillebulle/umber/releases/download/";

    for (at, _) in text.match_indices(PREFIX) {
        let rest = &text[at + PREFIX.len()..];
        // Up to whatever ends a markdown link.
        let url: String = rest
            .chars()
            .take_while(|c| !matches!(c, ')' | ' ' | '\n' | '"' | '>'))
            .collect();
        let (tag, name) = url
            .split_once('/')
            .unwrap_or_else(|| panic!("{PREFIX}{url} names no file"));
        assert_eq!(
            tag,
            format!("v{version}"),
            "README.md links to a file of release {tag}, but this build is \
             {version}: {PREFIX}{url}"
        );
        assert!(
            wanted.iter().any(|w| w == name),
            "README.md links to `{name}`, which is not a file this release \
             publishes. Either the workflow no longer builds it, or the name is \
             wrong — the set is ASSETS in this file."
        );
    }
}

#[test]
fn a_section_stops_at_the_next_version() {
    let text = "# Changelog\n\n## 0.2.0 — later\n\n- new thing\n\n## 0.1.0\n\n- old thing\n";
    assert_eq!(section(text, "0.2.0").as_deref(), Some("- new thing"));
    assert_eq!(section(text, "0.1.0").as_deref(), Some("- old thing"));
    assert_eq!(section(text, "9.9.9"), None);
    // A prefix must not match a longer version, or 0.1.0's notes would be
    // published for 0.1.0-rc1 and for 0.1.01.
    assert_eq!(section("## 0.1.01\n\n- x\n", "0.1.0"), None);
}
