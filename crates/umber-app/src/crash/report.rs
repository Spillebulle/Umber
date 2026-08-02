//! What a crash report holds, and every sentence it can produce.
//!
//! Deliberately free of egui, winit and wgpu: a report is a plain record that
//! is written by a panic hook in one process and read by a window in another,
//! so every rule about what it says — how a payload is tidied, how a duration
//! is spelt, which documents were rescued and which were not — is a pure
//! function of the record and is tested without a device. The same division
//! [`crate::update::install`] keeps, and for the same reason: the crash path is
//! the one path in the application that cannot be exercised by using it.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// The shape of the file. Bumped only if a field's *meaning* changes; adding
/// one does not need it, because every field is `#[serde(default)]` and an
/// older file therefore still loads with blanks where the new one is.
///
/// It is recorded rather than enforced. The reader is the same executable that
/// wrote the file — `--crash-report` runs the binary that crashed — so a skew
/// only happens when somebody hands an old report to a new build by hand, and
/// showing them what could be read beats refusing the file that says why their
/// work stopped.
pub const FORMAT: u32 = 1;

/// How much of any one text field is kept.
///
/// A panic message is normally a line. It can be arbitrary: a `Debug` payload
/// of a large structure, or a wgpu validation error naming every field of a
/// descriptor. Neither the file nor the window benefits from megabytes of it,
/// and an unbounded write from a panic hook is the shape of failure this whole
/// module exists to avoid.
pub const FIELD_LIMIT: usize = 8 * 1024;

/// How much backtrace is kept. Larger than [`FIELD_LIMIT`] because the frames
/// are the useful part and a deep recursion is exactly the crash somebody needs
/// all of.
pub const BACKTRACE_LIMIT: usize = 128 * 1024;

/// What [`tidy`] appends when it has cut something short. Said out loud rather
/// than trailing off, so nobody reads a truncated backtrace as a complete one.
const TRUNCATED: &str = "\n… (truncated by Umber)";

/// One crash, as it is written to disk and read back by the reporter.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub format: u32,
    /// The Umber that crashed.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub arch: String,
    /// The GPU, as `umber_render::Gpu` already logs it. `None` when the crash
    /// happened before a device existed — which is itself worth knowing.
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
    /// The thread that panicked. Only ever `main` in a report that is shown:
    /// see [`crate::crash::install_hook`].
    #[serde(default)]
    pub thread: String,
    #[serde(default)]
    pub message: String,
    /// `file:line:column`, as the panic reported it.
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub backtrace: String,
    /// Whether a backtrace was actually captured. `RUST_BACKTRACE` is not
    /// needed — the hook forces one — but a platform can still decline, and an
    /// empty string must not be shown as though the stack were empty.
    #[serde(default)]
    pub backtrace_available: bool,
    #[serde(default)]
    pub documents: Vec<DocumentNote>,
}

/// One open document, as it stood when the process died.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentNote {
    #[serde(default)]
    pub title: String,
    /// True when closing the tab would have lost something — exactly what
    /// [`crate::session::Tab::modified`] means.
    #[serde(default)]
    pub modified: bool,
    /// [`crate::session::Tab::revision`] at the moment of the crash, against
    /// which an autosave's own revision says whether the copy is complete.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub autosave: Option<AutosaveCopy>,
}

/// The last autosave of one document, and what it was written from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutosaveCopy {
    #[serde(default)]
    pub path: String,
    /// The document's revision when the *capture* began. Equal to the
    /// document's revision at the crash means the copy holds everything; lower
    /// means it does not, and the box has to say so.
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub seconds_ago: u64,
}

/// One document the crash box can point somebody at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rescue {
    pub title: String,
    pub path: String,
    /// Whether the copy holds every change the document had.
    pub complete: bool,
    pub seconds_ago: u64,
}

impl Rescue {
    /// The sentence under the path.
    ///
    /// The incomplete case is stated plainly and first. Claiming work was safe
    /// when it was not is worse than claiming nothing — the same rule
    /// `Session::mark_autosaved` lives by, said out loud here because this is
    /// the one place a person reads the answer.
    pub fn note(&self) -> String {
        let age = age_phrase(self.seconds_ago);
        if self.complete {
            format!("Written {age}, with everything you had done.")
        } else {
            format!("Written {age}. It does not include your most recent changes.")
        }
    }
}

impl Report {
    /// Every open document that has a copy worth pointing at.
    ///
    /// One rule decides it, and it is `modified`: that flag means "closing this
    /// would lose something", so a document whose own file the autosave wrote
    /// has already had it cleared and is not listed. A saved, untouched
    /// document is mentioned nowhere, which is right — there is nothing to say
    /// about it.
    pub fn rescued(&self) -> Vec<Rescue> {
        self.documents
            .iter()
            .filter(|d| d.modified)
            .filter_map(|d| {
                let copy = d.autosave.as_ref()?;
                Some(Rescue {
                    title: d.title.clone(),
                    path: copy.path.clone(),
                    complete: copy.revision == d.revision,
                    seconds_ago: copy.seconds_ago,
                })
            })
            .collect()
    }

    /// Documents that had unsaved work and no copy of it anywhere.
    ///
    /// Named rather than passed over. A crash box that lists two rescued
    /// documents and says nothing about the third reads as a promise about the
    /// third.
    pub fn at_risk(&self) -> Vec<String> {
        self.documents
            .iter()
            .filter(|d| d.modified && d.autosave.is_none())
            .map(|d| d.title.clone())
            .collect()
    }

    /// The block behind "Technical details", and the whole of what the report
    /// file is for.
    ///
    /// Assembled here rather than in the window so that what is shown and what
    /// a person would paste into a bug report are the same text by
    /// construction.
    pub fn details(&self) -> String {
        let mut out = String::with_capacity(self.backtrace.len() + 512);
        out.push_str(&format!("Umber {}\n", blank_as_unknown(&self.version)));
        out.push_str(&format!(
            "{} ({})\n",
            blank_as_unknown(&self.os),
            blank_as_unknown(&self.arch),
        ));
        match (&self.adapter, &self.backend) {
            (Some(name), Some(backend)) => out.push_str(&format!("GPU: {name} ({backend})\n")),
            (Some(name), None) => out.push_str(&format!("GPU: {name}\n")),
            // Not "GPU: unknown". No device means the crash happened before one
            // existed, which is a different and more useful fact.
            _ => out.push_str("GPU: no device had been created\n"),
        }

        out.push('\n');
        let thread = blank_as_unknown(&self.thread);
        match &self.location {
            Some(at) => out.push_str(&format!("thread '{thread}' panicked at {at}:\n")),
            None => out.push_str(&format!("thread '{thread}' panicked:\n")),
        }
        out.push_str(blank_as_unknown(&self.message));
        out.push('\n');

        out.push('\n');
        if self.backtrace_available && !self.backtrace.trim().is_empty() {
            out.push_str(&self.backtrace);
        } else {
            // Distinguished from an empty stack, which is not a thing that
            // happens. See `Report::backtrace_available`.
            out.push_str("No backtrace was captured on this platform.");
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out
    }

    pub fn encode(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|e| e.to_string())
    }

    pub fn decode(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// Write the report where the child process will find it.
    ///
    /// Not `docformat::write_encoded`'s temporary-and-rename: that exists so a
    /// half-written file cannot replace an artist's last good document, and
    /// this file replaces nothing and is named after the instant it describes.
    /// The plainer write is also the one with fewer ways to fail inside a panic
    /// hook, which is the only place it is called from.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(path, self.encode()?).map_err(|e| e.to_string())
    }

    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        Self::decode(&text)
    }
}

/// Make a payload safe to put in a file and in a window.
///
/// Three things, all of which have bitten somebody else's crash handler:
///
/// * **Control characters go.** A panic message is arbitrary text and can carry
///   a stray `\r` or an escape sequence; the first breaks a line count and the
///   second can repaint a terminal the report is printed to. Tabs and newlines
///   are kept, because a backtrace is made of them.
/// * **The length is bounded.** See [`FIELD_LIMIT`].
/// * **The cut lands on a character boundary**, so what is written is still
///   UTF-8. Slicing a `String` by bytes is the classic way to produce a crash
///   report that itself cannot be read.
pub fn tidy(text: &str, limit: usize) -> String {
    let mut out = String::with_capacity(text.len().min(limit) + TRUNCATED.len());
    for ch in text.chars() {
        if out.len() >= limit {
            out.push_str(TRUNCATED);
            return out;
        }
        // `is_control` covers DEL and the C1 range as well as the C0 one.
        if ch.is_control() && ch != '\n' && ch != '\t' {
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    // Trailing blank lines are noise in a window that scrolls; leading ones
    // shift the first useful line out of view.
    out.trim().to_string()
}

/// The message out of a panic payload.
///
/// `&str` and `String` are what `panic!` produces; anything else came through
/// `panic_any` and cannot be printed, so the report says which it was rather
/// than inventing a message.
pub fn payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        return tidy(s, FIELD_LIMIT);
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return tidy(s, FIELD_LIMIT);
    }
    "a panic carrying a payload that is not a string".to_string()
}

/// How long ago, in words.
///
/// Rounded down and coarse on purpose: the useful question is "is that copy
/// worth going back to", which "four minutes ago" answers and "4 m 12 s" does
/// not. Written here rather than with `umber_core::time` because that module
/// spells *moments*, and this is an interval whose exact seconds nobody wants.
pub fn age_phrase(seconds: u64) -> String {
    match seconds {
        0..=29 => "moments ago".to_string(),
        30..=89 => "a minute ago".to_string(),
        90..=3599 => format!("{} minutes ago", (seconds + 30) / 60),
        3600..=5399 => "an hour ago".to_string(),
        _ => format!("{} hours ago", (seconds + 1800) / 3600),
    }
}

/// A field that was never filled in, said rather than left blank.
fn blank_as_unknown(text: &str) -> &str {
    if text.trim().is_empty() {
        "unknown"
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(title: &str, modified: bool, revision: u64) -> DocumentNote {
        DocumentNote {
            title: title.to_string(),
            modified,
            revision,
            path: None,
            autosave: None,
        }
    }

    fn with_copy(mut note: DocumentNote, revision: u64, seconds_ago: u64) -> DocumentNote {
        note.autosave = Some(AutosaveCopy {
            path: format!("/tmp/{}.ora", note.title),
            revision,
            seconds_ago,
        });
        note
    }

    fn sample() -> Report {
        Report {
            format: FORMAT,
            version: "0.0.4".into(),
            os: "windows".into(),
            arch: "x86_64".into(),
            adapter: Some("NVIDIA GeForce RTX 4070".into()),
            backend: Some("Vulkan".into()),
            thread: "main".into(),
            message: "wgpu error: Validation Error".into(),
            location: Some("src/app.rs:1899:22".into()),
            backtrace: "   0: umber_app::app::render\n   1: winit::run_app\n".into(),
            backtrace_available: true,
            documents: vec![with_copy(doc("Study.ora", true, 12), 12, 90)],
        }
    }

    // -- the file ----------------------------------------------------------

    #[test]
    fn a_report_survives_being_written_and_read_back() {
        let report = sample();
        let text = report.encode().expect("a report encodes");
        assert_eq!(Report::decode(&text).expect("it decodes"), report);
    }

    /// The reader is normally the same build as the writer, but a report kept
    /// from an older Umber must still open rather than refusing the one file
    /// that says why somebody's work stopped.
    #[test]
    fn a_report_missing_every_new_field_still_loads() {
        let older = r#"{"format":1,"version":"0.0.1","message":"boom"}"#;
        let report = Report::decode(older).expect("an older report loads");
        assert_eq!(report.message, "boom");
        assert!(report.documents.is_empty());
        assert!(report.adapter.is_none());
        // And it still produces a readable details block rather than a page of
        // empty lines.
        let details = report.details();
        assert!(details.contains("boom"), "{details}");
        assert!(details.contains("unknown"), "{details}");
    }

    #[test]
    fn a_report_round_trips_through_a_file() {
        let dir = std::env::temp_dir().join(format!(
            "umber-crash-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let path = dir.join("crash-1.json");
        let report = sample();
        report.write(&path).expect("the report is written");
        assert_eq!(Report::read(&path).expect("it reads back"), report);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- tidying -----------------------------------------------------------

    #[test]
    fn control_characters_are_replaced_and_newlines_are_kept() {
        let raw = "line one\r\nline\u{1b}[2J two\ttabbed";
        let clean = tidy(raw, FIELD_LIMIT);
        assert!(!clean.contains('\r'), "{clean:?}");
        assert!(!clean.contains('\u{1b}'), "{clean:?}");
        assert!(clean.contains('\n'), "{clean:?}");
        assert!(clean.contains('\t'), "{clean:?}");
    }

    /// A cut in the middle of a multi-byte character would produce a report
    /// that cannot itself be read — the failure this handler exists to not be.
    #[test]
    fn a_long_message_is_cut_on_a_character_boundary_and_says_so() {
        let raw = "é".repeat(4096);
        let clean = tidy(&raw, 64);
        assert!(clean.ends_with(TRUNCATED.trim_start()), "{clean:?}");
        assert!(clean.len() < 200, "{}", clean.len());
        // The whole point: still valid UTF-8, which is only interesting because
        // `é` is two bytes and the limit is not a multiple of two.
        assert!(std::str::from_utf8(clean.as_bytes()).is_ok());
    }

    #[test]
    fn a_message_within_the_limit_is_left_alone() {
        assert_eq!(tidy("  ordinary panic  ", FIELD_LIMIT), "ordinary panic");
    }

    // -- the payload -------------------------------------------------------

    #[test]
    fn every_shape_of_panic_payload_produces_a_message() {
        let literal: Box<dyn std::any::Any + Send> = Box::new("assertion failed");
        assert_eq!(payload_message(&*literal), "assertion failed");

        let formatted: Box<dyn std::any::Any + Send> = Box::new("wgpu error: 3".to_string());
        assert_eq!(payload_message(&*formatted), "wgpu error: 3");

        // `panic_any(1u32)` carries something with no printable form. Saying so
        // is the honest answer; inventing a message would be indistinguishable
        // from a real one.
        let odd: Box<dyn std::any::Any + Send> = Box::new(1u32);
        let message = payload_message(&*odd);
        assert!(message.contains("not a string"), "{message}");
    }

    // -- what was rescued --------------------------------------------------

    #[test]
    fn only_documents_that_would_have_lost_something_are_listed() {
        let report = Report {
            documents: vec![
                // Saved and untouched: nothing to say about it at all.
                with_copy(doc("Saved.ora", false, 4), 4, 10),
                // Unsaved, and a copy that holds all of it.
                with_copy(doc("Whole.ora", true, 7), 7, 60),
                // Unsaved, and a copy taken two strokes ago.
                with_copy(doc("Behind.ora", true, 9), 7, 240),
                // Unsaved with no copy anywhere.
                doc("Untitled 3", true, 2),
            ],
            ..Report::default()
        };

        let rescued = report.rescued();
        let titles: Vec<&str> = rescued.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(titles, ["Whole.ora", "Behind.ora"]);
        assert!(rescued[0].complete);
        assert!(!rescued[1].complete);
        assert_eq!(report.at_risk(), ["Untitled 3"]);
    }

    /// The one sentence in the box that must never overstate: a copy taken
    /// before the last few strokes has to say so.
    #[test]
    fn an_incomplete_copy_says_what_it_is_missing() {
        let whole = Rescue {
            title: "a".into(),
            path: "/tmp/a.ora".into(),
            complete: true,
            seconds_ago: 60,
        };
        assert!(
            whole.note().contains("everything you had done"),
            "{}",
            whole.note()
        );

        let behind = Rescue {
            complete: false,
            ..whole
        };
        let note = behind.note();
        assert!(note.contains("does not include"), "{note}");
        assert!(!note.contains("everything you had done"), "{note}");
    }

    #[test]
    fn a_report_with_nothing_open_claims_nothing() {
        let report = Report::default();
        assert!(report.rescued().is_empty());
        assert!(report.at_risk().is_empty());
    }

    // -- phrasing ----------------------------------------------------------

    #[test]
    fn an_age_is_spelt_at_the_coarseness_somebody_would_use() {
        assert_eq!(age_phrase(0), "moments ago");
        assert_eq!(age_phrase(29), "moments ago");
        assert_eq!(age_phrase(45), "a minute ago");
        assert_eq!(age_phrase(240), "4 minutes ago");
        assert_eq!(age_phrase(3600), "an hour ago");
        assert_eq!(age_phrase(7200), "2 hours ago");
    }

    // -- the details block -------------------------------------------------

    #[test]
    fn the_details_block_carries_everything_a_bug_report_needs() {
        let details = sample().details();
        for wanted in [
            "0.0.4",
            "windows",
            "x86_64",
            "NVIDIA GeForce RTX 4070",
            "Vulkan",
            "main",
            "src/app.rs:1899:22",
            "wgpu error: Validation Error",
            "umber_app::app::render",
        ] {
            assert!(
                details.contains(wanted),
                "{wanted} missing from:\n{details}"
            );
        }
    }

    /// A crash before `Gpu::new` returns has no adapter, and "unknown" would
    /// hide the most interesting thing about it.
    #[test]
    fn a_crash_with_no_device_says_there_was_no_device() {
        let report = Report {
            adapter: None,
            backend: None,
            ..sample()
        };
        assert!(
            report.details().contains("no device had been created"),
            "{}",
            report.details(),
        );
    }

    #[test]
    fn a_missing_backtrace_is_not_drawn_as_an_empty_stack() {
        let report = Report {
            backtrace: String::new(),
            backtrace_available: false,
            ..sample()
        };
        assert!(
            report.details().contains("No backtrace was captured"),
            "{}",
            report.details(),
        );
    }
}
