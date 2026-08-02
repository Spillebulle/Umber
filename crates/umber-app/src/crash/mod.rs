//! Crash reporting: the panic hook, and the box somebody actually sees.
//!
//! # How the box is drawn, and why not the other two ways
//!
//! **A separate process.** The hook gathers what it can, writes a
//! [`Report`] to a file, and spawns *this same executable* with
//! `--crash-report <path>`. That second process opens a fresh window on a fresh
//! device and draws the dialog with `theme`, `widgets`, `icons` and
//! `tabs::dialog_frame` — the same objects the settings dialog and the module
//! library are made of, because it is the same application, merely started
//! again for one purpose.
//!
//! The two alternatives both lose, and it is worth writing down why:
//!
//! * **Drawing it in-process** — catch the panic, keep the event loop alive,
//!   put a modal over the canvas. It costs nothing to start and it matches the
//!   rest of the interface for free. It is also unreliable at exactly the
//!   moment it is needed. The crash this was built for is a wgpu validation
//!   error inside `Queue::submit`, and the panic that followed it *while
//!   unwinding* was `wgpu-hal` refusing to destroy a swapchain semaphore still
//!   held by a surface texture. By the time a hook runs, the device may be
//!   poisoned, the surface may be unconfigurable, and egui's own textures may
//!   be among the objects that have been destroyed — so drawing the crash box
//!   with the device that just died is asking the failing subsystem to report
//!   its own failure. A crash handler that panics inside the panic handler is
//!   worse than none, because it replaces a legible stderr message with a
//!   double fault.
//! * **A plain OS message box** — `MessageBoxW` and its two counterparts.
//!   Always works, which is a real argument. But it looks nothing like Umber,
//!   it has no expandable section, it cannot scroll a backtrace, it cannot
//!   offer "restart", and the three platforms would be three implementations
//!   with three different capabilities. It is the right fallback and the wrong
//!   primary, so what is here falls back to **stderr** instead: if the report
//!   cannot be written, or the child cannot be spawned, or the child cannot get
//!   a window, the process still dies with the message it prints today.
//!
//! Nothing here makes a crash quieter. The previous hook — the standard
//! library's, which prints the message and the backtrace — is called **first**
//! and unconditionally, so `RUST_LOG`, stderr and every existing habit are
//! untouched. This is an addition to what a crash does, not a replacement for
//! it.
//!
//! # The rules the hook lives by
//!
//! * **It must not panic.** Every step is fallible and every failure is a
//!   logged line and a return. There is no `unwrap`, no indexing and no slicing
//!   on the path from `set_hook` to `spawn`.
//! * **It reports the main thread only.** A panicking worker — the autosave's
//!   encoder — ends that thread and leaves the application running, and a box
//!   saying "Umber has stopped responding" over a working canvas is a lie. The
//!   previous hook still prints it, so the failure is not hidden.
//! * **It runs once.** `REPORTING` latches, so the second panic during
//!   unwinding cannot spawn a second reporter or recurse into this code. That
//!   second panic still reaches stderr through the chained hook.
//! * **It reads a snapshot, never live state.** The hook cannot borrow
//!   `Editor` — it has no reference to one, and the one that exists may be
//!   halfway through a mutation. So the frame loop keeps [`CONTEXT`] up to date
//!   and the hook takes what is in it, with `try_lock`: a lock held by the very
//!   frame that panicked would otherwise deadlock the hook on its own thread.
//!
//! # `panic = "abort"`
//!
//! The workspace does not set it, and if it ever did none of this changes: the
//! hook runs *before* the abort, exactly as it runs before unwinding, and
//! nothing here depends on the stack being unwound. There is deliberately no
//! `catch_unwind` around `run_app` either. Catching would happen *after* every
//! destructor that produced the second panic had already run, `run_app` is not
//! `UnwindSafe`, and on Windows the loop unwinds through a Win32 message
//! callback where catching is not dependable. The hook is the only moment the
//! process is still in a state worth describing.
//!
//! # What is not deleted
//!
//! Reports accumulate, and that is deliberate. Each is a few kilobytes of JSON,
//! and it is the only record of what happened — the window shows it once and
//! somebody may come back to it days later. [`crate::autosave::Reaper`] is the
//! only thing in Umber that deletes a file on the user's behalf, and its
//! containment is careful for good reason; a second deleter, for kilobytes, is
//! the wrong trade.

mod report;
mod window;

pub use report::Report;
use report::{AutosaveCopy, BACKTRACE_LIMIT, DocumentNote, FIELD_LIMIT, FORMAT, tidy};

use crate::session::{DocId, Session};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// The flag that turns this executable into the reporter.
pub const FLAG: &str = "--crash-report";

/// Directory under the platform data directory that holds the reports.
///
/// The **data** directory, beside the autosave's own, for the same reason: it
/// is a record of work rather than a setting.
const DIR_NAME: &str = "crash";

// ---------------------------------------------------------------------------
// Which program this process is
// ---------------------------------------------------------------------------

/// What this process was started to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Launch {
    /// Umber, as usual.
    Normal,
    /// The crash reporter, over the report at this path.
    Report(PathBuf),
}

/// Read the command line.
///
/// A pure function of the arguments, so the whole of it is tested without
/// starting anything — the same shape [`crate::update::install::detect`] keeps.
///
/// An argument this does not recognise is **logged and ignored**, not refused.
/// Umber is a painting application launched by file managers, desktop entries
/// and `cargo run --`, any of which can pass something unexpected; refusing to
/// start over a stray word would be a far worse failure than starting normally.
pub fn parse_args<I: IntoIterator<Item = String>>(args: I) -> Launch {
    let mut args = args.into_iter().skip(1);
    while let Some(arg) = args.next() {
        if arg == FLAG {
            return match args.next() {
                Some(path) => Launch::Report(PathBuf::from(path)),
                // The flag with nothing after it. There is no report to show,
                // so the only sensible thing left is to be Umber.
                None => {
                    log::warn!("{FLAG} was given no path; starting normally");
                    Launch::Normal
                }
            };
        }
        log::warn!("ignoring unrecognised argument “{arg}”");
    }
    Launch::Normal
}

// ---------------------------------------------------------------------------
// What the hook is allowed to know
// ---------------------------------------------------------------------------

/// Everything the frame loop has told the hook it may say.
///
/// A snapshot rather than a reference to anything live: see the module docs.
struct Context {
    adapter: Option<String>,
    backend: Option<String>,
    docs: Vec<Doc>,
    copies: Vec<Copy>,
}

struct Doc {
    id: DocId,
    title: String,
    modified: bool,
    revision: u64,
    path: Option<String>,
}

/// The last autosave of one document. Held apart from [`Doc`] because the two
/// are refreshed by different events — the tab strip changes far more often
/// than a copy is written — and because the *age* has to be computed at the
/// moment of the crash rather than at the moment of the write.
struct Copy {
    id: DocId,
    path: String,
    revision: u64,
    written: Instant,
}

impl Context {
    const fn empty() -> Self {
        Self {
            adapter: None,
            backend: None,
            docs: Vec::new(),
            copies: Vec::new(),
        }
    }

    fn documents(&self, now: Instant) -> Vec<DocumentNote> {
        self.docs
            .iter()
            .map(|d| DocumentNote {
                title: d.title.clone(),
                modified: d.modified,
                revision: d.revision,
                path: d.path.clone(),
                autosave: self
                    .copies
                    .iter()
                    .find(|c| c.id == d.id)
                    .map(|c| AutosaveCopy {
                        path: c.path.clone(),
                        revision: c.revision,
                        seconds_ago: now.saturating_duration_since(c.written).as_secs(),
                    }),
            })
            .collect()
    }
}

static CONTEXT: Mutex<Context> = Mutex::new(Context::empty());

/// A cheap reading of the tab strip, so the per-frame call rebuilds nothing
/// while nothing has changed. The drawing path allocates nothing.
static DOCUMENTS_MARK: AtomicU64 = AtomicU64::new(u64::MAX);

/// Set once the hook has begun a report, so a second panic — the one that
/// arrives while the first is unwinding — cannot start another.
static REPORTING: AtomicBool = AtomicBool::new(false);

/// Record the GPU, as soon as there is one.
pub fn note_adapter(info: &wgpu::AdapterInfo) {
    let mut ctx = lock();
    ctx.adapter = Some(tidy(&info.name, FIELD_LIMIT));
    ctx.backend = Some(format!("{:?}", info.backend));
}

/// Record what is open, from the frame loop.
///
/// Called every frame and does nothing on almost all of them: the tab strip is
/// reduced to one number first, and only a change to it rebuilds the list. That
/// is what lets this sit on the drawing path at all.
pub fn note_documents(session: &Session) {
    let mut mark = session.tabs().len() as u64;
    for tab in session.tabs() {
        mark = mark.rotate_left(7).wrapping_add(tab.revision)
            ^ u64::from(tab.modified)
            ^ (u64::from(tab.path.is_some()) << 1);
    }
    if DOCUMENTS_MARK.load(Ordering::Relaxed) == mark {
        return;
    }

    let mut ctx = lock();
    ctx.docs = session
        .tabs()
        .iter()
        .map(|tab| Doc {
            id: tab.id,
            title: tidy(&tab.title, FIELD_LIMIT),
            modified: tab.modified,
            revision: tab.revision,
            path: tab.path.as_ref().map(|p| p.display().to_string()),
        })
        .collect();
    // Only once the rebuild has actually happened, so a run that was skipped
    // is retried on the next frame rather than remembered as done.
    DOCUMENTS_MARK.store(mark, Ordering::Relaxed);
}

/// Record where a document was just written, and what it was written from.
///
/// `revision` is the document's revision when the *capture* began — the same
/// number [`crate::session::Session::mark_autosaved`] compares — which is what
/// lets the box say whether the copy holds everything or is a stroke behind.
pub fn note_autosave(id: DocId, path: PathBuf, revision: u64) {
    let entry = Copy {
        id,
        path: path.display().to_string(),
        revision,
        written: Instant::now(),
    };
    let mut ctx = lock();
    match ctx.copies.iter_mut().find(|c| c.id == id) {
        Some(existing) => *existing = entry,
        None => ctx.copies.push(entry),
    }
}

/// The context lock, taking a poisoned one rather than giving up on it.
///
/// A poisoned mutex here means a thread panicked while holding it, which is
/// precisely the situation this module exists for. The data behind it is a few
/// owned strings with no invariant between them, so reading it after a panic is
/// safe in the sense that matters: the worst case is a slightly stale document
/// list in a report that would otherwise have none.
fn lock() -> std::sync::MutexGuard<'static, Context> {
    CONTEXT.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// The hook
// ---------------------------------------------------------------------------

/// Install the panic hook. Call once, as early in `main` as possible.
pub fn install_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // First, and unconditionally. Everything below is an addition to what a
        // crash already does; none of it may take the stderr message away.
        previous(info);
        report_panic(info);
    }));
}

/// Gather, write and hand over. Every failure inside is logged and swallowed.
fn report_panic(info: &std::panic::PanicHookInfo<'_>) {
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("<unnamed>");
    if name != "main" {
        // A worker died. The application is still running, so a box saying it
        // has stopped would be false. The chained hook has already said so on
        // stderr, which is the honest amount of noise for this.
        log::error!("the {name} thread panicked; Umber is still running");
        return;
    }
    if REPORTING.swap(true, Ordering::SeqCst) {
        // The second panic, arriving while the first unwinds. Nothing to do:
        // the report is already written and the reporter already spawned.
        return;
    }

    let report = gather(info, name);
    let Some(path) = report_path() else {
        log::error!("no data directory: the crash report was not written");
        return;
    };
    if let Err(e) = report.write(&path) {
        log::error!(
            "could not write the crash report to {}: {e}",
            path.display()
        );
        return;
    }
    log::error!("crash report written to {}", path.display());
    spawn_reporter(&path);
}

/// Build the report out of the panic and whatever the frame loop last said.
fn gather(info: &std::panic::PanicHookInfo<'_>, thread: &str) -> Report {
    // Forced rather than left to `RUST_BACKTRACE`: the whole point of a crash
    // report is that it is complete without anybody having set anything up
    // beforehand. `force_capture` can still come back empty where the platform
    // has no unwind tables, which is why the report carries a flag for it
    // rather than an empty string that would read as an empty stack.
    let backtrace = std::backtrace::Backtrace::force_capture();
    let captured = backtrace.status() == std::backtrace::BacktraceStatus::Captured;

    let now = Instant::now();
    // `try_lock`, and this is the one place it matters: the panicking thread
    // may be the thread that holds this lock, in which case `lock()` would
    // deadlock the hook against itself. A report with no document list is far
    // better than a process that hangs instead of dying.
    let documents = match CONTEXT.try_lock() {
        Ok(ctx) => ctx.documents(now),
        Err(std::sync::TryLockError::Poisoned(e)) => e.into_inner().documents(now),
        Err(std::sync::TryLockError::WouldBlock) => {
            log::warn!("the crash context was busy; the report names no documents");
            Vec::new()
        }
    };
    let (adapter, backend) = match CONTEXT.try_lock() {
        Ok(ctx) => (ctx.adapter.clone(), ctx.backend.clone()),
        Err(std::sync::TryLockError::Poisoned(e)) => {
            let ctx = e.into_inner();
            (ctx.adapter.clone(), ctx.backend.clone())
        }
        Err(std::sync::TryLockError::WouldBlock) => (None, None),
    };

    Report {
        format: FORMAT,
        version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        adapter,
        backend,
        thread: thread.to_string(),
        message: report::payload_message(info.payload()),
        location: info.location().map(|l| format!("{l}")),
        backtrace: tidy(&backtrace.to_string(), BACKTRACE_LIMIT),
        backtrace_available: captured,
        documents,
    }
}

/// Where this crash's report goes.
///
/// Named after the moment rather than overwriting one file: two crashes in a
/// session are two things somebody may want to look at, and the second is
/// usually the interesting one.
fn report_path() -> Option<PathBuf> {
    let dir = directories::ProjectDirs::from("", "", "Umber")?
        .data_dir()
        .join(DIR_NAME);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        // A clock before the epoch is not a reason to lose the report.
        .unwrap_or(0);
    Some(dir.join(format!("crash-{stamp}.json")))
}

/// Hand the report to a fresh copy of this executable.
///
/// stdio is inherited on purpose: the child's own `RUST_LOG` output belongs in
/// the same terminal as the message that has just been printed there.
fn spawn_reporter(path: &Path) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            log::error!("could not find this executable, so no crash box: {e}");
            return;
        }
    };
    match std::process::Command::new(&exe).arg(FLAG).arg(path).spawn() {
        Ok(_) => {}
        Err(e) => log::error!("could not start the crash reporter: {e}"),
    }
}

// ---------------------------------------------------------------------------
// wgpu's own fatal errors
// ---------------------------------------------------------------------------

/// What a device says when it has an error nobody asked it to hand back.
///
/// wgpu's default for this logs one line — "Handling wgpu errors as fatal by
/// default" — and panics from inside `wgpu_core`, so the location in the report
/// is a line of somebody else's crate and the message is whatever the backend
/// wrote. Routing it here loses none of that and adds Umber's own voice to it:
/// the error is logged in full, and then raised as a panic whose message says
/// what happened, so it travels down exactly the path above.
///
/// Deliberately still fatal. A device that has reported an uncaptured error is
/// producing undefined results from then on, and carrying on would mean a
/// canvas that is quietly wrong — which is the failure mode this codebase
/// refuses everywhere else.
pub fn device_error(error: wgpu::Error) {
    log::error!("the graphics device reported an uncaptured error: {error}");
    panic!("The graphics device reported an error Umber cannot carry on from.\n\n{error}");
}

// ---------------------------------------------------------------------------
// The reporter process
// ---------------------------------------------------------------------------

/// Show the report at `path`. This is the whole of what `--crash-report` does.
///
/// Falls back to stderr at both places it can fail — an unreadable report and a
/// window that will not open — because the reason this process exists is that
/// somebody's application stopped, and the least it can do is say why in the
/// terminal it may have been started from.
pub fn show_report(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let report = match Report::read(path) {
        Ok(report) => report,
        Err(e) => {
            eprintln!(
                "Umber could not read the crash report at {}: {e}",
                path.display()
            );
            return Err(e.into());
        }
    };
    if let Err(e) = window::show(&report, path) {
        eprintln!("Umber could not open the crash report window: {e}");
        eprintln!("\n{}", report.details());
        return Err(e);
    }
    Ok(())
}

/// Start Umber again, with nothing on its command line.
///
/// Used by the crash box's Restart button. Spawned rather than `exec`'d even on
/// Unix: the reporter has a window on screen, and replacing its process image
/// would make that window vanish mid-click with no explanation.
pub fn restart() {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(&exe).spawn() {
                log::error!("could not restart Umber: {e}");
            }
        }
        Err(e) => log::error!("could not find this executable to restart it: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn a_plain_launch_is_umber() {
        assert_eq!(parse_args(args(&["umber"])), Launch::Normal);
        assert_eq!(parse_args(args(&[])), Launch::Normal);
    }

    #[test]
    fn the_flag_and_a_path_is_the_reporter() {
        assert_eq!(
            parse_args(args(&["umber", FLAG, "C:\\reports\\crash-1.json"])),
            Launch::Report(PathBuf::from("C:\\reports\\crash-1.json")),
        );
    }

    /// The flag with nothing after it names no report, so there is nothing to
    /// show. Starting normally is the only thing left that is any use.
    #[test]
    fn the_flag_with_no_path_starts_umber() {
        assert_eq!(parse_args(args(&["umber", FLAG])), Launch::Normal);
    }

    /// A file manager, a desktop entry or `cargo run --` can pass anything.
    /// Refusing to start a painting application over a stray word would be a
    /// far worse failure than ignoring it.
    #[test]
    fn an_unrecognised_argument_does_not_stop_umber_starting() {
        assert_eq!(parse_args(args(&["umber", "--verbose"])), Launch::Normal);
        assert_eq!(
            parse_args(args(&["umber", "--verbose", FLAG, "r.json"])),
            Launch::Report(PathBuf::from("r.json")),
        );
    }

    /// A path is taken verbatim, whatever it looks like. It came from this
    /// process's own hook, and second-guessing it would only break the spellings
    /// Windows and Unix disagree about.
    #[test]
    fn a_path_that_looks_like_a_flag_is_still_a_path() {
        assert_eq!(
            parse_args(args(&["umber", FLAG, "--odd.json"])),
            Launch::Report(PathBuf::from("--odd.json")),
        );
    }

    /// The context is what the hook has instead of the editor, so what goes in
    /// has to come out — including an age measured at the crash rather than at
    /// the write.
    #[test]
    fn the_context_merges_a_copy_onto_the_document_it_belongs_to() {
        let id = DocId::for_test(7);
        let other = DocId::for_test(8);
        let written = Instant::now();
        let ctx = Context {
            adapter: None,
            backend: None,
            docs: vec![
                Doc {
                    id,
                    title: "Study.ora".into(),
                    modified: true,
                    revision: 12,
                    path: None,
                },
                Doc {
                    id: other,
                    title: "Untitled 2".into(),
                    modified: true,
                    revision: 3,
                    path: None,
                },
            ],
            copies: vec![Copy {
                id,
                path: "/tmp/study.ora".into(),
                revision: 11,
                written,
            }],
        };

        let notes = ctx.documents(written + std::time::Duration::from_secs(90));
        assert_eq!(notes.len(), 2);
        let copy = notes[0].autosave.as_ref().expect("the copy is carried");
        assert_eq!(copy.path, "/tmp/study.ora");
        assert_eq!(copy.revision, 11);
        assert_eq!(copy.seconds_ago, 90);
        // The document with no copy must not inherit its neighbour's.
        assert!(notes[1].autosave.is_none());

        let report = Report {
            documents: notes,
            ..Report::default()
        };
        // 11 against 12: the copy is a stroke behind, and the box has to say so.
        assert!(!report.rescued()[0].complete);
        assert_eq!(report.at_risk(), ["Untitled 2"]);
    }
}
