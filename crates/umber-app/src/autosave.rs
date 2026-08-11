//! Writing open documents out on a timer, without the painter noticing.
//!
//! Umber's Save is an explicit act: it blocks on a GPU readback once per layer,
//! encodes eight PNGs, and writes an archive. Doing that on a five-minute timer
//! would stop the canvas dead in the middle of a stroke, which is the one thing
//! this application exists not to do. So an autosave is the same file written
//! by an entirely different route:
//!
//! 1. **It starts only at a quiet moment.** [`Autosave::next_due`] refuses
//!    while the pointer is down or a stroke is live, so "every five minutes"
//!    is really "at the first quiet moment after five minutes". That is what a
//!    painter wants anyway — nobody wants their work written down halfway
//!    through a line.
//! 2. **The pixels come off the GPU without a stall**, through
//!    `CanvasRenderer::begin_capture`: one layer in flight at a time, read back
//!    four megabytes per frame, collected by a poll that never waits. Measured
//!    on a 2048-square eight-layer document, the worst frame it adds is about a
//!    millisecond.
//! 3. **The encode and the writing happen on a thread.** Deflating eight
//!    canvas-sized PNGs is seconds of CPU; none of it belongs on the frame
//!    loop. The writer here is the same shape as the one in [`crate::prefs`].
//!
//! # Where it writes
//!
//! * A document that has **never been saved** goes to the internal location
//!   only — [`internal_dir`], under the platform's *data* directory beside the
//!   brush library. There is nowhere else it could go: Umber has not been told
//!   where the painter wants it, and inventing a location in their documents
//!   folder would be putting files somewhere they did not ask for.
//! * A document that **has a path** is written to both. Writing its own file is
//!   the point of an autosave, and the internal copy is what survives the file
//!   being overwritten by something else, or a drive going away.
//!
//! Autosaving to the document's own path **overwrites it without asking**. That
//! is deliberate and it is what was asked for; it is also why the tab's dot is
//! cleared when — and only when — the document has not moved since the capture
//! began. See [`crate::session::Session::mark_autosaved`].
//!
//! # What it does not write
//!
//! The undo history. A save carries it when the preference is on, and the file
//! format has room for it, but the history is up to 32 MB of PNG-encoded
//! patches and re-encoding all of it every five minutes, unattended, is a cost
//! nobody asked for. An autosave is a recovery artefact: it exists so that the
//! painting is not lost, not so that the afternoon can be replayed.
//!
//! # Expiry
//!
//! Internal copies are deleted once they are older than the configured age.
//! **[`Reaper`] is the only thing in Umber that deletes a document, and it can
//! only reach inside one directory.** See its own documentation — that
//! containment is structural, not a matter of the callers being careful.
//!
//! # Offering a copy back
//!
//! Writing copies is only half of a recovery. The other half is that the next
//! start of Umber *offers* them, and that needs one fact the copies themselves
//! cannot carry: did the last session end on purpose, or did it stop?
//!
//! [`SessionMark`] is the answer. One small file per run, under
//! [`SESSIONS_DIR`], **held open and exclusively locked for the whole run**:
//!
//! * A **clean exit** removes it — [`Autosave::end_run`], from the one place
//!   the event loop is known to have finished.
//! * A **crash, a hard kill, an out-of-memory or a power cut** leaves it. That
//!   is the whole point: a crash report is written by a panic hook and proves a
//!   panic, but nothing is written when the process is killed outright, so a
//!   report on disk cannot be the only signal.
//! * **A second copy of Umber running** holds its own marker, locked, and the
//!   first one's marker is locked too — so neither mistakes the other for a
//!   session that died. The lock is the operating system's, which is exactly
//!   why it survives every way a process can stop without warning; a recorded
//!   process id would not, because ids are reused.
//! * A lock that can be neither taken nor refused — a file system that does not
//!   support locking — is read as **still running**. Not offering costs an
//!   offer, which the autosave folder still holds; over-offering would put two
//!   processes on one painting.
//!
//! The marker also carries what the session had open, refreshed by
//! [`Autosave::note_documents`] on the same terms
//! [`crate::crash::note_documents`] keeps: called every frame, reduced to one
//! number first, and rebuilt only when that number moves. That is what lets a
//! document closed or saved after its copy was written stop being offered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use glam::UVec2;
use serde::{Deserialize, Serialize};
use umber_core::docformat::{self, SaveDocument, SaveLayer};
use umber_core::textobj::TextObject;
use umber_core::{Background, BlendMode, Effect};
use umber_render::{CanvasRenderer, DocumentCapture, Gpu};

use crate::editor::Editor;
use crate::session::{DocId, Session};
use crate::tabs::Notice;

/// Directory under the platform data directory that holds the internal copies.
pub const DIR_NAME: &str = "autosave";

/// Directory under [`DIR_NAME`] that holds one [`SessionMark`] per run.
///
/// A subdirectory rather than a name beside the copies, so [`Reaper`] — which
/// does not recurse and only ever considers a `.ora` — cannot see a marker at
/// all, and so the folder somebody opens from Settings holds their documents
/// and one folder of Umber's bookkeeping rather than the two mixed together.
pub const SESSIONS_DIR: &str = "sessions";

/// How often an autosave runs out of the box.
pub const DEFAULT_INTERVAL_MINUTES: u32 = 5;

/// Bounds on the interval, matching the slider in the settings dialog. Below a
/// minute the capture of a large document would barely finish before the next
/// began; above two hours it is not an autosave.
pub const MIN_INTERVAL_MINUTES: u32 = 1;
pub const MAX_INTERVAL_MINUTES: u32 = 120;

/// How long an internal copy is kept, out of the box: thirty days.
pub const DEFAULT_EXPIRY_HOURS: u32 = 30 * 24;

/// Longest expiry the settings dialog offers, and the ceiling a hand-edited
/// preferences file is clamped to: a year.
pub const MAX_EXPIRY_HOURS: u32 = 24 * 365;

/// The ladder the settings dialog's expiry control steps through, in hours.
///
/// Zero is "keep for ever". A ladder rather than a free slider because the
/// useful answers are a handful of round durations and nobody wants to land
/// exactly on 720 by dragging; the preferences file still takes any number of
/// hours, so a hand-edited 100 survives until the control is next touched.
pub const EXPIRY_LADDER: [u32; 8] = [0, 6, 24, 72, 168, 336, DEFAULT_EXPIRY_HOURS, 2160];

// ---------------------------------------------------------------------------
// Where the internal copies live
// ---------------------------------------------------------------------------

/// The internal autosave directory, or `None` on a system with no home
/// directory — a real case in containers and on some CI runners.
///
/// The **data** directory, not the configuration one: these are documents, and
/// a config directory is for settings. It sits beside the brush library, which
/// [`umber_core::preset::UserLibrary::default_dir`] finds the same way.
pub fn internal_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "Umber").map(|d| d.data_dir().join(DIR_NAME))
}

/// Where one run's [`SessionMark`] goes. Inside [`internal_dir`], so there is
/// still only one statement of where Umber keeps its own copies.
pub fn sessions_dir() -> Option<PathBuf> {
    internal_dir().map(|d| d.join(SESSIONS_DIR))
}

/// The path as the settings dialog shows it, or a plain explanation when there
/// is none. Never a silent blank.
pub fn internal_dir_label() -> String {
    match internal_dir() {
        Some(path) => path.display().to_string(),
        None => "unavailable: this system has no data directory".to_string(),
    }
}

/// Show a directory in the system file manager.
///
/// Best effort by construction: there is no portable way to ask, and a desktop
/// with no file manager is a real configuration. A failure is logged and
/// nothing else happens, because the settings dialog already prints the path
/// beside the button.
pub fn reveal(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    let command = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // Explorer reports failure with a non-zero exit code even when it worked,
    // so the status is deliberately not checked on any platform — spawning is
    // the whole of what can be known here.
    std::process::Command::new(command).arg(path).spawn()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Expiry
// ---------------------------------------------------------------------------

/// Why a deleter would not delete something.
///
/// One vocabulary shared by the two — [`Reaper`] and [`Marks`] — rather than
/// one each, and each of them can only produce a subset of it: a reaper never
/// answers [`Refused::NotASessionMark`] and a marks deleter never answers
/// [`Refused::NotAnAutosave`]. That is the point rather than an untidiness.
/// Both refusals mean the same thing — *this is not the kind of file I delete*
/// — and the two are kept apart precisely so each can only say it about its
/// own kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It does not resolve to a file directly inside the reaper's root.
    Outside,
    /// Not a plain file — a directory, or a symbolic link.
    NotAPlainFile,
    /// Not a name an autosave writes.
    NotAnAutosave,
    /// Not a name [`SessionMark`] writes. Only [`Marks`] can answer this, and
    /// only [`Marks`] can act on it — see its own documentation for why the two
    /// deleters are kept apart.
    NotASessionMark,
    /// The file system said no.
    Io(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outside => write!(f, "it is not inside the autosave folder"),
            Self::NotAPlainFile => write!(f, "it is not a plain file"),
            Self::NotAnAutosave => write!(f, "it is not an autosave"),
            Self::NotASessionMark => write!(f, "it is not a session marker"),
            Self::Io(e) => write!(f, "{e}"),
        }
    }
}

/// What one sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Expired {
    pub deleted: usize,
    /// Everything the reaper looked at and would not touch, with the reason.
    /// Kept rather than counted so a refusal can be logged with its path.
    pub refused: Vec<(PathBuf, Refused)>,
}

/// The one thing in Umber that deletes a document — and it can only reach
/// inside a single directory.
///
/// **Expiry must never touch a file the painter chose the location of.**
/// Deleting somebody's painting is the worst thing this application could do,
/// and "the callers only ever hand it internal paths" is not good enough: a
/// later change makes that false, silently, and nobody finds out until an
/// afternoon has gone. So the containment is in the type rather than in the
/// callers.
///
/// A `Reaper` is built around one **canonicalised** root and refuses anything
/// that does not resolve to a file *directly inside* it:
///
/// * The root is resolved once, at construction. A relative path, a `..`, a
///   junction or a symbolic link in it is gone by the time anything is
///   compared against it.
/// * Every candidate is resolved independently before it is compared, so a
///   symbolic link inside the directory pointing at `~/paintings/hands.ora`
///   resolves *outside* the root and is refused. The metadata is read with
///   `symlink_metadata`, which does not follow, so a link is refused as "not a
///   plain file" before it is even resolved.
/// * The comparison is *parent equals root*, not "starts with", so the reaper
///   cannot descend. It never recurses either.
/// * Only names an autosave writes are candidates: a `.ora`, or the
///   `.ora.saving` temporary a write that died halfway leaves behind.
///
/// `a_reaper_refuses_a_path_outside_its_root` and
/// `a_documents_own_file_survives_its_internal_copy_expiring` pin the two that
/// matter.
#[derive(Clone, Debug)]
pub struct Reaper {
    root: PathBuf,
}

impl Reaper {
    /// Build a reaper for `root`, resolving it once and for all.
    ///
    /// Fails when the directory does not exist, which is the ordinary state
    /// before the first autosave and means there is nothing to expire.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            root: std::fs::canonicalize(root)?,
        })
    }

    /// Delete every internal autosave last written more than `max_age` ago.
    ///
    /// Age comes from the file system's modification time rather than from
    /// anything encoded in the name: a name is something anyone can write, and
    /// "when was this last autosaved" is exactly what an mtime means.
    ///
    /// A file dated in the *future* — a clock that has been put back, a copy
    /// restored from a backup — is left alone rather than treated as infinitely
    /// old. Leaning towards keeping is the only defensible direction here.
    pub fn expire(&self, max_age: Duration, now: SystemTime) -> Expired {
        let mut out = Expired::default();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Does not follow links, so a link's own metadata is what is read.
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let Ok(modified) = meta.modified() else {
                continue;
            };
            let Ok(age) = now.duration_since(modified) else {
                continue;
            };
            if age <= max_age {
                continue;
            }
            match self.remove(&path) {
                Ok(()) => out.deleted += 1,
                Err(why) => out.refused.push((path, why)),
            }
        }
        out
    }

    /// Delete one file, refusing anything that is not an autosave of this
    /// reaper's own directory.
    ///
    /// Public, and the only way out of this module to a `remove_file`, so that
    /// every deletion goes through the checks above rather than around them.
    pub fn remove(&self, candidate: &Path) -> Result<(), Refused> {
        // Before anything else, and without following: a symbolic link is
        // never something Umber wrote, and following one is precisely how a
        // reaper reaches a file the painter chose the location of.
        let meta = std::fs::symlink_metadata(candidate).map_err(|e| Refused::Io(e.to_string()))?;
        if !meta.is_file() {
            return Err(Refused::NotAPlainFile);
        }
        if !is_autosave_name(candidate) {
            return Err(Refused::NotAnAutosave);
        }
        // Resolved, so `..`, a junction, or a link anywhere along the path
        // cannot walk out of the root behind the comparison's back.
        let resolved = std::fs::canonicalize(candidate).map_err(|e| Refused::Io(e.to_string()))?;
        if resolved.parent() != Some(self.root.as_path()) {
            return Err(Refused::Outside);
        }
        std::fs::remove_file(&resolved).map_err(|e| Refused::Io(e.to_string()))
    }
}

/// True for the two names an autosave writes: the archive, and the temporary
/// neighbour `docformat::write_encoded` renames into place.
fn is_autosave_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    let ext = format!(".{}", docformat::EXTENSION);
    lower.ends_with(&ext) || lower.ends_with(&format!("{ext}.saving"))
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/// FNV-1a, written out rather than taken from `DefaultHasher`.
///
/// The internal copy's name has to be the *same* one next week, so a document
/// autosaved today is overwritten rather than accumulated. `DefaultHasher` is
/// explicitly not stable between Rust releases, so an upgrade would silently
/// rename every internal copy and leave the old ones to expire — a directory
/// slowly filling with duplicates of the same painting.
fn digest(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

/// A file name stem taken from a title, safe on every platform.
fn stem_of(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('-').trim();
    let short: String = trimmed.chars().take(48).collect();
    if short.is_empty() {
        "untitled".to_string()
    } else {
        short.trim_end().to_string()
    }
}

/// The name of one document's internal copy.
///
/// A document with a path is keyed on that path, so reopening it tomorrow
/// autosaves over yesterday's copy rather than beside it. One that has never
/// been saved is keyed on this run and its own number, because two untitled
/// documents in one session are two paintings — and an untitled document today
/// is not the untitled document of a week ago either.
fn internal_name(stem: &str, key: u64) -> String {
    format!("{stem}-{key:016x}.{}", docformat::EXTENSION)
}

// ---------------------------------------------------------------------------
// Did the last session end cleanly?
// ---------------------------------------------------------------------------

/// The shape of a marker file. Bumped only if a field's *meaning* changes;
/// adding one does not need it, because every field is `#[serde(default)]`.
///
/// A marker this build cannot read is **discarded**, not refused: it names
/// copies that are still on disk either way, and the folder is one click away
/// in Settings. Same rule the undo history's manifest lives by.
const MARK_FORMAT: u32 = 1;

/// How much newer the painter's own file has to be before it counts as newer
/// than the internal copy.
///
/// Two seconds because an autosave writes the internal copy and then the
/// document's own file, a moment apart, and because FAT records a modification
/// time to the nearest two seconds. Without the slack the two halves of one
/// autosave read as "the copy is behind" on a memory stick.
const SAME_MOMENT: Duration = Duration::from_secs(2);

/// How much of a document's title the marker keeps.
///
/// Far beyond any real title — a file name is bounded by the file system long
/// before this — and here for the reason [`crate::crash::Report`] bounds every
/// field it writes: an unbounded write from a path that runs unattended is the
/// shape of failure both of these exist to avoid.
const TITLE_LIMIT: usize = 200;

/// One document, as the running session last described it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkedDocument {
    #[serde(default)]
    pub title: String,
    /// The file the painter chose, if the document had one. A `String` rather
    /// than a `PathBuf` for the reason [`crate::crash::Report`]'s is: this goes
    /// through JSON, and a path that is not UTF-8 must not fail the write — a
    /// marker that refused to be written is a recovery nobody is offered.
    ///
    /// The trade is that such a path comes back with replacement characters in
    /// it. The ceiling on that is low and worth stating: a mangled name cannot
    /// collide with a real one, so a Save through it creates an oddly named
    /// file rather than overwriting the wrong painting.
    #[serde(default)]
    pub path: Option<String>,
    /// The internal copy chosen for this document, if one has been.
    ///
    /// Chosen when a capture *begins*, so the file it names may not exist —
    /// [`offer_from`] reads the file system rather than trusting this, and
    /// [`ours`] refuses anything that is not a copy of Umber's own.
    ///
    /// Lossy for the same reason [`MarkedDocument::path`] is, and the failure
    /// is the safe direction: a mangled name does not exist, so the document is
    /// listed as one there is no copy of rather than opened from the wrong
    /// file.
    #[serde(default)]
    pub copy: Option<String>,
    /// Whether closing this document would have lost something, as of the last
    /// time the marker was written. Exactly [`crate::session::Tab::modified`].
    #[serde(default)]
    pub modified: bool,
}

/// What one run of Umber leaves behind while it is running.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    #[serde(default)]
    pub format: u32,
    /// The Umber that was running.
    ///
    /// Written, and read by nothing — deliberately, and unlike
    /// [`SessionRecord::format`], which is compared. What it is for is the
    /// person looking at the file: a marker that outlives its build is one
    /// somebody is debugging, and "which Umber left this" is the first thing
    /// they will want. [`MARK_FORMAT`] is what decides whether the file may be
    /// acted on.
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub documents: Vec<MarkedDocument>,
}

/// This run's marker: a file that exists for exactly as long as Umber is
/// running, and is **locked** for exactly as long as this process holds it.
///
/// The lock is what makes the whole scheme work, and it is not a detail:
///
/// * It is released by the operating system when the process ends, **however**
///   it ends. A hard kill, an out-of-memory, a power cut and an ordinary panic
///   all leave the file behind with nobody holding it, which is precisely the
///   reading "this session did not end cleanly".
/// * It cannot go stale. A recorded process id can — ids are reused, and a
///   marker naming one would eventually point at somebody's web browser.
/// * A second Umber sees the first's marker *locked* and leaves it alone, so
///   starting a second window does not offer to recover the documents open in
///   the first.
///
/// The record is rewritten in place through the same handle, so the lock is
/// never let go of in the middle of a run.
#[derive(Debug)]
pub struct SessionMark {
    path: PathBuf,
    file: std::fs::File,
}

impl SessionMark {
    /// Create this run's marker in `dir` and take its lock.
    ///
    /// The name is the run's own token, so two Umbers started in the same
    /// millisecond still get a file each — and if they somehow did not, the
    /// lock refuses the second rather than letting it overwrite the first.
    pub fn open(dir: &Path, token: u64) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(mark_name(token));
        // **Not** `truncate`, and that is the one subtle thing here: the file is
        // opened before it is locked, so truncating on open would empty a
        // marker somebody else is holding *before* finding out they hold it —
        // wiping a running session's record on the way to failing. The record
        // is truncated by `write` instead, which only ever runs once the lock
        // is ours, and until then nothing may read the file because we hold it.
        let file = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        // Write access on both platforms: Windows' `LockFileEx` wants a handle
        // that can be written to, and this handle is the one the record is
        // rewritten through anyway.
        file.try_lock().map_err(|e| match e {
            std::fs::TryLockError::Error(e) => e,
            std::fs::TryLockError::WouldBlock => {
                std::io::Error::other("another Umber already holds this marker")
            }
        })?;
        Ok(Self { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Replace what the marker says. Truncate-and-write through the handle that
    /// holds the lock, so nothing has to be closed and reopened.
    pub fn write(&mut self, record: &SessionRecord) -> std::io::Result<()> {
        use std::io::{Seek, Write};
        let bytes = serde_json::to_vec(record).map_err(std::io::Error::other)?;
        self.file.set_len(0)?;
        self.file.rewind()?;
        self.file.write_all(&bytes)?;
        self.file.flush()
    }
}

/// The one thing that deletes a **session marker** — and it is deliberately not
/// [`Reaper`].
///
/// The temptation is to widen `Reaper` by one name. That is exactly the
/// loosening its own documentation refuses: it is the only thing in Umber that
/// may delete a *document*, and the narrowness of "a `.ora` directly inside one
/// canonicalised root" is what makes that safe to reason about. A second, much
/// smaller deleter that can only ever reach a sixteen-hex-digit `.json` inside
/// the `sessions` directory takes nothing away from that guarantee, where a
/// `Reaper` that had learnt about a second extension would.
///
/// The containment is the same shape, and for the same reasons: one
/// canonicalised root, every candidate canonicalised independently, the
/// candidate's *parent* required to equal the root so it cannot descend,
/// `symlink_metadata` first so a link is refused before it is resolved, no
/// recursion, and only names this module writes.
/// `a_marks_deleter_refuses_a_document` is the guard, and it is the one that
/// matters: recovering a copy — or declining to — must never remove it.
#[derive(Clone, Debug)]
pub struct Marks {
    root: PathBuf,
}

impl Marks {
    /// Build a deleter for `root`, resolving it once and for all.
    ///
    /// Fails when the directory does not exist, which is the ordinary state
    /// before the first run that ever wrote a marker.
    pub fn new(root: &Path) -> std::io::Result<Self> {
        Ok(Self {
            root: std::fs::canonicalize(root)?,
        })
    }

    /// Every marker directly inside the root.
    pub fn list(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_mark_name(p))
            .filter(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file()))
            .collect();
        // A stable order so two runs of the recovery scan list the same
        // documents in the same order.
        out.sort();
        out
    }

    /// Delete one marker, refusing anything that is not one of this directory's.
    pub fn remove(&self, candidate: &Path) -> Result<(), Refused> {
        let meta = std::fs::symlink_metadata(candidate).map_err(|e| Refused::Io(e.to_string()))?;
        if !meta.is_file() {
            return Err(Refused::NotAPlainFile);
        }
        if !is_mark_name(candidate) {
            return Err(Refused::NotASessionMark);
        }
        let resolved = std::fs::canonicalize(candidate).map_err(|e| Refused::Io(e.to_string()))?;
        if resolved.parent() != Some(self.root.as_path()) {
            return Err(Refused::Outside);
        }
        std::fs::remove_file(&resolved).map_err(|e| Refused::Io(e.to_string()))
    }
}

/// The name one run's marker takes: its token, in hex, and nothing else.
fn mark_name(token: u64) -> String {
    format!("{token:016x}.json")
}

/// True only for a name [`mark_name`] could have produced.
///
/// Deliberately strict about the stem as well as the extension. A `.json`
/// somebody dropped in the directory is not ours to delete, and this is the
/// whole of what stops [`Marks`] from being a general JSON deleter.
fn is_mark_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Take a stopped session's marker, or answer `None` if it is not ours to
/// take.
///
/// Answered by the lock: if it can be taken, nobody holds it, and the only
/// thing that ever holds it is a running Umber. A refusal means one is still
/// running. **An error means neither**, and is read as "still running" — see
/// the module docs.
///
/// The handle is **returned rather than dropped**, and that is the point of the
/// name. A marker read as abandoned and then let go of is unlocked for as long
/// as the offer sits on screen — minutes — so a second Umber started in that
/// window would find it, read it as abandoned too, and offer the same documents
/// again. Two windows on one painting is exactly what the lock exists to
/// prevent; holding it until the offer is answered is what makes the guarantee
/// cover the whole gesture rather than the instant it was tested in.
fn claim_if_abandoned(path: &Path) -> Option<std::fs::File> {
    let file = match std::fs::File::options().read(true).write(true).open(path) {
        Ok(file) => file,
        Err(e) => {
            log::warn!("could not read {}: {e}", path.display());
            return None;
        }
    };
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(std::fs::TryLockError::WouldBlock) => None,
        Err(std::fs::TryLockError::Error(e)) => {
            log::warn!(
                "could not tell whether {} belongs to a running Umber: {e}",
                path.display(),
            );
            None
        }
    }
}

/// One document a session that stopped left a copy of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recoverable {
    /// What the tab was called, so a never-saved document comes back as
    /// "Untitled 3" rather than as the hashed name of its copy.
    pub title: String,
    /// The file the painter chose for it, if it had one. **This** is what a
    /// recovered document's tab points at — never the copy, or a Save would
    /// write into the autosave folder.
    pub original: Option<PathBuf>,
    /// The copy to open.
    pub copy: PathBuf,
    /// How long before the scan the copy was written, from the file's own
    /// modification time — which is exactly what "when was this last
    /// autosaved" means, and is the same reading [`Reaper::expire`] ages a copy
    /// by. Taken once, like [`crate::crash::Report`]'s, rather than recomputed
    /// as the dialog sits on screen.
    pub seconds_ago: u64,
}

impl Recoverable {
    /// The sentence under the title.
    ///
    /// **It says what cannot be known.** A crash box can compare the copy's
    /// revision against the document's, because the panic hook reads a snapshot
    /// of a session that is still in memory. Nothing here can: the session that
    /// wrote this copy is gone, and whatever was painted after it was written
    /// left no record anywhere. Claiming the copy holds everything would be the
    /// one thing this feature must not do, so it says the opposite plainly.
    pub fn note(&self) -> String {
        format!(
            "Autosaved {}. Anything painted after that is not in it.",
            crate::crash::age_phrase(self.seconds_ago),
        )
    }

    /// What opening this one does to the file it belongs to, said before the
    /// click.
    ///
    /// **"Not until you save it" is the load-bearing half.** Somebody clicking
    /// Open is very often deciding *whether* they want this copy, and the
    /// answer to "does opening it replace what I already have?" has to be there
    /// before the click rather than discovered five minutes afterwards. It is
    /// true because [`Candidate::write_own_file`] makes it true — the autosave
    /// writes its own copy and leaves that file alone — rather than because a
    /// dialog says so.
    pub fn destination(&self) -> String {
        match &self.original {
            Some(path) => format!(
                "Save writes back to {}. Nothing is written there until you do.",
                path.display(),
            ),
            None => {
                "This document had never been saved, so Save will ask where to put it.".to_string()
            }
        }
    }
}

/// Everything one start-up has to offer, and which markers it came from.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offer {
    /// The markers this was read out of, to be forgotten once it is answered.
    pub marks: Vec<PathBuf>,
    pub found: Vec<Recoverable>,
    /// Documents that held unsaved work and have no copy anywhere.
    ///
    /// Named rather than passed over, exactly as [`crate::crash::Report`]'s own
    /// `at_risk` is: a box that lists two recoverable documents and says nothing
    /// about the third reads as a promise about the third.
    pub at_risk: Vec<String>,
}

impl Offer {
    /// Whether there is anything to *do* about this, which is the only reason
    /// to put a modal in front of somebody at start-up.
    ///
    /// **`at_risk` alone does not count**, and that is deliberate rather than
    /// an oversight of the rule it answers to. Naming a document with no copy
    /// exists so that a dialog offering two back does not read as a promise
    /// about the third; with nothing offered there is no such promise to
    /// correct, and what is left is a box that says work was lost and gives
    /// nobody anything to click. That box would also be the *common* case: an
    /// operating system restart force-kills applications, and Umber refuses to
    /// close while a document holds unsaved work, so an ordinary reboot leaves
    /// a marker every time.
    pub fn is_empty(&self) -> bool {
        self.found.is_empty()
    }
}

/// Is `candidate` a file an autosave could have written into `copies`?
///
/// The marker is the one thing in this module that hands over a path Umber did
/// not construct in the same breath, and [`Reaper`] already says why "the
/// callers only ever pass internal paths" is not good enough: a later change,
/// or a hand-edited file, makes it false in silence. So the copy an offer opens
/// has to be a name an autosave writes, directly inside the directory autosaves
/// go in — which is also what stops a marker naming the *painter's own*
/// document from having it opened, marked modified and offered back as a copy
/// of itself.
///
/// Compared without canonicalising, unlike `Reaper`'s, and the difference is
/// the stakes: this decides what is *read*, in a directory the user already
/// owns, where `Reaper` decides what is deleted.
fn ours(candidate: &Path, copies: &Path) -> bool {
    is_autosave_name(candidate) && candidate.parent() == Some(copies)
}

/// The rule that turns one dead session's record into what the dialog offers.
///
/// **A pure function of injected readings**, the shape
/// [`crate::update::install::detect`] keeps and for the same reason: it is the
/// only way the interesting cases — a copy that is missing, a copy older than
/// the painter's own file, a clock that has run backwards — are tested at all,
/// and none of them is a state a test can conveniently arrange on a real disk.
///
/// `read` answers "when was this file last written", or `None` where there is
/// no such file. `copies` is the directory an internal copy may be in, and
/// nothing outside it is a candidate — see [`ours`].
pub fn offer_from(
    record: &SessionRecord,
    now: SystemTime,
    copies: &Path,
    read: &dyn Fn(&Path) -> Option<SystemTime>,
) -> (Vec<Recoverable>, Vec<String>) {
    let mut found = Vec::new();
    let mut at_risk = Vec::new();
    for doc in &record.documents {
        let copy = doc
            .copy
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| ours(p, copies));
        let written = copy.as_deref().and_then(read);
        let (Some(copy), Some(written)) = (copy, written) else {
            // No copy, or one whose write never landed. Worth naming only if
            // there was something in it to lose.
            if doc.modified {
                at_risk.push(doc.title.clone());
            }
            continue;
        };
        let original = doc.path.as_deref().map(PathBuf::from);
        // The painter's own file already holds everything this copy does, so
        // there is nothing to offer. Not silence about lost work: whatever was
        // painted after both were written is in neither, and no copy exists
        // that would bring it back.
        let superseded = original
            .as_deref()
            .and_then(read)
            .is_some_and(|own| own + SAME_MOMENT >= written);
        if superseded {
            continue;
        }
        found.push(Recoverable {
            title: doc.title.clone(),
            original,
            copy,
            // A copy dated in the future — a clock put back, a file restored
            // from a backup — reads as "moments ago" rather than as an error.
            seconds_ago: now
                .duration_since(written)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        });
    }
    (found, at_risk)
}

/// Read a marker, or `None` if it cannot be read as one.
///
/// A revision this build does not know is one of the `None`s, and the
/// comparison is the whole of what [`MARK_FORMAT`] is for. Unlike
/// [`crate::crash::Report`]'s — where the reader is always the same executable
/// that wrote the file — a marker genuinely is read across builds: a downgrade,
/// or a portable copy started beside an installed one. A future revision that
/// changed what a field *meant* would otherwise be read straight through.
/// **Read through the handle the lock is held on**, never by opening the path
/// again. Windows' `LockFileEx` locks the file's *bytes*, so a second handle
/// reading a marker this process has just claimed is refused outright — which
/// is a real bug and not a hypothetical: it made every offer come back empty.
/// It is also the property that stops anything reading a marker a *running*
/// Umber is in the middle of rewriting.
fn read_record(file: &mut std::fs::File, path: &Path) -> Option<SessionRecord> {
    use std::io::{Read, Seek};
    let mut text = String::new();
    file.rewind().ok()?;
    file.read_to_string(&mut text).ok()?;
    let record: SessionRecord = serde_json::from_str(&text).ok()?;
    if record.format > MARK_FORMAT {
        log::info!(
            "{} was written by a newer Umber (revision {}); leaving it alone",
            path.display(),
            record.format,
        );
        return None;
    }
    Some(record)
}

/// What every session that did not end cleanly left behind, from `dir`.
///
/// Markers belonging to a *running* Umber are left strictly alone, and the ones
/// that are not are **held** for as long as the offer they produced is on
/// screen — see [`claim_if_abandoned`].
///
/// A dead marker that turns out to offer nothing — because its copies have
/// expired, because its documents were all saved, or because it never got as
/// far as writing one — is forgotten on the spot: it names nothing, so there is
/// nothing to lose, and leaving it would mean the directory filled with markers
/// no dialog would ever answer for. One that could not be **read** is a
/// different case and is kept: it may name copies that are still there, and
/// deleting the only record of them because a power cut caught a rewrite
/// half-done is the one outcome worth a few hundred stale bytes.
fn collect_offer(dir: &Path, now: SystemTime) -> (Offer, Vec<std::fs::File>) {
    let Ok(marks) = Marks::new(dir) else {
        return (Offer::default(), Vec::new());
    };
    let mut offer = Offer::default();
    let mut held = Vec::new();
    // The copies sit in the directory the markers' own is nested in — see
    // [`SESSIONS_DIR`] — so there is still one statement of where they live.
    let copies = dir.parent().unwrap_or(dir);
    let read = |path: &Path| -> Option<SystemTime> {
        std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
    };
    for path in marks.list() {
        let Some(mut lock) = claim_if_abandoned(&path) else {
            continue;
        };
        let Some(record) = read_record(&mut lock, &path) else {
            log::warn!(
                "{} could not be read; leaving it rather than losing what it names",
                path.display(),
            );
            continue;
        };
        let (found, at_risk) = offer_from(&record, now, copies, &read);
        if found.is_empty() && at_risk.is_empty() {
            // Before the removal, and not merely tidiness: Windows keeps a name
            // that still has a handle open on it, so removing first would leave
            // the marker on disk and this would find it again on every start.
            drop(lock);
            if let Err(why) = marks.remove(&path) {
                log::info!("left {} alone: {why}", path.display());
            }
            continue;
        }
        offer.marks.push(path);
        offer.found.extend(found);
        offer.at_risk.extend(at_risk);
        held.push(lock);
    }
    // Two sessions that both died with the same document open name the same
    // copy — the internal name is keyed on the document's own path. Grouped by
    // copy so the duplicates are adjacent, and freshest first within a group so
    // the one `dedup_by` keeps is the newer reading rather than an older
    // session's memory of the same file.
    offer.found.sort_by(|a, b| {
        a.copy
            .cmp(&b.copy)
            .then_with(|| a.seconds_ago.cmp(&b.seconds_ago))
    });
    offer.found.dedup_by(|a, b| a.copy == b.copy);
    // Then into the order a person would want to read them in: what was being
    // painted last, first. Sorting by the copy's *name* would order the list by
    // a hash, which is no order at all.
    offer.found.sort_by(|a, b| {
        a.seconds_ago
            .cmp(&b.seconds_ago)
            .then(a.title.cmp(&b.title))
    });
    offer.at_risk.sort();
    offer.at_risk.dedup();
    (offer, held)
}

// ---------------------------------------------------------------------------
// The scheduler
// ---------------------------------------------------------------------------

/// What the autosave knows about one open document.
#[derive(Debug)]
struct Record {
    /// Where its internal copy goes. Chosen once, on first sight.
    internal: Option<PathBuf>,
    /// When it was last written, or first seen — which is what starts its
    /// clock, so a document opened now is not written the instant it appears.
    last: Instant,
}

/// One layer of a [`Snapshot`], as the file will describe it.
#[derive(Clone, Debug)]
pub struct LayerMeta {
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend: BlendMode,
    /// The texture-array slice holding the pixels, which is what the capture
    /// reads. `None` for a folder, which holds none.
    pub slot: Option<u32>,
    /// The slice holding the layer's mask, when it has one — another slice of
    /// the same array, so it is read by exactly the same capture.
    pub mask: Option<u32>,
    /// The layer's effects, in composite order.
    ///
    /// **Here rather than read off the stack at write time**, and that is the
    /// snapshot rule this struct exists for rather than an incidental choice.
    /// An effect is derived from nothing the capture reads — no slice, no
    /// pixels — so taking it later would be free and would be *wrong*: the
    /// readback spans frames and the encode spans a thread, so a shadow dialled
    /// or switched off in between would produce a file whose parameters and
    /// whose pixels came from different moments. Exactly the reason the names
    /// are here.
    ///
    /// A `Vec` of `Copy` structs, so the clone costs what the names already
    /// cost — and it is built once every few minutes, never on a frame.
    pub effects: Vec<Effect>,
    /// What set this layer's pixels, where they were set rather than painted.
    ///
    /// **Here for exactly the reason [`LayerMeta::effects`] is here**, and the
    /// hazard is one step worse. A record is derived from nothing the capture
    /// reads, so taking it at write time would be free — and a caption re-set
    /// while the readback was crossing frames would produce a file whose record
    /// says one thing and whose pixels say another. That does not merely
    /// misdescribe the layer: `docformat` fingerprints the record against the
    /// image *it* is writing, so the two would agree, and reopening would
    /// re-render the newer text over the older pixels with nothing to say a
    /// mismatch had happened.
    ///
    /// Boxed as [`umber_core::Layer`] boxes it, so a stack of ordinary painted
    /// layers pays a pointer per layer rather than a `TextObject` per layer.
    pub text: Option<Box<TextObject>>,
    pub clipped: bool,
    pub locked: bool,
    pub link: Option<u8>,
    /// How deeply nested, 0 at the top level.
    pub depth: u8,
    /// This entry is a folder: no slot, no pixels, nothing to read back.
    pub folder: bool,
}

/// Everything about one document that its file will carry.
///
/// Owned rather than borrowed from the editor, and taken **once**, at the
/// moment a capture begins. The readback spans several frames and the encode a
/// background thread, so a layer renamed or reordered in between would
/// otherwise produce a file whose names and pixels came from different
/// instants. It is also what lets the scheduler be a plain `&mut` field of the
/// editor while the description of the document is read out of it.
///
/// Built once every few minutes, so the handful of `String`s cost nothing —
/// unlike [`Autosave::next_due`], which runs every frame and allocates nothing.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub id: DocId,
    /// The document's revision now. The tab's dot only comes off if it is still
    /// this when the write lands — see
    /// [`Session::mark_autosaved`](crate::session::Session::mark_autosaved).
    pub revision: u64,
    pub title: String,
    /// The file the painter chose, if the document has one.
    pub path: Option<PathBuf>,
    /// Whether [`Candidate::path`] may be **written**, as against merely named.
    ///
    /// False for exactly one case, and it is the one that would hurt: a
    /// document recovered out of an autosave copy. Its path came from a marker
    /// rather than from the painter opening or saving that file, and what it
    /// holds is by definition not what is at that path — so writing it back on
    /// a timer would replace an artist's picture with a version they had not
    /// asked for, five minutes after clicking Open to see what was in the copy,
    /// and with no history to step back through. The internal copy is still
    /// written, so nothing is lost either way. See [`crate::session::Tab`]'s
    /// `recovered`, which is what clears it: an explicit Save is the painter
    /// choosing that file for this document.
    pub write_own_file: bool,
    pub size: UVec2,
    pub background: Background,
    pub dpi: f32,
    pub active_layer: usize,
    /// The lowest texture-array slice a baked effect may take —
    /// `Editor::effect_slot_base`'s number, taken here for the reason
    /// [`LayerMeta::effects`] is taken here.
    pub effect_base: u32,
    /// Bottom to top, matching `LayerStack`'s own order.
    pub layers: Vec<LayerMeta>,
}

impl Candidate {
    /// The stack as the composite pass takes it, for the flattened preview.
    pub fn draws(&self) -> Vec<umber_render::LayerDraw> {
        self.effected_draws().into_iter().map(|e| e.draw).collect()
    }

    /// The same flattening with each layer's effects beside its draw.
    ///
    /// [`Candidate::draws`] is this with the effects thrown away, so the two
    /// cannot disagree — the arrangement `Editor::effected_draws` keeps for
    /// exactly the same reason, on the live stack rather than on the snapshot.
    ///
    /// **The effects come out of the snapshot and are never read off the stack**,
    /// which is the rule this whole struct exists for: a shadow dialled or
    /// switched off while the readback was crossing frames would produce a file
    /// whose parameters and whose pixels came from different moments.
    pub fn effected_draws(&self) -> Vec<umber_render::LayerEffects<'_>> {
        self.layers
            .iter()
            .enumerate()
            // Exactly what `Editor::effected_draws` does, and for the same
            // reason: a pass-through folder is its contents composited in place,
            // so it contributes nothing but its eye. The flattened preview this
            // builds has to match the screen, so the two rules have to be the
            // same rule.
            .filter_map(|(i, l)| {
                Some(umber_render::LayerEffects {
                    draw: umber_render::LayerDraw {
                        slot: l.slot?,
                        opacity: l.opacity,
                        blend: l.blend.index(),
                        visible: self.effective_visible(i),
                        mask: l.mask,
                        clipped: l.clipped,
                    },
                    effects: &l.effects,
                })
            })
            .collect()
    }

    /// Is this entry drawn, once every folder it is inside has had its say?
    ///
    /// A second reading of `LayerStack::effective_visible`, over the snapshot
    /// rather than over the live stack — which is the point of the snapshot:
    /// the stack may have been renamed, reordered or re-nested since the
    /// capture began, and the file has to describe the instant the pixels came
    /// from. The rule itself is one line and the ancestor walk is the same walk.
    fn effective_visible(&self, index: usize) -> bool {
        let Some(entry) = self.layers.get(index) else {
            return false;
        };
        let mut want = entry.depth;
        entry.visible
            && self.layers[index + 1..].iter().all(|above| {
                if want == 0 || above.depth >= want {
                    return true;
                }
                want = above.depth;
                above.visible
            })
    }

    /// The slices the capture should read: every layer in stack order, then
    /// every mask in the same order.
    ///
    /// Masks last rather than interleaved so that a document with none reads
    /// exactly the list it always did, and so [`Candidate::mask_index`] is
    /// arithmetic rather than a second table.
    pub fn slots(&self) -> Vec<u32> {
        let masks = self.layers.iter().filter_map(|l| l.mask);
        self.layers
            .iter()
            .filter_map(|l| l.slot)
            .chain(masks)
            .collect()
    }

    /// Where entry `index`'s pixels landed in [`Candidate::slots`].
    ///
    /// **Not `index` itself once a document has folders in it.** A folder holds
    /// no slice, so it is not read back and not in the list — and an entry
    /// looked up by its stack position would then be handed the pixels of a
    /// layer below it. That is the autosave's version of the mistake the undo
    /// history's slot-to-position mapping exists to prevent, and it would write
    /// somebody's file with the layers shifted.
    pub fn pixel_index(&self, index: usize) -> Option<usize> {
        self.layers.get(index)?.slot?;
        Some(
            self.layers[..index]
                .iter()
                .filter(|l| l.slot.is_some())
                .count(),
        )
    }

    /// Where layer `index`'s mask landed in [`Candidate::slots`], if it has one.
    pub fn mask_index(&self, index: usize) -> Option<usize> {
        self.layers.get(index)?.mask?;
        let before = self.layers[..index]
            .iter()
            .filter(|l| l.mask.is_some())
            .count();
        // Past every layer slice, which is the count of entries that *have*
        // one — not the entry count, which folders inflate.
        let pixels = self.layers.iter().filter(|l| l.slot.is_some()).count();
        Some(pixels + before)
    }
}

/// A capture being read off the GPU.
#[derive(Debug)]
struct InFlight {
    doc: Candidate,
    /// Where this document's internal copy goes, resolved when the capture
    /// began. `None` on a system with no data directory.
    internal: Option<PathBuf>,
}

/// One document, ready to be encoded and written.
struct Task {
    doc: Candidate,
    internal: Option<PathBuf>,
    pixels: DocumentCapture,
    /// Applied after the write. `None` keeps internal copies for ever.
    expiry: Option<Duration>,
}

/// What the writer thread has to say when it is done.
#[derive(Clone, Debug)]
pub enum Report {
    Written {
        id: DocId,
        revision: u64,
        /// True when the document's *own* file was among the destinations,
        /// which is the only case where the tab's dot may come off.
        wrote_user_file: bool,
        /// How many internal copies the sweep deleted.
        expired: usize,
    },
    /// Something went wrong. Shown to the user once per run and then only
    /// logged: a broken autosave must never become a dialog that keeps
    /// appearing while somebody is trying to paint.
    Failed { title: String, message: String },
}

/// The autosave's whole state: the schedule, what is in flight, and the thread
/// that writes.
pub struct Autosave {
    pub enabled: bool,
    pub interval: Duration,
    /// How long an internal copy is kept. `None` is "for ever".
    pub expiry: Option<Duration>,

    docs: HashMap<DocId, Record>,
    flight: Option<InFlight>,
    /// Distinguishes this run's never-saved documents from a previous run's.
    token: u64,
    /// Numbers the never-saved documents within this run.
    seq: u64,
    /// A failure has already been shown. See [`Report::Failed`].
    complained: bool,
    /// The start-up expiry sweep has run. See [`Autosave::sweep_once`].
    swept: bool,
    /// Where this run's marker goes, and where a previous run's is looked for.
    ///
    /// A field rather than a call to [`sessions_dir`] at the point of use, so a
    /// test can point the whole mechanism at a scratch directory. A test that
    /// wrote a marker into somebody's real data folder would then be found by
    /// their next start of Umber.
    pub marks_dir: Option<PathBuf>,
    /// This run's marker, held open and locked. `None` before
    /// [`Autosave::begin_run`], and on a system with no data directory.
    mark: Option<SessionMark>,
    /// Locks taken over the markers a session that stopped left, held for as
    /// long as the offer they produced is unanswered. See
    /// [`claim_if_abandoned`].
    claimed: Vec<std::fs::File>,
    /// [`Autosave::begin_run`] has run.
    begun: bool,
    /// The reduction of the tab strip the marker was last written from. See
    /// [`Autosave::note_documents`].
    noted: u64,
    /// When the last document was written, for the settings dialog.
    pub last_written: Option<Instant>,

    tx: Option<mpsc::Sender<Task>>,
    rx: Option<mpsc::Receiver<Report>>,
    waker: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for Autosave {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Autosave")
            .field("enabled", &self.enabled)
            .field("interval", &self.interval)
            .field("expiry", &self.expiry)
            .field("in_flight", &self.flight.is_some())
            .finish()
    }
}

impl Default for Autosave {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(DEFAULT_INTERVAL_MINUTES as u64 * 60),
            expiry: Some(Duration::from_secs(DEFAULT_EXPIRY_HOURS as u64 * 3600)),
            docs: HashMap::new(),
            flight: None,
            // Nanoseconds since the epoch: unique per run without a random
            // number generator, and monotonic enough that two runs a
            // millisecond apart still differ.
            token: SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0),
            seq: 0,
            complained: false,
            swept: false,
            marks_dir: sessions_dir(),
            mark: None,
            claimed: Vec::new(),
            begun: false,
            // Nothing has been written down yet, so the first reading of the
            // tab strip has to count as a change whatever it comes to.
            noted: u64::MAX,
            last_written: None,
            tx: None,
            rx: None,
            waker: None,
        }
    }
}

impl Autosave {
    /// Ask to have the event loop woken when a write finishes.
    ///
    /// Under `ControlFlow::Wait` a value appearing in a channel is not an
    /// event, so without this the tab's dot would stay on until the painter
    /// happened to move the mouse. The same arrangement the update check uses.
    pub fn set_waker(&mut self, waker: Arc<dyn Fn() + Send + Sync>) {
        self.waker = Some(waker);
    }

    /// True while a document's pixels are being read off the GPU.
    pub fn capturing(&self) -> bool {
        self.flight.is_some()
    }

    /// The document being captured, if any.
    pub fn capturing_id(&self) -> Option<DocId> {
        self.flight.as_ref().map(|f| f.doc.id)
    }

    /// Which document should be written now, if any.
    ///
    /// `quiet` is the caller's answer to "is the painter between strokes?". An
    /// autosave never starts while it is false, which is what turns "every five
    /// minutes" into "at the first quiet moment after five minutes" — and is
    /// the whole of how this avoids interrupting a stroke.
    ///
    /// Also prunes documents that have been closed, so a long session does not
    /// leak a record per tab ever opened.
    ///
    /// Takes the [`Session`] rather than a list built by the caller because
    /// this runs on **every frame**: a `Vec` of one entry per open document,
    /// allocated and thrown away sixty times a second, is exactly the kind of
    /// per-frame allocation the drawing path does not have anywhere else. What
    /// it needs is only each document's id and whether it holds work; the
    /// [`Candidate`] with everything else in it is built once, when a document
    /// is actually due.
    pub fn next_due(&mut self, now: Instant, quiet: bool, session: &Session) -> Option<DocId> {
        let tabs = session.tabs();
        self.docs.retain(|id, _| tabs.iter().any(|t| t.id == *id));
        for tab in tabs {
            self.docs.entry(tab.id).or_insert(Record {
                internal: None,
                last: now,
            });
        }
        if !self.enabled || !quiet || self.flight.is_some() {
            return None;
        }
        tabs.iter()
            .filter(|t| t.modified)
            .find(|t| {
                self.docs
                    .get(&t.id)
                    .is_some_and(|r| now.duration_since(r.last) >= self.interval)
            })
            .map(|t| t.id)
    }

    /// Note that a capture has started for `doc`.
    pub fn begin(&mut self, doc: Candidate) {
        let internal = self.internal_path_for(&doc);
        self.flight = Some(InFlight { doc, internal });
    }

    /// Give up on the capture in flight — the document is closing, is being
    /// resized, or has just been saved explicitly.
    ///
    /// The renderer's own `cancel_capture` is the caller's to make; this only
    /// forgets what the pixels were going to become.
    pub fn abandon(&mut self) {
        self.flight = None;
    }

    /// Restart a document's clock, because it has just been written by
    /// something else.
    ///
    /// Without this, an explicit Save leaves the document's clock as far behind
    /// as it ever was, and the very next brush stroke triggers a full autosave
    /// of a document that was written a second ago.
    pub fn defer(&mut self, id: DocId, now: Instant) {
        if let Some(record) = self.docs.get_mut(&id) {
            record.last = now;
        }
    }

    /// Sweep the internal directory of anything expired, once per run.
    ///
    /// Driven from the frame loop rather than from start-up because the expiry
    /// setting comes out of the preferences file, which is read on the first
    /// frame — sweeping before that would use the default rather than the
    /// painter's choice, and switching expiry off would not take effect until
    /// the run after.
    pub fn sweep_once(&mut self) {
        if std::mem::replace(&mut self.swept, true) {
            return;
        }
        self.sweep();
    }

    /// Hand a finished capture to the writer thread and start the clock again.
    ///
    /// Returns immediately: the encode is seconds of PNG deflate and none of it
    /// belongs on the frame loop.
    pub fn finish(&mut self, pixels: DocumentCapture, now: Instant) {
        let Some(flight) = self.flight.take() else {
            return;
        };
        if let Some(record) = self.docs.get_mut(&flight.doc.id) {
            record.last = now;
        }
        let task = Task {
            doc: flight.doc,
            internal: flight.internal,
            pixels,
            expiry: self.expiry,
        };
        match self.writer() {
            Some(tx) => {
                if tx.send(task).is_err() {
                    log::error!("the autosave writer has gone; nothing was written");
                }
            }
            None => log::error!("no autosave writer; nothing was written"),
        }
    }

    /// Claim this run's marker, and collect what a session that did not end
    /// cleanly left behind. Once per run.
    ///
    /// The scan comes **before** the marker is created, which is what keeps
    /// this run's own file out of its own answer without having to name it.
    pub fn begin_run(&mut self, now: SystemTime) -> Offer {
        if std::mem::replace(&mut self.begun, true) {
            return Offer::default();
        }
        let Some(dir) = self.marks_dir.clone() else {
            return Offer::default();
        };
        let (offer, held) = collect_offer(&dir, now);
        // Held for as long as the offer is on screen, so a second Umber started
        // while somebody is reading it cannot read the same markers as
        // abandoned and offer the same documents again.
        self.claimed = held;
        match SessionMark::open(&dir, self.token) {
            Ok(mark) => self.mark = Some(mark),
            // Umber carries on without one. What is lost is the *next* start's
            // offer, not anything this session is doing — the copies are
            // written either way, and the folder is one click away in Settings.
            Err(e) => log::warn!("could not mark this session as running: {e}"),
        }
        offer
    }

    /// Take this run's marker down, because the loop finished on purpose.
    ///
    /// Called from exactly one place — see [`crate::UmberApp::ended_cleanly`] —
    /// and deliberately not from a `Drop`: a panic unwinds through destructors,
    /// so a `Drop` here would remove the very evidence that the session did not
    /// end cleanly, in precisely the case this exists for.
    pub fn end_run(&mut self) {
        let Some(mark) = self.mark.take() else {
            return;
        };
        let path = mark.path().to_path_buf();
        // The lock goes with the handle, before the file is removed.
        drop(mark);
        self.forget_marks(std::slice::from_ref(&path));
    }

    /// Forget markers that have been answered, or that this run has taken down.
    ///
    /// Every removal goes through [`Marks`], which cannot reach a document.
    pub fn forget_marks(&mut self, paths: &[PathBuf]) {
        // The offer is answered, so the locks taken over its markers go — and
        // they go *first*, because a handle still open on a file being removed
        // is the one part of this that is not the same on every platform.
        self.claimed.clear();
        let Some(dir) = self.marks_dir.as_deref() else {
            return;
        };
        let Ok(marks) = Marks::new(dir) else {
            return;
        };
        for path in paths {
            if let Err(why) = marks.remove(path) {
                log::info!("left {} alone: {why}", path.display());
            }
        }
    }

    /// Note that this document came out of `copy`, so that is the copy it goes
    /// back to.
    ///
    /// Two things it buys, and both were wrong without it. The marker would
    /// describe a freshly recovered document as having **no copy** until its
    /// first autosave, so a crash in between would put it in the next start's
    /// "Umber had no copy of this one" — while the copy it was recovered from
    /// sat in the folder. And a never-saved document, whose copy is keyed on
    /// this run rather than on a path, would otherwise be autosaved to a second
    /// file beside the one it came out of.
    pub fn adopt_copy(&mut self, id: DocId, copy: PathBuf) {
        self.docs
            .entry(id)
            .or_insert_with(|| Record {
                internal: None,
                last: Instant::now(),
            })
            .internal = Some(copy);
    }

    /// Keep the marker's description of the open documents up to date.
    ///
    /// Called **every frame** and does nothing on almost all of them: the tab
    /// strip and the copies chosen for it are reduced to one number first, and
    /// only a change to it rewrites the file. That is the same bargain
    /// [`crate::crash::note_documents`] makes, and it is what lets this sit on
    /// the drawing path — the reduction allocates nothing, and the rewrite
    /// happens on the handful of frames where a document was opened, closed,
    /// saved, first painted on, or given a copy.
    ///
    /// Keeping it current is not bookkeeping for its own sake: a document
    /// closed or saved after its copy was written must stop being offered, and
    /// this is the only thing that says so.
    pub fn note_documents(&mut self, session: &Session) {
        if self.mark.is_none() {
            return;
        }
        let mark = self.fingerprint(session);
        if self.noted == mark {
            return;
        }
        let record = self.record(session);
        let Some(file) = self.mark.as_mut() else {
            return;
        };
        match file.write(&record) {
            // Only once the write has actually landed, so a failure is retried
            // on the next frame rather than remembered as done.
            Ok(()) => self.noted = mark,
            Err(e) => log::warn!("could not update the session marker: {e}"),
        }
    }

    /// A cheap reading of what the marker would say. Allocates nothing.
    fn fingerprint(&self, session: &Session) -> u64 {
        let mut hash = digest(&(session.len() as u64).to_le_bytes());
        for tab in session.tabs() {
            let mix = |hash: u64, part: u64| hash.rotate_left(7).wrapping_add(part);
            hash = mix(hash, digest(tab.title.as_bytes()));
            hash = mix(
                hash,
                tab.path
                    .as_ref()
                    .map_or(0, |p| digest(p.as_os_str().as_encoded_bytes())),
            );
            hash = mix(hash, u64::from(tab.modified));
            hash = mix(
                hash,
                self.internal_copy(tab.id)
                    .map_or(0, |p| digest(p.as_os_str().as_encoded_bytes())),
            );
        }
        hash
    }

    fn record(&self, session: &Session) -> SessionRecord {
        SessionRecord {
            format: MARK_FORMAT,
            version: env!("CARGO_PKG_VERSION").to_string(),
            documents: session
                .tabs()
                .iter()
                .map(|tab| MarkedDocument {
                    // Bounded, for the reason every field of a crash report is:
                    // this is written from a frame loop and a tab title is a
                    // file name, which has no ceiling of its own.
                    title: tab.title.chars().take(TITLE_LIMIT).collect(),
                    path: tab.path.as_ref().map(|p| p.display().to_string()),
                    copy: self.internal_copy(tab.id).map(|p| p.display().to_string()),
                    modified: tab.modified,
                })
                .collect(),
        }
    }

    /// Delete expired internal copies, without writing anything.
    ///
    /// Called once at start-up, on the writer thread — an expiry sweep reads a
    /// directory and touches a file system, and neither belongs on the frame
    /// that is trying to put a window on screen.
    pub fn sweep(&mut self) {
        let expiry = self.expiry;
        std::thread::Builder::new()
            .name("umber-autosave-sweep".to_owned())
            .spawn(move || sweep_now(expiry))
            .map(|_| ())
            .unwrap_or_else(|e| log::warn!("could not start the autosave sweep: {e}"));
    }

    /// Collect whatever the writer thread has finished.
    ///
    /// Returns at most one message worth showing the user: a failure, once per
    /// run. Everything else goes to the caller as state to apply.
    pub fn poll(&mut self) -> Vec<Report> {
        let Some(rx) = self.rx.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        while let Ok(report) = rx.try_recv() {
            if let Report::Failed { title, message } = &report {
                log::warn!("could not autosave “{title}”: {message}");
                if self.complained {
                    continue;
                }
                self.complained = true;
            }
            out.push(report);
        }
        if out.iter().any(|r| matches!(r, Report::Written { .. })) {
            self.last_written = Some(Instant::now());
        }
        out
    }

    /// Where this document's internal copy has been put, if one has been.
    ///
    /// Read by [`crate::crash`] so a crash box can name the file rather than
    /// the directory. Not the same as [`Autosave::internal_path_for`], which
    /// *chooses* a path and is therefore `&mut`: this only reports one that has
    /// already been decided, so it cannot invent a destination for a document
    /// that was never written.
    pub fn internal_copy(&self, id: DocId) -> Option<&Path> {
        self.docs.get(&id)?.internal.as_deref()
    }

    /// Where this document's internal copy goes, chosen once and remembered.
    fn internal_path_for(&mut self, doc: &Candidate) -> Option<PathBuf> {
        if let Some(existing) = self.docs.get(&doc.id).and_then(|r| r.internal.clone()) {
            return Some(existing);
        }
        let dir = internal_dir()?;
        // A document with a file is keyed on that file, so the same painting
        // always lands on the same internal copy. One that has never been saved
        // is keyed on this run and its own number: two untitled documents in a
        // session are two paintings, and an untitled document a week ago is not
        // this one either.
        let (stem, key) = match &doc.path {
            Some(path) => (
                path.file_stem()
                    .map(|s| stem_of(&s.to_string_lossy()))
                    .unwrap_or_else(|| stem_of(&doc.title)),
                digest(path.to_string_lossy().as_bytes()),
            ),
            None => {
                self.seq += 1;
                (
                    stem_of(&doc.title),
                    digest(&[self.token.to_le_bytes(), self.seq.to_le_bytes()].concat()),
                )
            }
        };
        let path = dir.join(internal_name(&stem, key));
        if let Some(record) = self.docs.get_mut(&doc.id) {
            record.internal = Some(path.clone());
        }
        Some(path)
    }

    /// The background writer, started on first use.
    fn writer(&mut self) -> Option<&mpsc::Sender<Task>> {
        if self.tx.is_none() {
            let (task_tx, task_rx) = mpsc::channel::<Task>();
            let (report_tx, report_rx) = mpsc::channel::<Report>();
            let waker = self.waker.clone();
            let spawned = std::thread::Builder::new()
                .name("umber-autosave".to_owned())
                .spawn(move || {
                    while let Ok(task) = task_rx.recv() {
                        for report in run_task(task) {
                            if report_tx.send(report).is_err() {
                                return;
                            }
                        }
                        if let Some(wake) = &waker {
                            wake();
                        }
                    }
                });
            match spawned {
                Ok(_) => {
                    self.tx = Some(task_tx);
                    self.rx = Some(report_rx);
                }
                Err(e) => {
                    log::warn!("could not start the autosave writer: {e}");
                    return None;
                }
            }
        }
        self.tx.as_ref()
    }
}

// ---------------------------------------------------------------------------
// The frame loop
// ---------------------------------------------------------------------------

/// Everything the autosave does *before* the frame is submitted: start a
/// capture if one is due, and push whichever is in flight along by one step.
///
/// The copy is recorded into the frame's own encoder rather than into one of
/// its own, so it costs one command and no extra submission.
///
/// `quiet` is what stops any of this happening mid-stroke. It is the caller's
/// to decide because only the event loop knows whether the pointer is down.
pub fn drive(
    editor: &mut Editor,
    gpu: &Gpu,
    canvases: &mut HashMap<DocId, CanvasRenderer>,
    encoder: &mut wgpu::CommandEncoder,
    quiet: bool,
) {
    if let Some(id) = editor
        .autosave
        .next_due(Instant::now(), quiet, &editor.session)
        && let Some(doc) = snapshot(editor, id)
    {
        let slots = doc.slots();
        // A document with no renderer has no pixels to read — the resume path
        // rebuilds them, and until then there is nothing worth writing.
        //
        // The draw list is the *baked* one, so `mergedimage.png` shows the
        // effects the screen shows. Nothing is normally rebaked here: the frame
        // this rides in has already baked the same slices from the same
        // parameters, and an autosave only starts with the pointer up and no
        // stroke in flight, so every stamp matches. What the call is for is the
        // slot assignment, which is the renderer's and not the snapshot's.
        if canvases.get_mut(&id).is_some_and(|canvas| {
            let stack = doc.effected_draws();
            let baked = canvas.bake_effects(
                &gpu.device,
                &gpu.queue,
                encoder,
                doc.effect_base,
                &stack,
                umber_render::EffectFrame {
                    // No stroke can be in flight — `next_due` refuses one — so
                    // there is no active draw for the bake to fold a scratch in
                    // for, and `u32::MAX` is what the composite already reads as
                    // "no layer".
                    active_index: u32::MAX,
                    stroke: umber_render::StrokeStyle::default(),
                    stroke_live: false,
                },
            );
            canvas.begin_capture(&slots, &baked.draws)
        }) {
            log::debug!("autosaving “{}”", doc.title);
            editor.autosave.begin(doc);
        }
    }

    if let Some(id) = editor.autosave.capturing_id()
        && let Some(canvas) = canvases.get_mut(&id)
    {
        canvas.drive_capture(&gpu.device, &gpu.queue, encoder);
    }
}

/// Everything the autosave does *after* the frame has been presented: map what
/// was recorded, collect what has come home, and apply whatever the writer
/// thread has finished.
///
/// Returns a message for the user, if there is one worth showing — a failure,
/// once per run. See [`Report::Failed`].
pub fn collect(
    editor: &mut Editor,
    gpu: &Gpu,
    canvases: &mut HashMap<DocId, CanvasRenderer>,
) -> Option<Notice> {
    // Here rather than at start-up: the expiry setting is read from the
    // preferences file on the first frame, and this is after it.
    editor.autosave.sweep_once();

    // The other half of "once, on the first frame": claim this run's marker,
    // and find out whether the last run put one down. Before the sweep would be
    // wrong — expiry can take the very copies an offer is about, and a dialog
    // listing a file that has just been deleted is worse than one listing
    // nothing. The sweep runs on a thread, so this is *ordering*, not a
    // guarantee; `offer_from` reads the file system rather than trusting the
    // marker, so a copy that goes in between is simply not offered.
    let offer = editor.autosave.begin_run(SystemTime::now());
    if !offer.is_empty() {
        log::info!(
            "the last session did not end cleanly: {} copy/copies to offer back, \
             {} document(s) with no copy",
            offer.found.len(),
            offer.at_risk.len(),
        );
        editor.recovery.offer(offer);
    }

    // Every frame, and free on almost all of them. What it costs on the ones
    // where it is not is one small write; what it buys is that a document
    // closed or saved since its copy was written stops being offered back.
    editor.autosave.note_documents(&editor.session);

    if let Some(id) = editor.autosave.capturing_id()
        && let Some(canvas) = canvases.get_mut(&id)
    {
        canvas.submit_capture();
        if let Some(pixels) = canvas.take_capture(&gpu.device) {
            editor.autosave.finish(pixels, Instant::now());
        } else if !canvas.capture_in_flight() {
            // The renderer gave up on it — a resize, or a failed map. Nothing
            // is coming, so the scheduler must stop waiting for it or this
            // document is never autosaved again.
            editor.autosave.abandon();
        }
    }

    let mut notice = None;
    for report in editor.autosave.poll() {
        match report {
            Report::Written {
                id,
                revision,
                wrote_user_file,
                ..
            } => {
                // Only a document whose *own* file was written has nothing left
                // to lose, and only if it has not moved since — an autosave
                // that took the dot off work it had not written would be worse
                // than no autosave at all.
                if wrote_user_file {
                    editor.session.mark_autosaved(id, revision);
                }
                // Where a crash box would send somebody, if the next frame
                // turns out to be the one that fails. The document's own file
                // where that was written — it is what the artist would go
                // looking for — and the internal copy otherwise, which is the
                // only destination a never-saved document has. `revision` is
                // the number the capture began with, so the box can say whether
                // the copy holds everything or is a stroke behind rather than
                // claiming work was safe when it was not.
                let written = wrote_user_file
                    .then(|| editor.session.tab_of(id).and_then(|t| t.path.clone()))
                    .flatten()
                    .or_else(|| editor.autosave.internal_copy(id).map(Path::to_path_buf));
                if let Some(path) = written {
                    crate::crash::note_autosave(id, path, revision);
                }
            }
            Report::Failed { title, message } => {
                notice = Some(Notice {
                    title: format!("Could not autosave “{title}”"),
                    lines: vec![
                        message,
                        "Autosave will keep trying. Your work is not lost. Use \
                         File, Save to write it where you want it."
                            .to_string(),
                    ],
                });
            }
        }
    }
    notice
}

/// Describe one open document as its file would.
///
/// `None` when the tab has gone, which a capture that spans frames has to
/// allow for. The active document's state is live in the editor and every
/// other one is parked in its tab — see [`crate::session`].
fn snapshot(editor: &Editor, id: DocId) -> Option<Candidate> {
    let index = editor.session.tabs().iter().position(|t| t.id == id)?;
    let tab = &editor.session.tabs()[index];
    let (doc, layers) = match editor.session.parked(index) {
        Some(parked) => (&parked.doc, &parked.layers),
        None => (&editor.doc, &editor.layers),
    };
    Some(Candidate {
        id,
        revision: tab.revision,
        title: tab.title.clone(),
        path: tab.path.clone(),
        write_own_file: !tab.recovered,
        size: doc.size,
        background: doc.background,
        dpi: doc.dpi,
        active_layer: layers.active_index(),
        // Snapshotted with everything else, and for the reason everything else
        // here is: the capture spans frames, so a layer added in between would
        // move it and the flattened preview would name effect slices that had
        // been reassigned.
        effect_base: layers.slot_capacity_needed() + 1,
        layers: layers
            .layers()
            .iter()
            .map(|l| LayerMeta {
                name: l.name.clone(),
                visible: l.visible,
                opacity: l.opacity,
                blend: l.blend,
                slot: l.slot(),
                mask: l.mask(),
                // Taken with the names, at the instant the capture begins —
                // see `LayerMeta::effects`.
                effects: l.effects().to_vec(),
                // And the text record with them, for the reason
                // `LayerMeta::text` gives: a record read at write time could
                // describe a caption re-set since the pixels were read, and the
                // fingerprint the writer takes would agree with it.
                text: l.text().cloned().map(Box::new),
                clipped: l.clipped,
                locked: l.locked,
                link: l.link,
                depth: l.depth,
                folder: l.is_folder(),
            })
            .collect(),
    })
}

// ---------------------------------------------------------------------------
// The writer thread
// ---------------------------------------------------------------------------

/// Encode one document and put it wherever it belongs.
fn run_task(task: Task) -> Vec<Report> {
    let Task {
        doc,
        internal,
        pixels,
        expiry,
    } = task;

    // The one thing that could have changed under the capture without being
    // noticed. `CanvasRenderer::resize` cancels a capture in flight, and
    // `apply_canvas` forgets it, so this cannot happen — and a file whose
    // layers were two different sizes would be silently sheared, which is not
    // a failure to leave to "cannot happen".
    if pixels.size != doc.size {
        return vec![Report::Failed {
            title: doc.title,
            message: format!(
                "the canvas changed size while it was being written \
                 ({} x {} against {} x {})",
                pixels.size.x, pixels.size.y, doc.size.x, doc.size.y,
            ),
        }];
    }

    // Zipped by `pixel_index` rather than positionally: a folder is an entry
    // with no slice, so the capture is shorter than the stack and a positional
    // zip would pair every layer above a folder with the pixels of the one
    // below it — and then truncate the top of the stack away entirely.
    let empty: Vec<u8> = Vec::new();
    let layers: Vec<SaveLayer<'_>> = doc
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let px = doc
                .pixel_index(i)
                .and_then(|k| pixels.layers.get(k))
                .map_or(&empty[..], Vec::as_slice);
            SaveLayer {
                visible: l.visible,
                opacity: l.opacity,
                // The masks are the tail of the same capture — see
                // `Candidate::slots`. A mask the capture did not bring back is
                // written as no mask at all rather than as a blank one: an autosave
                // that invented an empty mask would hide the layer it belonged to.
                mask: doc
                    .mask_index(i)
                    .and_then(|k| pixels.layers.get(k))
                    .map(Vec::as_slice),
                // The snapshot's, not the live stack's: this runs on the
                // writer thread, minutes after the document was described.
                effects: &l.effects,
                // The same, and it is the field whose absence is silent: with
                // nothing here `..SaveLayer::new` writes `None`, so a document
                // that opened as text is written back as plain paint by the
                // five-minute timer with no warning anywhere.
                text: l.text.as_deref(),
                clipped: l.clipped,
                locked: l.locked,
                link: l.link,
                depth: l.depth,
                folder: l.folder,
                ..SaveLayer::new(&l.name, l.blend, px)
            }
        })
        .collect();

    // No history: see the module docs. `None` writes exactly the file every
    // build before histories existed wrote, which any Umber can reopen.
    let document = SaveDocument {
        size: pixels.size,
        layers: &layers,
        active: doc.active_layer,
        background: doc.background,
        dpi: doc.dpi,
        merged: &pixels.merged,
        history: None,
    };

    let encoded = match docformat::encode(&document) {
        // Warnings are dropped rather than shown. They say the same thing on
        // every autosave of the same document, and an explicit Save already
        // reports them — a notice raised by a timer would be a dialog appearing
        // over somebody's canvas every five minutes.
        Ok((bytes, _)) => bytes,
        Err(e) => {
            return vec![Report::Failed {
                title: doc.title,
                message: e.to_string(),
            }];
        }
    };

    let mut reports = Vec::new();
    let mut wrote_user_file = false;

    // The internal copy first. It is the one that exists for every document,
    // saved or not, and writing it before the painter's own file means a
    // failure to replace theirs still leaves a recoverable copy somewhere.
    if let Some(path) = &internal
        && let Err(message) = write_internal(path, &encoded)
    {
        reports.push(Report::Failed {
            title: doc.title.clone(),
            message,
        });
    }

    // Then the document's own file, which this **overwrites without asking**.
    // That is what an autosave is; the alternative is a dialog on a timer.
    //
    // Gated, and on exactly one thing: a document recovered out of a copy has a
    // path nobody has saved to. Overwriting *without asking* is right where the
    // painter put the document at that path themselves and is not where Umber
    // did. See `Candidate::write_own_file`.
    if let Some(path) = &doc.path
        && doc.write_own_file
    {
        match docformat::write_encoded(path, &encoded) {
            Ok(()) => {
                wrote_user_file = true;
                log::debug!("autosaved {}", path.display());
            }
            Err(e) => reports.push(Report::Failed {
                title: doc.title.clone(),
                message: format!("{}: {e}", path.display()),
            }),
        }
    }

    // Swept against the directory the internal copy was *just written to*,
    // rather than against a directory named separately. It is a small thing and
    // it is the same principle as `Reaper` itself: the only place expiry can
    // reach is the place Umber puts its own copies, and there is no second
    // statement of where that is to drift.
    let expired = match (&internal, expiry) {
        (Some(path), Some(max_age)) => path.parent().map(|d| sweep_with(d, max_age)).unwrap_or(0),
        _ => 0,
    };
    reports.push(Report::Written {
        id: doc.id,
        revision: doc.revision,
        wrote_user_file,
        expired,
    });
    reports
}

fn write_internal(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent()
        && let Err(e) = std::fs::create_dir_all(dir)
    {
        return Err(format!("{} could not be created: {e}", dir.display()));
    }
    docformat::write_encoded(path, bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    log::debug!("autosave copy at {}", path.display());
    Ok(())
}

/// One expiry sweep of `dir`. Returns how many were deleted.
fn sweep_with(dir: &Path, max_age: Duration) -> usize {
    // A directory that is not there is one with nothing to expire, which is the
    // ordinary state before the first autosave.
    let Ok(reaper) = Reaper::new(dir) else {
        return 0;
    };
    let done = reaper.expire(max_age, SystemTime::now());
    for (path, why) in &done.refused {
        // Logged rather than swallowed. A refusal here is either a foreign file
        // somebody dropped in the directory or a link that would have led out
        // of it, and both are worth being able to see.
        log::info!("left {} alone: {why}", path.display());
    }
    if done.deleted > 0 {
        log::info!("expired {} internal autosave(s)", done.deleted);
    }
    done.deleted
}

fn sweep_now(expiry: Option<Duration>) {
    if let Some(max_age) = expiry
        && let Some(dir) = internal_dir()
    {
        sweep_with(&dir, max_age);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory of this test's own.
    ///
    /// Keyed by process id as well as by name, because the name alone is the
    /// same path in every checkout: several worktrees running `cargo test` at
    /// once — which is how this project is worked on — then share one
    /// directory, and the `remove_dir_all` below wipes it out from under
    /// another run. The symptom is a reaper reporting `Access is denied
    /// (os error 5)` on a file it did create, which reads as a bug in the
    /// code under test rather than in the harness.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("umber-autosave-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        dir
    }

    fn touch(path: &Path, age: Duration) {
        std::fs::write(path, b"not really an archive").expect("write");
        // `set_modified` rather than waiting: an expiry test that slept for its
        // own threshold would be a test nobody runs.
        let when = SystemTime::now() - age;
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open")
            .set_modified(when)
            .expect("set mtime");
    }

    #[test]
    fn a_reaper_refuses_a_path_outside_its_root() {
        // The rule the whole feature rests on. Expiry must never be able to
        // reach a file the painter chose the location of, and it must be
        // refused *structurally* — not because today's callers happen to pass
        // internal paths only.
        let root = scratch("root");
        let elsewhere = scratch("elsewhere");
        let painting = elsewhere.join("hands.ora");
        std::fs::write(&painting, b"an afternoon").expect("write");

        let reaper = Reaper::new(&root).expect("reaper");
        assert_eq!(reaper.remove(&painting), Err(Refused::Outside));
        assert!(
            painting.exists(),
            "the reaper deleted a file outside itself"
        );

        // And the ways round it: a relative path, a walk up and back down, and
        // a name inside a subdirectory of the root.
        let up = root.join("..").join(
            elsewhere
                .file_name()
                .map(std::ffi::OsStr::to_os_string)
                .expect("a name"),
        );
        assert_eq!(reaper.remove(&up.join("hands.ora")), Err(Refused::Outside));
        assert!(painting.exists());

        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("nested");
        let inner = nested.join("deep.ora");
        std::fs::write(&inner, b"still not yours").expect("write");
        assert_eq!(
            reaper.remove(&inner),
            Err(Refused::Outside),
            "the reaper must not descend"
        );
        assert!(inner.exists());

        // A directory is not a candidate either, whatever it is called.
        let dressed = root.join("looks-like-one.ora");
        std::fs::create_dir_all(&dressed).expect("directory");
        assert_eq!(reaper.remove(&dressed), Err(Refused::NotAPlainFile));
        assert!(dressed.exists());

        // Nor is a file that is not one of ours.
        let foreign = root.join("notes.txt");
        std::fs::write(&foreign, b"hello").expect("write");
        assert_eq!(reaper.remove(&foreign), Err(Refused::NotAnAutosave));
        assert!(foreign.exists());

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn a_documents_own_file_survives_its_internal_copy_expiring() {
        // The case the rule exists for: a painting saved where the painter put
        // it, plus the internal copy Umber keeps beside it. Expiry takes the
        // copy and must not so much as look at the original.
        let internal = scratch("expiry-internal");
        let documents = scratch("expiry-documents");

        let theirs = documents.join("hands.ora");
        touch(&theirs, Duration::from_secs(400 * 24 * 3600));
        let ours = internal.join("hands-0123456789abcdef.ora");
        touch(&ours, Duration::from_secs(400 * 24 * 3600));

        let reaper = Reaper::new(&internal).expect("reaper");
        let done = reaper.expire(Duration::from_secs(30 * 24 * 3600), SystemTime::now());

        assert_eq!(done.deleted, 1, "{done:?}");
        assert!(!ours.exists(), "the internal copy should have expired");
        assert!(
            theirs.exists(),
            "expiry reached a file the painter chose the location of"
        );

        let _ = std::fs::remove_dir_all(&internal);
        let _ = std::fs::remove_dir_all(&documents);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_directory_is_refused_rather_than_followed() {
        // The subtle way a contained reaper stops being contained. A symbolic
        // link inside its own directory, pointing at a painting outside it,
        // looks like an ordinary candidate to anything that does not resolve.
        let internal = scratch("link-internal");
        let documents = scratch("link-documents");
        let painting = documents.join("hands.ora");
        touch(&painting, Duration::from_secs(400 * 24 * 3600));

        let link = internal.join("hands-aaaaaaaaaaaaaaaa.ora");
        std::os::unix::fs::symlink(&painting, &link).expect("symlink");

        let reaper = Reaper::new(&internal).expect("reaper");
        assert_eq!(reaper.remove(&link), Err(Refused::NotAPlainFile));

        let done = reaper.expire(Duration::from_secs(30 * 24 * 3600), SystemTime::now());
        assert_eq!(done.deleted, 0, "{done:?}");
        assert!(painting.exists(), "expiry followed a link out of its root");
        assert!(link.exists());

        let _ = std::fs::remove_dir_all(&internal);
        let _ = std::fs::remove_dir_all(&documents);
    }

    #[test]
    fn only_what_is_actually_old_is_expired() {
        let internal = scratch("age");
        let old = internal.join("old-1111111111111111.ora");
        let fresh = internal.join("fresh-2222222222222222.ora");
        let leftover = internal.join("old-3333333333333333.ora.saving");
        touch(&old, Duration::from_secs(31 * 24 * 3600));
        touch(&fresh, Duration::from_secs(60));
        touch(&leftover, Duration::from_secs(31 * 24 * 3600));

        let reaper = Reaper::new(&internal).expect("reaper");
        let done = reaper.expire(Duration::from_secs(30 * 24 * 3600), SystemTime::now());

        assert_eq!(done.deleted, 2, "{done:?}");
        assert!(!old.exists());
        assert!(
            !leftover.exists(),
            "a temporary left by a write that died should expire too"
        );
        assert!(fresh.exists(), "a fresh autosave was deleted");

        let _ = std::fs::remove_dir_all(&internal);
    }

    #[test]
    fn a_file_dated_in_the_future_is_kept_rather_than_treated_as_ancient() {
        // A clock put back, or a copy restored from a backup. Leaning towards
        // keeping is the only defensible direction for something that deletes.
        let internal = scratch("future");
        let path = internal.join("ahead-4444444444444444.ora");
        std::fs::write(&path, b"tomorrow").expect("write");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("open")
            .set_modified(SystemTime::now() + Duration::from_secs(3600))
            .expect("set mtime");

        let reaper = Reaper::new(&internal).expect("reaper");
        let done = reaper.expire(Duration::from_secs(1), SystemTime::now());
        assert_eq!(done.deleted, 0, "{done:?}");
        assert!(path.exists());

        let _ = std::fs::remove_dir_all(&internal);
    }

    /// A marker read the way a start-up reads one: through a handle of its
    /// own, no lock involved, because in a test nothing is holding it.
    fn record_at(path: &Path) -> Option<SessionRecord> {
        let mut file = std::fs::File::options().read(true).open(path).ok()?;
        read_record(&mut file, path)
    }

    fn candidate(id: DocId, title: &str) -> Candidate {
        Candidate {
            id,
            revision: 0,
            title: title.to_string(),
            path: None,
            write_own_file: true,
            size: UVec2::splat(8),
            background: Background::Transparent,
            dpi: 72.0,
            active_layer: 0,
            effect_base: 2,
            layers: vec![LayerMeta {
                name: "Layer 1".to_string(),
                visible: true,
                opacity: 1.0,
                blend: BlendMode::Normal,
                slot: Some(0),
                depth: 0,
                folder: false,
                mask: None,
                effects: Vec::new(),
                text: None,
                clipped: false,
                locked: false,
                link: None,
            }],
        }
    }

    /// A session of `count` documents, every one of them holding work.
    fn session_of(count: usize) -> Session {
        let mut session = Session::default();
        session.mark_modified();
        for i in 1..count {
            session.open(
                format!("Untitled {}", i + 1),
                None,
                crate::session::DocumentState::blank(umber_core::Document::new(8, 8)),
            );
            session.mark_modified();
        }
        session
    }

    #[test]
    fn nothing_is_due_until_the_interval_has_passed() {
        let session = session_of(1);
        let id = session.active_id();
        let mut autosave = Autosave {
            interval: Duration::from_secs(300),
            ..Autosave::default()
        };
        let start = Instant::now();

        assert_eq!(
            autosave.next_due(start, true, &session),
            None,
            "the clock starts when the document is first seen"
        );
        assert_eq!(
            autosave.next_due(start + Duration::from_secs(299), true, &session),
            None
        );
        assert_eq!(
            autosave.next_due(start + Duration::from_secs(300), true, &session),
            Some(id)
        );
    }

    #[test]
    fn an_autosave_waits_for_a_quiet_moment_rather_than_interrupting_a_stroke() {
        // The whole of how this avoids dropping a stroke: the timer decides
        // when it is *wanted*, and the pointer decides when it may happen.
        let session = session_of(1);
        let id = session.active_id();
        let mut autosave = Autosave {
            interval: Duration::from_secs(1),
            ..Autosave::default()
        };
        let start = Instant::now();
        autosave.next_due(start, true, &session);

        let later = start + Duration::from_secs(60);
        assert_eq!(
            autosave.next_due(later, false, &session),
            None,
            "an autosave started in the middle of a stroke"
        );
        assert_eq!(autosave.next_due(later, true, &session), Some(id));
    }

    #[test]
    fn a_document_with_nothing_to_lose_is_left_alone() {
        let session = Session::default();
        let mut autosave = Autosave::default();
        let start = Instant::now();
        autosave.next_due(start, true, &session);
        assert_eq!(
            autosave.next_due(start + Duration::from_secs(3600), true, &session),
            None
        );
    }

    #[test]
    fn switching_it_off_stops_it_dead() {
        let session = session_of(1);
        let mut autosave = Autosave {
            enabled: false,
            ..Autosave::default()
        };
        let start = Instant::now();
        autosave.next_due(start, true, &session);
        assert_eq!(
            autosave.next_due(start + Duration::from_secs(3600), true, &session),
            None
        );
    }

    #[test]
    fn every_open_document_gets_its_turn() {
        // One at a time, so two documents never contend for the GPU readback —
        // but both are written, which is the point of doing this per document
        // rather than only for the tab in front.
        let session = session_of(2);
        let first = session.tabs()[0].id;
        let second = session.tabs()[1].id;
        let mut autosave = Autosave {
            interval: Duration::from_secs(60),
            ..Autosave::default()
        };
        let start = Instant::now();
        autosave.next_due(start, true, &session);

        let due = start + Duration::from_secs(61);
        assert_eq!(autosave.next_due(due, true, &session), Some(first));
        autosave.begin(candidate(first, "Untitled 1"));
        assert_eq!(
            autosave.next_due(due, true, &session),
            None,
            "a second capture was started while one was in flight"
        );
        autosave.abandon();
        // With the first one's clock restarted, the second is the one due.
        autosave.docs.get_mut(&first).expect("a record").last = due;
        assert_eq!(autosave.next_due(due, true, &session), Some(second));
    }

    #[test]
    fn a_closed_document_is_forgotten() {
        let mut session = session_of(2);
        let mut autosave = Autosave::default();
        let now = Instant::now();
        autosave.next_due(now, true, &session);
        assert_eq!(autosave.docs.len(), 2);

        session.remove(0);
        autosave.next_due(now, true, &session);
        assert_eq!(autosave.docs.len(), 1, "a closed document was remembered");
    }

    #[test]
    fn one_documents_internal_copy_keeps_its_name() {
        // The same painting has to land on the same file every time, or the
        // directory fills with copies of one document and the expiry sweep
        // becomes the only thing keeping it in check.
        let session = session_of(1);
        let id = session.active_id();
        let mut autosave = Autosave::default();
        autosave.next_due(Instant::now(), true, &session);

        let candidate = candidate(id, "Untitled 1");
        let first = autosave.internal_path_for(&candidate);
        let second = autosave.internal_path_for(&candidate);
        assert_eq!(first, second);
        // A machine with no data directory must produce a document with no
        // internal copy rather than a panic.
        if let Some(path) = first {
            assert!(path.to_string_lossy().ends_with(".ora"), "{path:?}");
        }
    }

    #[test]
    fn a_documents_own_path_decides_its_internal_name_across_runs() {
        // Two `Autosave`s stand in for two runs of the application. A document
        // reopened tomorrow has to autosave over yesterday's copy rather than
        // beside it.
        let session = session_of(1);
        let id = session.active_id();
        let path = PathBuf::from("/work/studies/hands.ora");
        let mut candidate = candidate(id, "hands.ora");
        candidate.path = Some(path);

        let mut monday = Autosave::default();
        monday.next_due(Instant::now(), true, &session);
        let mut tuesday = Autosave::default();
        tuesday.next_due(Instant::now(), true, &session);

        assert_eq!(
            monday.internal_path_for(&candidate),
            tuesday.internal_path_for(&candidate),
        );
    }

    #[test]
    fn two_never_saved_documents_do_not_share_one_internal_copy() {
        let session = session_of(2);
        let first = session.tabs()[0].id;
        let second = session.tabs()[1].id;
        let mut autosave = Autosave::default();
        autosave.next_due(Instant::now(), true, &session);
        let a = autosave.internal_path_for(&candidate(first, "Untitled 1"));
        let b = autosave.internal_path_for(&candidate(second, "Untitled 2"));
        assert_ne!(a, b);
    }

    /// A one-pixel document's worth of pixels, as the capture hands them over.
    fn one_pixel_capture() -> DocumentCapture {
        DocumentCapture {
            size: UVec2::ONE,
            layers: vec![vec![200, 40, 40, 255]],
            merged: vec![200, 40, 40, 255],
        }
    }

    #[test]
    fn a_saved_document_is_written_to_its_own_file_and_to_the_internal_copy() {
        // Both destinations, one encode, and both openable afterwards. The
        // painter's own file is *overwritten without asking* — that is what an
        // autosave is, and this is the test that says so out loud.
        let internal = scratch("write-internal");
        let documents = scratch("write-documents");
        let theirs = documents.join("hands.ora");
        std::fs::write(&theirs, b"the previous version").expect("write");

        let mut doc = candidate(Session::default().active_id(), "hands.ora");
        doc.path = Some(theirs.clone());
        doc.size = UVec2::ONE;
        let ours = internal.join("hands-5555555555555555.ora");

        let reports = run_task(Task {
            doc,
            internal: Some(ours.clone()),
            pixels: one_pixel_capture(),
            expiry: None,
        });

        assert!(
            reports.iter().all(|r| !matches!(r, Report::Failed { .. })),
            "{reports:?}",
        );
        assert!(matches!(
            reports.last(),
            Some(Report::Written {
                wrote_user_file: true,
                ..
            })
        ));
        // Openable by Umber's own reader, which is the only test of a saved
        // file that means anything.
        assert!(umber_core::docimport::import(&theirs).is_ok());
        assert!(umber_core::docimport::import(&ours).is_ok());
        // And the temporary neighbour the atomic write goes through is gone.
        assert!(!theirs.with_extension("ora.saving").exists());
        assert!(!ours.with_extension("ora.saving").exists());

        let _ = std::fs::remove_dir_all(&internal);
        let _ = std::fs::remove_dir_all(&documents);
    }

    /// **The autosave writes a layer's effects, and both writers had to be
    /// wired in the same change.**
    ///
    /// `app.rs`'s Save and this are the two places a `SaveLayer` is built, and
    /// both take it through `..SaveLayer::new(…)` — which *defaults*
    /// `effects` to empty. So dropping the field from either still compiles,
    /// and the failure is silent: a document opens with its effects and is
    /// written back without them. Wiring only Save would have been worse than
    /// wiring neither, because then survival would depend on which path last
    /// touched the file — Save keeping them and the autosave stripping them
    /// five minutes later, unattended, which is not a rule an artist can
    /// learn. Hence a runtime guard on each; this is the autosave's.
    #[test]
    fn the_autosave_writes_the_effects_the_snapshot_was_taken_with() {
        let internal = scratch("effects-internal");
        let mut doc = candidate(Session::default().active_id(), "Untitled 1");
        doc.size = UVec2::ONE;
        doc.layers[0].effects = vec![Effect::drop_shadow(), Effect::outline()];
        let ours = internal.join("Untitled 1-7777777777777777.ora");

        let reports = run_task(Task {
            doc,
            internal: Some(ours.clone()),
            pixels: one_pixel_capture(),
            expiry: None,
        });
        assert!(
            reports.iter().all(|r| !matches!(r, Report::Failed { .. })),
            "{reports:?}"
        );

        let back = umber_core::docimport::import(&ours).expect("reopen the copy");
        assert_eq!(
            back.layers[0].effects,
            vec![Effect::drop_shadow(), Effect::outline()],
            "an autosave dropped the layer's effects"
        );

        let _ = std::fs::remove_dir_all(&internal);
    }

    /// **The effects come off the snapshot, taken when the capture began.**
    ///
    /// The readback spans frames and the encode spans a thread, so anything
    /// read at write time can have moved since the pixels did. Names are
    /// snapshotted for exactly this reason and effects join them — a shadow
    /// dialled or switched off during the readback would otherwise produce a
    /// file whose parameters and whose pixels came from different moments.
    ///
    /// Effects are the tempting exception, because they are derived from
    /// nothing the capture reads: taking them later would cost nothing and be
    /// wrong anyway. That is what this pins.
    #[test]
    fn effects_are_snapshotted_when_the_capture_begins_not_when_it_is_written() {
        let mut editor = Editor::default();
        let id = editor.session.active_id();
        assert!(editor.layers.set_effect(0, Effect::drop_shadow()));

        let taken = snapshot(&editor, id).expect("the tab is open");
        assert_eq!(taken.layers[0].effects, vec![Effect::drop_shadow()]);

        // What an artist may do while the readback is still running.
        let louder = Effect {
            distance: 99.0,
            ..Effect::drop_shadow()
        };
        assert!(editor.layers.set_effect(0, louder));
        assert!(editor.layers.set_effect(0, Effect::outline()));

        assert_eq!(
            taken.layers[0].effects,
            vec![Effect::drop_shadow()],
            "the snapshot followed the document instead of holding its instant"
        );
    }

    /// A text record for a layer of the fixture above.
    fn a_record(caption: &str) -> TextObject {
        use umber_core::text::{Align, TextBlock};
        use umber_core::textobj::{Placement, TextFace};
        TextObject::new(
            TextBlock {
                text: caption.to_string(),
                size: 48.0,
                line_spacing: 1.2,
                tracking: 0.0,
                align: Align::Left,
            },
            TextFace {
                family: "Archivo".into(),
                style: "Regular".into(),
                postscript: String::new(),
            },
            umber_core::Color::from_srgb_u8(20, 20, 20, 255),
            Placement::identity(umber_core::PixelRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            }),
        )
    }

    /// **The autosave writes a layer's text record, and both writers had to be
    /// wired in the same change** — the text twin of
    /// `the_autosave_writes_the_effects_the_snapshot_was_taken_with`, and worse
    /// in one respect.
    ///
    /// `SaveLayer::text` defaults to `None`, so a writer that does not name it
    /// writes a text layer back as plain paint. Nothing reports that: a record
    /// that was never offered is not one that failed to be written, and
    /// `umber-text` deliberately did not raise `umber-version`, so the file that
    /// comes out is one every build reads happily with a caption that can no
    /// longer be set again. Wiring Save alone would have been worse than wiring
    /// neither, because then survival would depend on which of the two last
    /// touched the file — losing something every five minutes is a bug somebody
    /// reports, losing it sometimes is one they doubt themselves over.
    #[test]
    fn the_autosave_writes_the_text_the_snapshot_was_taken_with() {
        let internal = scratch("text-internal");
        let mut doc = candidate(Session::default().active_id(), "Untitled 1");
        doc.size = UVec2::ONE;
        let record = a_record("A caption");
        doc.layers[0].text = Some(Box::new(record.clone()));
        let ours = internal.join("Untitled 1-7777777777777778.ora");

        let reports = run_task(Task {
            doc,
            internal: Some(ours.clone()),
            pixels: one_pixel_capture(),
            expiry: None,
        });
        assert!(
            reports.iter().all(|r| !matches!(r, Report::Failed { .. })),
            "{reports:?}"
        );

        let back = umber_core::docimport::import(&ours).expect("reopen the copy");
        assert_eq!(
            back.layers[0].text.as_deref(),
            Some(&record),
            "an autosave wrote a text layer back as plain paint"
        );

        let _ = std::fs::remove_dir_all(&internal);
    }

    /// **The record comes off the snapshot, taken when the capture began** —
    /// the text twin of `effects_are_snapshotted_when_the_capture_begins_not_
    /// when_it_is_written`, and the case where reading late is not merely
    /// inconsistent but undetectable.
    ///
    /// `docformat` fingerprints the record against the layer image *it* is
    /// writing. So a caption re-set while the readback was crossing frames
    /// would be written beside the older pixels with a fingerprint that agrees
    /// with them, and the guard that exists to catch a record describing pixels
    /// it did not make would pass. Reopening would then re-render the newer text
    /// over the older picture with nothing anywhere to say so.
    #[test]
    fn text_is_snapshotted_when_the_capture_begins_not_when_it_is_written() {
        let mut editor = Editor::default();
        let id = editor.session.active_id();
        assert!(editor.layers.set_text(0, a_record("As it was")));

        let taken = snapshot(&editor, id).expect("the tab is open");
        assert_eq!(
            taken.layers[0].text.as_deref(),
            Some(&a_record("As it was"))
        );

        // What an artist may do while the readback is still running.
        assert!(editor.layers.set_text(0, a_record("Set again")));
        assert!(editor.layers.take_text(0).is_some());

        assert_eq!(
            taken.layers[0].text.as_deref(),
            Some(&a_record("As it was")),
            "the snapshot followed the document instead of holding its instant"
        );
    }

    #[test]
    fn a_never_saved_document_goes_only_to_the_internal_copy() {
        // There is nowhere else it could go. Umber has not been told where the
        // painter wants it, and putting a file in their documents folder
        // uninvited is not an answer.
        let internal = scratch("untitled-internal");
        let mut doc = candidate(Session::default().active_id(), "Untitled 1");
        doc.size = UVec2::ONE;
        let ours = internal.join("Untitled 1-6666666666666666.ora");

        let reports = run_task(Task {
            doc,
            internal: Some(ours.clone()),
            pixels: one_pixel_capture(),
            expiry: None,
        });

        assert!(matches!(
            reports.last(),
            Some(Report::Written {
                wrote_user_file: false,
                ..
            })
        ));
        assert!(umber_core::docimport::import(&ours).is_ok());

        let _ = std::fs::remove_dir_all(&internal);
    }

    #[test]
    fn expiry_after_a_write_takes_the_old_copies_and_leaves_the_painters_file() {
        // The whole feature end to end, in the shape that would hurt: an old
        // document of the painter's sitting in their own folder, an old
        // internal copy of it, and an autosave that runs the expiry sweep.
        let internal = scratch("e2e-internal");
        let documents = scratch("e2e-documents");

        let theirs = documents.join("hands.ora");
        touch(&theirs, Duration::from_secs(400 * 24 * 3600));
        let stale = internal.join("something-old-7777777777777777.ora");
        touch(&stale, Duration::from_secs(400 * 24 * 3600));

        let mut doc = candidate(Session::default().active_id(), "hands.ora");
        doc.path = Some(theirs.clone());
        doc.size = UVec2::ONE;

        let reports = run_task(Task {
            doc,
            internal: Some(internal.join("hands-8888888888888888.ora")),
            pixels: one_pixel_capture(),
            expiry: Some(Duration::from_secs(30 * 24 * 3600)),
        });

        assert!(matches!(
            reports.last(),
            Some(Report::Written { expired: 1, .. })
        ));
        assert!(!stale.exists(), "the old internal copy should have gone");
        assert!(
            theirs.exists(),
            "expiry reached a file the painter chose the location of"
        );
        // And it is the *new* document, not the ancient one that was there.
        assert!(umber_core::docimport::import(&theirs).is_ok());

        let _ = std::fs::remove_dir_all(&internal);
        let _ = std::fs::remove_dir_all(&documents);
    }

    /// **The one that would cost somebody a painting.** A document recovered
    /// out of a copy carries the file the painter chose, so that Save writes
    /// where they expect — and the timer must not, because they may have opened
    /// the copy only to look at it. Five minutes later, unasked, with no undo
    /// history in the copy to step back through, is the worst shape this could
    /// take.
    #[test]
    fn a_recovered_document_is_not_written_back_to_the_file_it_names() {
        let internal = scratch("recovered-internal");
        let documents = scratch("recovered-documents");
        let theirs = documents.join("hands.ora");
        std::fs::write(&theirs, b"what the painter has").expect("write");

        let mut doc = candidate(Session::default().active_id(), "hands.ora");
        doc.path = Some(theirs.clone());
        doc.write_own_file = false;
        doc.size = UVec2::ONE;
        let ours = internal.join("hands-5555555555555555.ora");

        let reports = run_task(Task {
            doc,
            internal: Some(ours.clone()),
            pixels: one_pixel_capture(),
            expiry: None,
        });

        assert!(
            reports.iter().all(|r| !matches!(r, Report::Failed { .. })),
            "{reports:?}",
        );
        assert!(
            matches!(
                reports.last(),
                Some(Report::Written {
                    wrote_user_file: false,
                    ..
                })
            ),
            "{reports:?}",
        );
        assert_eq!(
            std::fs::read(&theirs).expect("read"),
            b"what the painter has",
            "an autosave replaced a file nobody had saved this document to",
        );
        // And the copy is still written, so nothing is lost by holding back.
        assert!(umber_core::docimport::import(&ours).is_ok());

        let _ = std::fs::remove_dir_all(&internal);
        let _ = std::fs::remove_dir_all(&documents);
    }

    #[test]
    fn a_capture_of_the_wrong_size_is_refused_rather_than_written_sheared() {
        // Cannot happen — a resize cancels the capture on both sides — and a
        // file whose layers were two different sizes would be silently wrong,
        // which is not a failure to leave to "cannot happen".
        let internal = scratch("sheared");
        let mut doc = candidate(Session::default().active_id(), "Untitled 1");
        doc.size = UVec2::splat(4);

        let reports = run_task(Task {
            doc,
            internal: Some(internal.join("u-9999999999999999.ora")),
            pixels: one_pixel_capture(),
            expiry: None,
        });
        assert!(
            matches!(reports.as_slice(), [Report::Failed { .. }]),
            "{reports:?}",
        );
        assert!(std::fs::read_dir(&internal).into_iter().flatten().count() == 0);

        let _ = std::fs::remove_dir_all(&internal);
    }

    /// The whole feature through the frame loop, on a real device.
    ///
    /// Every part of this is tested on its own — the capture against the
    /// blocking readback in `umber-render`, the scheduler above, the writing
    /// below. What is left is the wiring in [`drive`] and [`collect`], and it
    /// is exactly the seam where a mistake means "autosave silently never
    /// fires", which no unit test would notice.
    ///
    /// Skips rather than fails with no adapter, like the GPU tests.
    #[test]
    fn a_frame_loop_writes_the_document_out_by_itself() {
        // Shared with every other test here that wants a device, and held for
        // the length of this one. Two of them each building their own crashed
        // the binary on the way out on ARM64 Windows — see `crate::gputest`.
        let Some((gpu, _serial)) = crate::gputest::lock() else {
            eprintln!("no GPU adapter available; skipping");
            return;
        };

        let documents = scratch("loop-documents");
        let internal = scratch("loop-internal");
        let theirs = documents.join("hands.ora");

        let mut editor = Editor::default();
        editor.doc = umber_core::Document::new(8, 8);
        // A document that has a file and has been painted on since — the case
        // where an autosave writes both destinations.
        editor.session.mark_saved(theirs.clone());
        editor.session.mark_modified();

        let id = editor.session.active_id();
        let mut canvas = CanvasRenderer::new(
            &gpu.device,
            editor.doc.size,
            wgpu::TextureFormat::Rgba8Unorm,
            editor.layers.slot_capacity_needed(),
        );
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        canvas.clear_all_layers(&mut enc);
        canvas.clear_stroke(&mut enc);
        gpu.queue.submit(Some(enc.finish()));
        let mut canvases = HashMap::from([(id, canvas)]);

        // Due immediately, and pointed at a scratch directory rather than at
        // the real one — a test must not write into somebody's data folder.
        editor.autosave.interval = Duration::ZERO;
        editor.autosave.expiry = None;
        // Same rule for the session marker, and it matters more: a marker left
        // in the real directory would be found by this machine's next start of
        // Umber and offered to somebody as a document to recover.
        editor.autosave.marks_dir = Some(scratch("loop-sessions"));
        editor
            .autosave
            .next_due(Instant::now(), true, &editor.session);
        editor
            .autosave
            .docs
            .get_mut(&id)
            .expect("a record")
            .internal = Some(internal.join("hands-aaaabbbbccccdddd.ora"));
        let ours = internal.join("hands-aaaabbbbccccdddd.ora");

        // A stroke in progress must hold everything back, however overdue it
        // is. This is the guarantee the whole schedule rests on.
        let mut enc = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        drive(&mut editor, gpu, &mut canvases, &mut enc, false);
        gpu.queue.submit(Some(enc.finish()));
        assert!(
            !editor.autosave.capturing(),
            "an autosave started in the middle of a stroke"
        );

        for _ in 0..2000 {
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            drive(&mut editor, gpu, &mut canvases, &mut enc, true);
            gpu.queue.submit(Some(enc.finish()));
            let notice = collect(&mut editor, gpu, &mut canvases);
            assert!(notice.is_none(), "{:?}", notice.map(|n| n.lines));
            // Wait for the dot as well as for the files. The writing happens on
            // a thread and the dot comes off when its report is *collected*,
            // which is earlier in this same iteration — so stopping the moment
            // both files appear can leave one frame's worth of bookkeeping
            // undone and fail an assertion the application would never fail,
            // since it collects on every frame.
            if theirs.exists() && ours.exists() && !editor.session.active_tab().modified {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        assert!(theirs.exists(), "the document's own file was never written");
        assert!(ours.exists(), "the internal copy was never written");
        assert!(umber_core::docimport::import(&theirs).is_ok());
        assert!(
            !editor.session.active_tab().modified,
            "the tab's dot should come off once its own file has been written"
        );

        let _ = std::fs::remove_dir_all(&documents);
        let _ = std::fs::remove_dir_all(&internal);
    }

    #[test]
    fn a_title_that_is_not_a_file_name_still_becomes_one() {
        assert_eq!(stem_of("hands"), "hands");
        assert_eq!(stem_of("study: hands/feet"), "study- hands-feet");
        assert_eq!(stem_of(""), "untitled");
        assert_eq!(stem_of("///"), "untitled");
        assert_eq!(stem_of(&"x".repeat(200)).len(), 48);
        // Names Windows refuses outright, and leading or trailing spaces.
        assert!(!stem_of("a<b>c:d\"e|f?g*h").contains(['<', '>', ':', '"', '|', '?', '*']));
        assert_eq!(stem_of("  spaced  "), "spaced");
    }

    #[test]
    fn the_name_digest_is_the_same_one_next_week() {
        // Pinned rather than merely deterministic: `DefaultHasher` is
        // deterministic within a build too, and changes between Rust releases,
        // which would rename every internal copy on an upgrade.
        assert_eq!(digest(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(digest(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(
            internal_name("hands", 0x0123_4567_89ab_cdef),
            "hands-0123456789abcdef.ora"
        );
    }

    // --- folders ------------------------------------------------------------

    /// A snapshot of a stack with a folder in it, bottom first:
    ///
    /// ```text
    ///   3  Above          slot 2
    ///   2  Group          folder, no slot
    ///   1    Inside       slot 1, with a mask
    ///   0  Below          slot 0
    /// ```
    fn nested_candidate() -> Candidate {
        let layer = |name: &str, slot: u32, depth: u8, mask: Option<u32>| LayerMeta {
            name: name.to_string(),
            visible: true,
            opacity: 1.0,
            blend: BlendMode::Normal,
            slot: Some(slot),
            depth,
            folder: false,
            mask,
            effects: Vec::new(),
            text: None,
            clipped: false,
            locked: false,
            link: None,
        };
        let mut doc = candidate(Session::default().active_id(), "nested");
        doc.layers = vec![
            layer("Below", 0, 0, None),
            layer("Inside", 1, 1, Some(3)),
            LayerMeta {
                name: "Group".to_string(),
                visible: true,
                opacity: 1.0,
                blend: BlendMode::Normal,
                slot: None,
                depth: 0,
                folder: true,
                mask: None,
                effects: Vec::new(),
                text: None,
                clipped: false,
                locked: false,
                link: None,
            },
            layer("Above", 2, 0, None),
        ];
        doc
    }

    /// **A folder is read back as nothing, so the capture is shorter than the
    /// stack**, and every layer above a folder would be handed the pixels of
    /// the one below it by a positional zip. That is somebody's unattended file
    /// written with its layers shifted, which is why `pixel_index` exists.
    #[test]
    fn a_folder_does_not_shift_a_layer_onto_another_layers_pixels() {
        let doc = nested_candidate();
        assert_eq!(
            doc.slots(),
            vec![0, 1, 2, 3],
            "three layer slices then the one mask, and no folder"
        );

        // Entry position -> where its pixels are in the capture. The folder at
        // 2 has none, and "Above" at 3 must not be handed slot 1's.
        assert_eq!(doc.pixel_index(0), Some(0), "Below");
        assert_eq!(doc.pixel_index(1), Some(1), "Inside");
        assert_eq!(doc.pixel_index(2), None, "the folder holds no pixels");
        assert_eq!(doc.pixel_index(3), Some(2), "Above, not shifted down one");
        assert_eq!(doc.pixel_index(9), None, "off the end");

        // Every layer's index into the capture names its own slice.
        for (entry, meta) in doc.layers.iter().enumerate() {
            let Some(slot) = meta.slot else { continue };
            let k = doc.pixel_index(entry).expect("a layer has pixels");
            assert_eq!(
                doc.slots()[k],
                slot,
                "layer “{}” was pointed at another layer's pixels",
                meta.name
            );
        }
    }

    /// The mask tail begins past every *layer* slice, which is the count of
    /// entries that have one — not the entry count, which folders inflate. Get
    /// that wrong and a masked layer is written with a layer's pixels as its
    /// mask, hiding whatever it covers.
    #[test]
    fn a_folder_does_not_shift_the_mask_tail() {
        let doc = nested_candidate();
        let k = doc.mask_index(1).expect("“Inside” has a mask");
        assert_eq!(doc.slots()[k], 3, "that is the mask's own slice");
        assert_eq!(doc.mask_index(0), None);
        assert_eq!(doc.mask_index(2), None, "a folder has no mask");
        assert_eq!(doc.mask_index(3), None);
    }

    /// The flattened preview the file carries has to match the screen, so the
    /// snapshot's reading of "is this drawn" has to match
    /// `LayerStack::effective_visible`'s. A folder contributes no draw and its
    /// eye reaches its contents.
    #[test]
    fn the_preview_hides_what_a_hidden_folder_hides() {
        let mut doc = nested_candidate();
        assert_eq!(doc.draws().len(), 3, "the folder is not a draw");
        assert!(doc.draws().iter().all(|d| d.visible));

        doc.layers[2].visible = false;
        let draws = doc.draws();
        assert_eq!(
            draws.iter().map(|d| d.visible).collect::<Vec<_>>(),
            vec![true, false, true],
            "only the layer inside the folder goes"
        );

        // And against the model, which is the rule this is a second reading
        // of. The same shape, built through the public API: three layers with
        // the middle one grouped.
        let mut stack = umber_core::LayerStack::new();
        stack.add();
        stack.add();
        stack.set_active(1);
        stack
            .group(&[1])
            .expect("the middle layer alone in a group");
        assert_eq!(
            stack
                .layers()
                .iter()
                .map(|l| (l.depth, l.is_folder()))
                .collect::<Vec<_>>(),
            doc.layers
                .iter()
                .map(|l| (l.depth, l.folder))
                .collect::<Vec<_>>(),
            "the fixture and the model are not the same shape"
        );
        for (i, meta) in doc.layers.iter().enumerate() {
            stack.get_mut(i).unwrap().visible = meta.visible;
        }
        for i in 0..doc.layers.len() {
            assert_eq!(
                doc.effective_visible(i),
                stack.effective_visible(i),
                "the snapshot and the model disagree about entry {i}"
            );
        }
    }

    // --- offering a copy back -----------------------------------------------

    /// The moment every `seconds_ago` in these tests is measured against.
    fn noon() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn marked(
        title: &str,
        path: Option<&str>,
        copy: Option<&str>,
        modified: bool,
    ) -> MarkedDocument {
        MarkedDocument {
            title: title.to_string(),
            path: path.map(str::to_string),
            copy: copy.map(str::to_string),
            modified,
        }
    }

    fn record_of(documents: Vec<MarkedDocument>) -> SessionRecord {
        SessionRecord {
            format: MARK_FORMAT,
            version: "0.0.5".to_string(),
            documents,
        }
    }

    /// The directory a made-up file system keeps its internal copies in.
    /// Every `copy` in these fixtures is a name an autosave writes, inside it —
    /// which is what [`ours`] demands and `a_copy_outside_the_autosave_folder_
    /// is_not_one_of_ours` is about.
    const COPIES: &str = "/data/autosave";

    /// A reading of a made-up file system: a path, and when it was last
    /// written. Anything not in the list does not exist.
    fn readings(entries: &[(&str, SystemTime)]) -> impl Fn(&Path) -> Option<SystemTime> + use<> {
        let owned: Vec<(PathBuf, SystemTime)> = entries
            .iter()
            .map(|(p, t)| (PathBuf::from(p), *t))
            .collect();
        move |path: &Path| owned.iter().find(|(p, _)| p == path).map(|(_, t)| *t)
    }

    /// The document with nowhere else to go. A never-saved painting's copy is
    /// the only record of it that exists, so it is always worth offering.
    #[test]
    fn a_never_saved_documents_copy_is_always_offered() {
        let record = record_of(vec![marked(
            "Untitled 3",
            None,
            Some("/data/autosave/u-1111111111111111.ora"),
            true,
        )]);
        let read = readings(&[(
            "/data/autosave/u-1111111111111111.ora",
            noon() - Duration::from_secs(240),
        )]);
        let (found, at_risk) = offer_from(&record, noon(), Path::new(COPIES), &read);

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].title, "Untitled 3");
        assert_eq!(found[0].original, None, "there is no file to save back to");
        assert_eq!(found[0].seconds_ago, 240);
        assert!(at_risk.is_empty());
    }

    /// The case the whole comparison exists for: the painter's own file is at
    /// least as new as the copy, so the copy holds nothing they do not already
    /// have. Offering it would be asking somebody to choose between two
    /// identical documents.
    #[test]
    fn a_copy_the_painters_own_file_already_holds_is_not_offered() {
        let record = record_of(vec![marked(
            "hands.ora",
            Some("/work/hands.ora"),
            Some("/data/autosave/hands-2222222222222222.ora"),
            false,
        )]);
        // The internal copy is written first and the painter's file a moment
        // later, which is the ordinary case and must not read as "behind".
        let copy_at = noon() - Duration::from_secs(300);
        let read = readings(&[
            ("/data/autosave/hands-2222222222222222.ora", copy_at),
            ("/work/hands.ora", copy_at + Duration::from_millis(400)),
        ]);
        let (found, at_risk) = offer_from(&record, noon(), Path::new(COPIES), &read);
        assert!(found.is_empty(), "{found:?}");
        assert!(at_risk.is_empty());
    }

    /// And its opposite: a copy written after the last Save is the only thing
    /// that holds what came in between.
    #[test]
    fn a_copy_newer_than_the_painters_own_file_is_offered_against_it() {
        let record = record_of(vec![marked(
            "hands.ora",
            Some("/work/hands.ora"),
            Some("/data/autosave/hands-2222222222222222.ora"),
            true,
        )]);
        let read = readings(&[
            (
                "/data/autosave/hands-2222222222222222.ora",
                noon() - Duration::from_secs(60),
            ),
            ("/work/hands.ora", noon() - Duration::from_secs(3600)),
        ]);
        let (found, _) = offer_from(&record, noon(), Path::new(COPIES), &read);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].original.as_deref(),
            Some(Path::new("/work/hands.ora")),
            "a recovered document has to point at the painter's own file, or a \
             Save writes into the autosave folder",
        );
    }

    /// A document that held work and has no copy is **named**, not passed over.
    /// A dialog that offers two documents back and is silent about the third
    /// reads as a promise about the third — the rule `Report::at_risk` keeps.
    #[test]
    fn a_document_with_no_copy_is_named_rather_than_passed_over() {
        let record = record_of(vec![
            marked("Untitled 4", None, None, true),
            // The copy was chosen but the write never landed, which looks
            // exactly the same from here and must be read the same way.
            marked(
                "Untitled 5",
                None,
                Some("/data/autosave/gone-3333333333333333.ora"),
                true,
            ),
            // Nothing to lose, so nothing to say about it at all.
            marked("reference.ora", Some("/work/reference.ora"), None, false),
        ]);
        let read = readings(&[]);
        let (found, at_risk) = offer_from(&record, noon(), Path::new(COPIES), &read);
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(at_risk, ["Untitled 4", "Untitled 5"]);
    }

    /// The marker is the one path in this module Umber did not build itself,
    /// and `Reaper` already says why "the callers only pass internal paths" is
    /// not good enough. A `copy` naming somewhere else is refused — which is
    /// also what stops a marker naming the painter's *own* document from having
    /// it opened, marked modified and offered back as a copy of itself.
    #[test]
    fn a_copy_outside_the_autosave_folder_is_not_one_of_ours() {
        let now = noon();
        let recent = now - Duration::from_secs(60);
        for (case, copy) in [
            ("the painter's own document", "/work/hands.ora"),
            ("a subdirectory of the copies", "/data/autosave/deep/x.ora"),
            ("a name no autosave writes", "/data/autosave/notes.txt"),
            ("a walk back out", "/data/autosave/../../etc/passwd"),
        ] {
            let record = record_of(vec![marked("hands.ora", None, Some(copy), true)]);
            let read = readings(&[(copy, recent)]);
            let (found, at_risk) = offer_from(&record, now, Path::new(COPIES), &read);
            assert!(found.is_empty(), "{case} was offered: {found:?}");
            // Not silently dropped either: the document held work and Umber has
            // nothing it can hand back, which is what `at_risk` is for.
            assert_eq!(at_risk, ["hands.ora"], "{case}");
        }
    }

    /// A clock put back, or a copy restored from a backup. "In the future" is
    /// not a duration, and printing a wrapped one would be worse than the
    /// coarsest possible truth.
    #[test]
    fn a_copy_dated_in_the_future_reads_as_moments_ago() {
        let record = record_of(vec![marked(
            "a",
            None,
            Some("/data/autosave/a-4444444444444444.ora"),
            true,
        )]);
        let read = readings(&[(
            "/data/autosave/a-4444444444444444.ora",
            noon() + Duration::from_secs(3600),
        )]);
        let (found, _) = offer_from(&record, noon(), Path::new(COPIES), &read);
        assert_eq!(found[0].seconds_ago, 0);
        assert!(
            found[0].note().contains("moments ago"),
            "{}",
            found[0].note()
        );
    }

    // --- the marker ---------------------------------------------------------

    /// The rule the whole feature rests on, and the reason [`Marks`] is a
    /// second, much smaller deleter rather than a widened [`Reaper`]: nothing
    /// on the recovery path may reach a document. Recovering a copy is a read,
    /// and declining one leaves it where it is.
    #[test]
    fn a_marks_deleter_refuses_a_document() {
        let root = scratch("marks-root");
        let elsewhere = scratch("marks-elsewhere");

        // An autosave copy sitting in the sessions directory — which cannot
        // happen, and is exactly the thing that must be refused if it does.
        let copy = root.join("hands-0123456789abcdef.ora");
        std::fs::write(&copy, b"an afternoon").expect("write");
        // A foreign `.json`, which is not ours to delete either.
        let foreign = root.join("settings.json");
        std::fs::write(&foreign, b"{}").expect("write");
        // A real marker, outside the root.
        let outside = elsewhere.join("00000000deadbeef.json");
        std::fs::write(&outside, b"{}").expect("write");

        let marks = Marks::new(&root).expect("marks");
        assert_eq!(marks.remove(&copy), Err(Refused::NotASessionMark));
        assert!(
            copy.exists(),
            "a document was deleted by the marker deleter"
        );
        assert_eq!(marks.remove(&foreign), Err(Refused::NotASessionMark));
        assert!(foreign.exists());
        assert_eq!(marks.remove(&outside), Err(Refused::Outside));
        assert!(outside.exists());

        // It does not descend, and a directory is not a candidate.
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).expect("nested");
        let inner = nested.join("00000000deadbeef.json");
        std::fs::write(&inner, b"{}").expect("write");
        assert_eq!(marks.remove(&inner), Err(Refused::Outside));
        assert!(inner.exists());

        let dressed = root.join("00000000cafebabe.json");
        std::fs::create_dir_all(&dressed).expect("directory");
        assert_eq!(marks.remove(&dressed), Err(Refused::NotAPlainFile));
        assert!(dressed.exists());

        // And the listing only ever sees markers, so nothing else can be
        // handed to `remove` in the first place.
        assert!(
            marks.list().iter().all(|p| is_mark_name(p)),
            "{:?}",
            marks.list()
        );

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn only_a_name_this_module_writes_is_a_marker() {
        assert!(is_mark_name(Path::new("0123456789abcdef.json")));
        assert!(is_mark_name(Path::new("/a/b/00000000DEADBEEF.json")));
        assert!(
            !is_mark_name(Path::new("0123456789abcde.json")),
            "15 digits"
        );
        assert!(
            !is_mark_name(Path::new("0123456789abcdefg.json")),
            "not hex"
        );
        assert!(!is_mark_name(Path::new("session.json")));
        assert!(!is_mark_name(Path::new("0123456789abcdef.ora")));
    }

    /// A running Umber's marker is locked, and a second copy of Umber must read
    /// that as "still running" rather than offering its documents back. Two
    /// windows open on one machine is an ordinary thing to do, and recovering
    /// the other one's painting into this one would put two processes on one
    /// file.
    #[test]
    fn a_marker_a_running_umber_holds_is_left_alone() {
        let dir = scratch("marker-live");
        let mut mark = SessionMark::open(&dir, 0x00c0_ffee_0000_0001).expect("marker");
        mark.write(&record_of(vec![marked("hands.ora", None, None, true)]))
            .expect("write");
        assert!(
            claim_if_abandoned(mark.path()).is_none(),
            "a live session's marker was read as abandoned",
        );

        // A second claim on the same name is refused, and — the part that is
        // easy to get wrong — refused *without* having emptied the first one on
        // the way. `open` deliberately does not truncate, because the file has
        // to be opened before it can be locked.
        assert!(
            SessionMark::open(&dir, 0x00c0_ffee_0000_0001).is_err(),
            "two sessions took the same marker",
        );
        assert_eq!(
            std::fs::metadata(mark.path()).map(|m| m.len() > 0).ok(),
            Some(true),
            "a refused claim emptied the record of the session that holds it",
        );

        // The same file once its holder has gone, however it went — the lock is
        // the operating system's and it is released with the handle.
        let path = mark.path().to_path_buf();
        drop(mark);
        assert!(
            claim_if_abandoned(&path).is_some(),
            "a marker whose session has gone was read as live",
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole signal, end to end. A run that ends on purpose takes its
    /// marker down; one that stops leaves it, and what it left is what the next
    /// start offers.
    #[test]
    fn a_session_that_stopped_offers_its_copies_and_one_that_ended_does_not() {
        // The sessions directory is nested *inside* the one the copies are in,
        // as it is in a real installation — which is also what makes a copy in
        // it one of ours. See `ours`.
        let copies = scratch("marker-copies");
        let dir = copies.join(SESSIONS_DIR);
        let copy = copies.join("hands-0123456789abcdef.ora");
        touch(&copy, Duration::from_secs(120));

        // A session that stopped: a marker with a document in it, and nobody
        // holding it.
        let mut stopped = SessionMark::open(&dir, 0x0000_0000_0000_0011).expect("marker");
        stopped
            .write(&record_of(vec![marked(
                "hands.ora",
                None,
                Some(&copy.display().to_string()),
                true,
            )]))
            .expect("write");
        let stopped_path = stopped.path().to_path_buf();
        drop(stopped);

        let (offer, held) = collect_offer(&dir, SystemTime::now());
        assert_eq!(offer.found.len(), 1, "{offer:?}");
        assert_eq!(offer.found[0].title, "hands.ora");
        // Compared by name: `Marks` canonicalises, which on Windows puts a
        // verbatim prefix in front of everything it hands back.
        assert_eq!(
            offer
                .marks
                .iter()
                .map(|p| p.file_name())
                .collect::<Vec<_>>(),
            vec![stopped_path.file_name()],
        );
        assert!(
            stopped_path.exists(),
            "a marker that had something to offer was thrown away before the \
             offer could be answered",
        );
        assert!(copy.exists(), "reading an offer must not touch the copies");

        // And it is **held** while the offer stands, so a second Umber started
        // in the minutes somebody spends reading the dialog cannot read the
        // same marker as abandoned and offer the same painting again.
        assert_eq!(held.len(), 1);
        assert!(
            claim_if_abandoned(&stopped_path).is_none(),
            "the marker behind a live offer was there for the taking",
        );

        // Answered: the marker goes and the copy stays. That is the whole of
        // why "not now" is a safe answer.
        let mut autosave = Autosave {
            marks_dir: Some(dir.clone()),
            claimed: held,
            ..Autosave::default()
        };
        autosave.forget_marks(&offer.marks);
        assert!(!stopped_path.exists());
        assert!(copy.exists(), "declining an offer deleted a document");
        assert!(collect_offer(&dir, SystemTime::now()).0.is_empty());

        let _ = std::fs::remove_dir_all(&copies);
    }

    /// A marker whose copies have all expired, or that never got as far as
    /// writing one, names nothing. Forgotten on the spot rather than left for a
    /// dialog that would have no rows in it — otherwise the directory fills
    /// with markers nothing will ever answer for.
    #[test]
    fn a_marker_with_nothing_to_offer_is_forgotten_rather_than_kept() {
        let dir = scratch("marker-empty");
        let mut empty = SessionMark::open(&dir, 0x0000_0000_0000_0022).expect("marker");
        empty
            .write(&record_of(vec![marked(
                "saved.ora",
                Some("/work/x.ora"),
                None,
                false,
            )]))
            .expect("write");
        let path = empty.path().to_path_buf();
        drop(empty);

        let (offer, held) = collect_offer(&dir, SystemTime::now());
        assert!(offer.is_empty(), "{offer:?}");
        assert!(
            held.is_empty(),
            "a marker that offered nothing was held on to"
        );
        assert!(!path.exists(), "a marker naming nothing was kept");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A marker that cannot be read is **kept**, where one that names nothing
    /// is forgotten. The two look alike from here and are not: a marker whose
    /// rewrite a power cut caught half done may still name copies that are
    /// sitting in the folder, and deleting the only record of them to save a
    /// few hundred bytes is the wrong way round.
    #[test]
    fn a_marker_that_cannot_be_read_is_kept_rather_than_thrown_away() {
        let dir = scratch("marker-unreadable");
        std::fs::create_dir_all(&dir).expect("dir");
        let torn = dir.join("00000000000000aa.json");
        std::fs::write(&torn, br#"{"format":1,"docum"#).expect("write");
        // And one from a build that knows a revision this one does not.
        let newer = dir.join("00000000000000bb.json");
        std::fs::write(&newer, br#"{"format":99,"documents":[]}"#).expect("write");

        let (offer, held) = collect_offer(&dir, SystemTime::now());
        assert!(offer.is_empty(), "{offer:?}");
        assert!(held.is_empty());
        assert!(torn.exists(), "a half-written marker was thrown away");
        assert!(newer.exists(), "a newer build's marker was thrown away");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session that lost work it had no copy of has **nothing to offer**, so
    /// no dialog is raised — and that is the case, not the exception. An
    /// operating system restart force-kills applications and Umber refuses to
    /// close while a document holds unsaved work, so an ordinary reboot leaves
    /// a marker every time. A modal saying "your work is gone" with nothing to
    /// click, after every reboot, is not honesty.
    #[test]
    fn nothing_is_offered_for_work_there_is_no_copy_of() {
        let offer = Offer {
            marks: vec![PathBuf::from(
                "/data/autosave/sessions/00000000deadbeef.json",
            )],
            found: Vec::new(),
            at_risk: vec!["Untitled 4".to_string()],
        };
        assert!(offer.is_empty());
        // Beside something that *can* be offered it is drawn, which is the rule
        // `Report::at_risk` keeps: a list of two must not read as a promise
        // about the third.
        let offer = Offer {
            found: vec![Recoverable {
                title: "hands.ora".into(),
                original: None,
                copy: PathBuf::from("/data/autosave/hands-0123456789abcdef.ora"),
                seconds_ago: 60,
            }],
            ..offer
        };
        assert!(!offer.is_empty());
        assert_eq!(offer.at_risk, ["Untitled 4"]);
    }

    /// `begin_run` scans before it writes, so a fresh start never offers its
    /// own marker back to itself — and `end_run` takes it down again.
    #[test]
    fn a_run_does_not_offer_itself_and_cleans_up_after_itself() {
        let dir = scratch("marker-self");
        let mut autosave = Autosave {
            marks_dir: Some(dir.clone()),
            ..Autosave::default()
        };
        assert!(autosave.begin_run(SystemTime::now()).is_empty());
        assert!(
            autosave.mark.is_some(),
            "the run did not mark itself as running",
        );
        assert_eq!(Marks::new(&dir).expect("marks").list().len(), 1);

        // Once per run: a second call must not mint a second marker.
        assert!(autosave.begin_run(SystemTime::now()).is_empty());
        assert_eq!(Marks::new(&dir).expect("marks").list().len(), 1);

        autosave.end_run();
        assert!(Marks::new(&dir).expect("marks").list().is_empty());
        // And again, which is what happens if the loop is left twice.
        autosave.end_run();

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The marker has to follow the session, or a document closed or saved
    /// after its copy was written would be offered back — work somebody
    /// deliberately let go of.
    #[test]
    fn the_marker_follows_the_documents_and_costs_nothing_while_they_stand_still() {
        let dir = scratch("marker-follow");
        let mut autosave = Autosave {
            marks_dir: Some(dir.clone()),
            ..Autosave::default()
        };
        autosave.begin_run(SystemTime::now());
        let mut session = session_of(2);

        autosave.note_documents(&session);
        let path = autosave
            .mark
            .as_ref()
            .expect("a marker")
            .path()
            .to_path_buf();
        let titles = |record: &SessionRecord| -> Vec<String> {
            record.documents.iter().map(|d| d.title.clone()).collect()
        };
        assert_eq!(
            titles(&autosave.record(&session)),
            ["Untitled 1", "Untitled 2"],
        );

        // Nothing has moved, so nothing is written.
        let noted = autosave.noted;
        autosave.note_documents(&session);
        assert_eq!(
            autosave.noted, noted,
            "the marker was rewritten for nothing"
        );

        // A document closed drops out of the record, so the next start does not
        // offer back a painting somebody put down on purpose.
        session.remove(0);
        autosave.note_documents(&session);
        assert_ne!(autosave.noted, noted);

        // And what is on disk is what the record said. Read only once the
        // marker's handle has gone, because the lock covers the file's bytes:
        // a running session's marker cannot be read by anything else, which is
        // the same fact `is_abandoned` rests on.
        drop(autosave.mark.take());
        assert_eq!(
            record_at(&path).as_ref().map(titles),
            Some(vec!["Untitled 2".to_string()]),
        );

        autosave.end_run();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The record has to survive the file, and a record from a build that knew
    /// fewer fields has to load rather than throwing away the one thing that
    /// says where somebody's work is.
    #[test]
    fn a_record_survives_being_written_and_read_back() {
        let dir = scratch("marker-round-trip");
        let record = record_of(vec![marked(
            "hands.ora",
            Some("/work/hands.ora"),
            Some("/data/autosave/hands-2222222222222222.ora"),
            true,
        )]);
        let mut mark = SessionMark::open(&dir, 0x0000_0000_0000_0033).expect("marker");
        mark.write(&record).expect("write");
        // Rewritten shorter: the file must not keep the tail of what it said
        // before, which is what `set_len` is for.
        mark.write(&record_of(Vec::new())).expect("write");
        mark.write(&record).expect("write");
        let path = mark.path().to_path_buf();
        drop(mark);

        assert_eq!(record_at(&path), Some(record));

        std::fs::write(&path, r#"{"format":1,"documents":[{"title":"a"}]}"#).expect("write");
        let older = record_at(&path).expect("an older record loads");
        assert_eq!(older.documents.len(), 1);
        assert!(older.documents[0].copy.is_none());
        assert!(older.version.is_empty());

        std::fs::write(&path, b"not json at all").expect("write");
        assert_eq!(record_at(&path), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_temporary_a_dead_write_leaves_is_recognised_as_ours() {
        assert!(is_autosave_name(Path::new("hands-0.ora")));
        assert!(is_autosave_name(Path::new("hands-0.ORA")));
        assert!(is_autosave_name(Path::new("hands-0.ora.saving")));
        assert!(!is_autosave_name(Path::new("hands.png")));
        assert!(!is_autosave_name(Path::new("hands")));
        assert!(!is_autosave_name(Path::new("orafile")));
    }
}
