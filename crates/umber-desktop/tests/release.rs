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
