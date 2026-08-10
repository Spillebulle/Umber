//! Umber as a thumbnailer, for the desktops that ask for one on a command line.
//!
//! ```text
//! umber --thumbnail <input> <output.png> <size>
//! ```
//!
//! This is the freedesktop thumbnailer contract: a `.thumbnailer` file under
//! `share/thumbnailers/` names a command with `%i` (the document), `%o` (the
//! PNG to write) and `%s` (the largest edge wanted), and the desktop runs it
//! once per file and caches the result. GNOME's Nautilus, Thunar, Nemo, Caja
//! and PCManFM all read it, so one file covers the desktops people use.
//!
//! **A mode of the one binary rather than a second executable**, which is the
//! shape this crate already keeps: `--crash-report` makes it the crash
//! reporter and `--install-update` makes it the installer's helper. A separate
//! thumbnailer binary would be a second thing for every package to install, a
//! second thing for the release workflow to build for five targets, and a
//! second thing to forget. Nothing GPU is touched on this path — it returns
//! long before the event loop — so the cost is the process start.
//!
//! **Windows does not come through here.** Explorer has no command-line
//! thumbnailer contract at all; it wants an in-process COM server, which is
//! `umber-shellext`. Both call the same `docimport::preview`, which is the
//! whole reason that module has no platform in it.
//!
//! **macOS does not either, and cannot yet.** Quick Look wants an `.appex`
//! extension inside a signed `.app` bundle, and the macOS release is a bare
//! binary in a tarball. See `docs/thumbnails.md`.

use std::path::{Path, PathBuf};

use umber_core::docimport::preview;
use umber_core::export::{ExportFormat, ExportOptions};

/// The flag that turns this executable into the thumbnailer.
///
/// Read back out of this constant by the packaging guard in `taskbar`, so the
/// `.thumbnailer` entry and the parser cannot drift apart.
pub const FLAG: &str = "--thumbnail";

/// What a thumbnail request is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub input: PathBuf,
    pub output: PathBuf,
    /// The largest edge the desktop wants. Zero means "no bound", which the
    /// specification does not produce but a hand-typed command line can.
    pub size: u32,
}

/// Read a thumbnail request off the command line, or answer `None`.
///
/// A pure function of the arguments, like [`crate::crash::parse_args`] and
/// [`crate::update::install::detect`], so the whole of it is tested without
/// writing a file.
///
/// **A malformed request answers `None` and Umber starts normally**, which is
/// the rule the rest of the command line already follows. The alternative —
/// refusing to start a painting application because a desktop passed a size it
/// could not parse — is the far worse failure.
pub fn job<I: IntoIterator<Item = String>>(args: I) -> Option<Request> {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg != FLAG {
            continue;
        }
        let input = args.next()?;
        let output = args.next()?;
        // The size is last and optional: `%s` is always sent by a real
        // desktop, and a command typed by hand to see what a document looks
        // like should not need it.
        let size = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        return Some(Request {
            input: PathBuf::from(input),
            output: PathBuf::from(output),
            size,
        });
    }
    None
}

/// Write the thumbnail, or say why not.
///
/// The error goes to stderr and the process exits non-zero, which is what the
/// specification asks for and what stops a desktop caching a failure as though
/// it were a picture.
pub fn run(request: &Request) -> Result<(), String> {
    let preview = preview::from_path(&request.input)
        .map_err(|e| format!("{}: {e}", request.input.display()))?
        .fit_within(request.size);

    // The PNG encoder the export already uses, rather than a second one. It
    // takes straight-alpha sRGB RGBA8, which is exactly what a `Preview` is.
    let png = umber_core::export::encode(
        &preview.rgba,
        preview.size.x,
        preview.size.y,
        &ExportOptions {
            format: ExportFormat::Png,
            ..Default::default()
        },
    )
    .map_err(|e| format!("{}: {e}", request.input.display()))?;

    write_atomically(&request.output, &png)
        .map_err(|e| format!("{}: {e}", request.output.display()))
}

/// Write beside the target and rename.
///
/// The thumbnail directory is shared and a desktop may be reading it while this
/// runs, so a half-written PNG appearing under the final name is a broken
/// picture cached for as long as the document is unchanged.
/// `docformat::write_encoded` does the same for a document and is not reused:
/// it is `umber-core`'s and reaching into it for this would widen a function
/// whose containment is the point.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension("png.part");
    std::fs::write(&temporary, bytes)?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Leaving a `.part` behind in somebody's thumbnail cache is worse
            // than the failure that produced it.
            let _ = std::fs::remove_file(&temporary);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn the_flag_and_three_arguments_is_a_request() {
        assert_eq!(
            job(args(&["umber", FLAG, "in.clip", "out.png", "128"])),
            Some(Request {
                input: PathBuf::from("in.clip"),
                output: PathBuf::from("out.png"),
                size: 128,
            })
        );
    }

    /// `%s` is always sent by a desktop; a command typed by hand to see what a
    /// document looks like should not have to.
    #[test]
    fn the_size_may_be_left_off() {
        assert_eq!(
            job(args(&["umber", FLAG, "in.clip", "out.png"])).map(|r| r.size),
            Some(0)
        );
    }

    /// Nothing else on the command line is a thumbnail request, and the
    /// ordinary launch must not be mistaken for one.
    #[test]
    fn an_ordinary_launch_is_not_a_thumbnail_request() {
        assert_eq!(job(args(&["umber"])), None);
        assert_eq!(job(args(&["umber", "painting.clip"])), None);
        assert_eq!(job(args(&["umber", "--crash-report", "r.json"])), None);
    }

    /// A request missing its output names no file to write, so there is nothing
    /// to do. Umber then starts normally rather than refusing, which is the
    /// rule the rest of the command line keeps.
    #[test]
    fn an_incomplete_request_is_not_one() {
        assert_eq!(job(args(&["umber", FLAG])), None);
        assert_eq!(job(args(&["umber", FLAG, "in.clip"])), None);
    }

    /// A size that will not parse is read as "no bound" rather than refusing
    /// the whole request: a thumbnail at full size is a worse thumbnail and is
    /// not a failure.
    #[test]
    fn a_size_that_is_not_a_number_does_not_lose_the_request() {
        assert_eq!(
            job(args(&["umber", FLAG, "in.clip", "out.png", "big"])).map(|r| r.size),
            Some(0)
        );
    }

    /// End to end, through the real extractor and the real PNG encoder, with
    /// no window and no GPU — which is the whole claim this path rests on.
    #[test]
    fn a_document_becomes_a_png_of_the_size_that_was_asked_for() {
        let dir = std::env::temp_dir().join(format!("umber-thumb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let input = dir.join("doc.png");
        let output = dir.join("thumb.png");

        // A 64-square document, built through the same encoder the thumbnail
        // comes back out of. `umber-core`'s richer fixtures are `cfg(test)` and
        // therefore invisible from here, and a hand-rolled ORA would be a
        // second statement of that container's layout in a crate with no
        // business knowing it. What is under test on this side is the mode —
        // parse, extract, fit, encode, write — and a flat document exercises
        // every step of it.
        let source = umber_core::export::encode(
            &[255u8, 0, 0, 255].repeat(64 * 64),
            64,
            64,
            &ExportOptions {
                format: ExportFormat::Png,
                ..Default::default()
            },
        )
        .expect("a source document");
        std::fs::write(&input, source).expect("the fixture");

        run(&Request {
            input: input.clone(),
            output: output.clone(),
            size: 16,
        })
        .expect("a thumbnail");

        let written = std::fs::read(&output).expect("the thumbnail");
        assert!(
            written.starts_with(b"\x89PNG"),
            "a PNG is what was asked for"
        );
        let decoded = preview::from_bytes(&written, umber_core::docimport::SourceFormat::Png)
            .expect("a readable PNG");
        assert_eq!(decoded.size, glam::UVec2::new(16, 16));
        // Nothing is left behind in a directory a desktop is watching.
        assert!(!output.with_extension("png.part").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file with no preview in it writes nothing at all, so a desktop does
    /// not cache an empty picture as though it were the document.
    #[test]
    fn a_document_that_cannot_be_read_writes_no_file() {
        let dir = std::env::temp_dir().join(format!("umber-thumb-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a temporary directory");
        let input = dir.join("broken.clip");
        let output = dir.join("thumb.png");
        std::fs::write(&input, b"not a clip at all").expect("the fixture");

        assert!(
            run(&Request {
                input,
                output: output.clone(),
                size: 16,
            })
            .is_err()
        );
        assert!(!output.exists(), "a failure leaves no picture behind");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
