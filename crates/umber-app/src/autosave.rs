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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use glam::UVec2;
use umber_core::docformat::{self, SaveDocument, SaveLayer};
use umber_core::{Background, BlendMode};
use umber_render::{CanvasRenderer, DocumentCapture, Gpu};

use crate::editor::Editor;
use crate::session::{DocId, Session};
use crate::tabs::Notice;

/// Directory under the platform data directory that holds the internal copies.
pub const DIR_NAME: &str = "autosave";

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

/// The path as the settings dialog shows it, or a plain explanation when there
/// is none. Never a silent blank.
pub fn internal_dir_label() -> String {
    match internal_dir() {
        Some(path) => path.display().to_string(),
        None => "unavailable — this system has no data directory".to_string(),
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

/// Why the reaper would not delete something.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// It does not resolve to a file directly inside the reaper's root.
    Outside,
    /// Not a plain file — a directory, or a symbolic link.
    NotAPlainFile,
    /// Not a name an autosave writes.
    NotAnAutosave,
    /// The file system said no.
    Io(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Outside => write!(f, "it is not inside the autosave folder"),
            Self::NotAPlainFile => write!(f, "it is not a plain file"),
            Self::NotAnAutosave => write!(f, "it is not an autosave"),
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
    pub size: UVec2,
    pub background: Background,
    pub dpi: f32,
    pub active_layer: usize,
    /// Bottom to top, matching `LayerStack`'s own order.
    pub layers: Vec<LayerMeta>,
}

impl Candidate {
    /// The stack as the composite pass takes it, for the flattened preview.
    pub fn draws(&self) -> Vec<umber_render::LayerDraw> {
        self.layers
            .iter()
            .enumerate()
            // Exactly what `Editor::layer_draws` does, and for the same reason:
            // a pass-through folder is its contents composited in place, so it
            // contributes nothing but its eye. The flattened preview this
            // builds has to match the screen, so the two rules have to be the
            // same rule.
            .filter_map(|(i, l)| {
                Some(umber_render::LayerDraw {
                    slot: l.slot?,
                    opacity: l.opacity,
                    blend: l.blend.index(),
                    visible: self.effective_visible(i),
                    mask: l.mask,
                    clipped: l.clipped,
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
        let (slots, draws) = (doc.slots(), doc.draws());
        // A document with no renderer has no pixels to read — the resume path
        // rebuilds them, and until then there is nothing worth writing.
        if canvases
            .get_mut(&id)
            .is_some_and(|canvas| canvas.begin_capture(&slots, &draws))
        {
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
                        "Autosave will keep trying. Your work is not lost — use \
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
        size: doc.size,
        background: doc.background,
        dpi: doc.dpi,
        active_layer: layers.active_index(),
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
    if let Some(path) = &doc.path {
        match docformat::write_encoded(path, &encoded) {
            Ok(()) => {
                wrote_user_file = true;
                log::debug!("autosaved {}", path.display());
            }
            Err(e) => reports.push(Report::Failed {
                title: doc.title.clone(),
                message: format!("{} — {e}", path.display()),
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
        return Err(format!("{} could not be created — {e}", dir.display()));
    }
    docformat::write_encoded(path, bytes).map_err(|e| format!("{} — {e}", path.display()))?;
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
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("umber-autosave-{name}"));
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

    fn candidate(id: DocId, title: &str) -> Candidate {
        Candidate {
            id,
            revision: 0,
            title: title.to_string(),
            path: None,
            size: UVec2::splat(8),
            background: Background::Transparent,
            dpi: 72.0,
            active_layer: 0,
            layers: vec![LayerMeta {
                name: "Layer 1".to_string(),
                visible: true,
                opacity: 1.0,
                blend: BlendMode::Normal,
                slot: Some(0),
                depth: 0,
                folder: false,
                mask: None,
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
        let instance = Gpu::create_instance();
        let Ok(gpu) = pollster::block_on(Gpu::new(instance, None)) else {
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
        drive(&mut editor, &gpu, &mut canvases, &mut enc, false);
        gpu.queue.submit(Some(enc.finish()));
        assert!(
            !editor.autosave.capturing(),
            "an autosave started in the middle of a stroke"
        );

        for _ in 0..2000 {
            let mut enc = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            drive(&mut editor, &gpu, &mut canvases, &mut enc, true);
            gpu.queue.submit(Some(enc.finish()));
            let notice = collect(&mut editor, &gpu, &mut canvases);
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
