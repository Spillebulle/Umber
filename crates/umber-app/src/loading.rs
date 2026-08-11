//! Decoding a document off the drawing thread, with something honest on screen.
//!
//! Opening a large document froze the whole application. Measured with
//! `examples/measure-open.rs` on a real 15000×5000 Clip Studio file: **13.4
//! seconds**, of which reading the file off disk is 55 ms, building the layer
//! stack afterwards is nothing, and **99.6% is decoding one layer's blocks
//! after another**. So the wait is one CPU-bound phase over owned data, which
//! is the best possible shape — it can move to a thread, and because the
//! readers loop over layers it can report where it has got to.
//!
//! The division follows the one this crate already keeps for the update check
//! and the autosave: the work happens on a worker, and an [`EventLoopProxy`]
//! wakes the loop when there is something to draw. Under `ControlFlow::Wait` a
//! value arriving in a channel is not an event, so without the wake the bar
//! would sit still until the pointer moved.
//!
//! **What is on screen may not claim more than is known.** `total` is the layer
//! count the reader declared and `done` is how many it has finished; before the
//! reader has counted them, [`Loading::fraction`] answers `None` and the bar
//! draws an empty track rather than inventing a position. That is
//! `update::Stage::progress`'s rule, and it is the same reason the splash shows
//! each stage before the work it names rather than after.
//!
//! **The GPU half stays on the main thread**, and there is no choice about it:
//! `Opened` carries the uploads and only the drawing thread has the device.
//! That is free — it is the 0 ms half.
//!
//! **A worker that ends without answering has to be told apart from one that
//! has not answered yet**, and for a while it was not. [`Loading::take`] read
//! the channel with `try_recv().ok()`, which maps `Disconnected` — every sender
//! dropped, which for this channel can only be a panic in the reader — onto the
//! same `None` that means "still going". `editor.loading` is cleared from
//! exactly one place and only on a `Some`, and `tabs::loading` draws a modal
//! with no Cancel for as long as it is set, so a panic in any of the five
//! document readers left an **uncancellable dialog over every open tab for the
//! life of the process**, with no further wake ever arriving. The one signal
//! was a `log::error!` line nobody sees, because `crash::report_panic`
//! deliberately reports nothing for a thread that is not `main`.
//!
//! The fix is that the *waiter* notices, which is what
//! [`update::Updates::poll`](crate::update) and `textpanel::Fonts::poll` both
//! already do. It is deliberately **not** a Cancel on the dialog: that argument
//! is `tabs::loading`'s and is untouched by this, since a Cancel that cannot
//! interrupt the worker is a control that lies.
//!
//! **Noticing is only half of it: something has to be looking.** The loop runs
//! at `ControlFlow::Wait`, so a reading nobody asks for is a reading nobody
//! takes — and the worker's own wakes are the progress report and the one after
//! the answer is sent, neither of which a panic reaches. So the wake is a
//! destructor, [`WakeAtTheEnd`], which unwinding runs. Without it the fix was
//! real and arrived on the artist's next mouse move.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::TryRecvError;

use umber_core::docimport::{ImportError, ImportedDocument};
use winit::event_loop::EventLoopProxy;

use crate::app::Wake;
use crate::editor::Editor;
use crate::tabs::Notice;

/// A decode in flight.
pub struct Loading {
    /// What the dialog calls it.
    pub name: String,
    /// The file being read, kept because the caller needs it again once the
    /// document arrives — a tab points at the artist's own path.
    pub path: PathBuf,
    /// Whether the opened document should count as modified. A recovered
    /// autosave copy does; an ordinary open does not.
    pub modified: bool,
    /// Where the tab should point, which is not always [`Self::path`]: a
    /// recovery reads Umber's own copy and belongs to the painter's file.
    pub record_path: Option<PathBuf>,
    /// Layers finished, and how many there are. Packed into one word so the
    /// worker can publish both at once and the drawing thread cannot read a
    /// `done` from after the `total` it is divided by.
    progress: Arc<AtomicU32>,
    /// The finished document, once the worker has one.
    outcome: std::sync::mpsc::Receiver<Result<ImportedDocument, ImportError>>,
}

/// The worker's end of the channel, and the wake that must follow it.
///
/// **The wake has to be a destructor, because a panic is not a return.** Under
/// `ControlFlow::Wait` a value appearing in a channel is not an event, so the
/// loop only looks at [`Loading::take`] on a frame something else asked for.
/// The worker's own wakes are the per-layer progress report and the one after
/// the answer is sent, and a panic reaches neither; dropping an
/// [`EventLoopProxy`] wakes nothing on its own. So a reader that died left the
/// modal on screen, correct in every respect except that nothing would look at
/// it, until an unrelated window event happened to produce a frame. Unwinding
/// runs destructors, so this fires on both paths and is the only statement of
/// the wake.
///
/// **It owns the sender, and that is the whole reason this is a struct rather
/// than a guard beside one.** What the woken frame has to see on the panic path
/// is the channel *disconnected*, which needs the sender already dropped — and
/// the first draft got that backwards while asserting the opposite in a
/// comment. A body local declared first does **not** drop last: a captured
/// upvar is a field of the closure's environment, and the environment is
/// dropped after every local in the body, so the guard went first and the wake
/// raced the disconnect. Measured rather than reasoned about, on both the
/// ordinary and the unwinding path. Holding the sender here and dropping it
/// *inside* `drop` makes the order a property of this type instead of of where
/// a `let` happens to sit.
///
/// The wake is a closure rather than an [`EventLoopProxy`] for the reason
/// `Autosave::waker` is one: it is what lets a test watch this fire without a
/// window, which a destructor otherwise has nothing checking at all.
struct Finish {
    /// `Option` only so `drop` can let it go before the wake.
    answer: Option<std::sync::mpsc::Sender<Result<ImportedDocument, ImportError>>>,
    wake: Box<dyn Fn() + Send>,
}

impl Finish {
    /// Hand the answer over. Failing means the application dropped the handle,
    /// which is a document nobody is waiting for any more.
    fn answer(&self, result: Result<ImportedDocument, ImportError>) {
        if let Some(answer) = self.answer.as_ref() {
            let _ = answer.send(result);
        }
    }
}

impl Drop for Finish {
    fn drop(&mut self) {
        // **The sender goes first and the order is the point.** On the panic
        // path nothing was sent, so what the woken frame has to find is a
        // channel with no senders left; waking before this drop would let it
        // read `Empty`, answer "still reading", and go back to sleep with no
        // second wake ever coming — the very bug this exists to close.
        drop(self.answer.take());
        // Failing means the loop has gone, which is the application closing:
        // there is nobody left to tell. Nothing in here may panic, because a
        // panic in a destructor while unwinding aborts the process — which is
        // why this is a `let _` over a `Result` and not an `expect`.
        (self.wake)();
    }
}

/// Why a decode ended with no document.
///
/// Two arms rather than one, because the sentences are not the same: a file
/// Umber will not read is the file's business and names what is wrong with it,
/// where a reader that stopped part way is **Umber's** and can say nothing
/// about the document at all. Folding the second into an [`ImportError`] would
/// also put a threading failure into `umber-core`, which has no threads in it.
#[derive(Debug)]
pub enum Failed {
    /// The reader refused the file, and said why.
    Refused(ImportError),
    /// The worker ended without answering.
    ///
    /// Reachable two ways and both are covered by the one arm: a panic inside
    /// [`umber_core::docimport::import_reporting`], and a thread that could not
    /// be started at all — which drops the closure, and with it the sending end
    /// of the channel, producing exactly this reading.
    WorkerVanished,
}

impl Failed {
    /// What the artist is told, for a decode of `name`.
    ///
    /// Here rather than at the call site so a test can read the sentence
    /// without a device: `App::collect_loading`'s other arm installs a
    /// document, which needs one.
    pub fn notice(&self, name: &str) -> Notice {
        let title = format!("Could not open “{name}”");
        match self {
            Self::Refused(error) => Notice {
                title,
                lines: vec![error.to_string()],
            },
            // Says what happened and stops. There is nothing to send anybody
            // to: the panic message went to stderr, which a copy started from
            // a file manager does not have, and a crash report is deliberately
            // not written for a worker.
            Self::WorkerVanished => Notice {
                title,
                lines: vec![
                    "Something went wrong while reading the file, so Umber stopped part \
                     way through."
                        .to_string(),
                    "Nothing was changed, and your other documents are untouched.".to_string(),
                ],
            },
        }
    }
}

/// What one frame's [`collect`] found.
pub enum Collected {
    /// A document arrived, with the request that asked for it — the caller
    /// needs its name and where the tab should point.
    ///
    /// Boxed because an [`ImportedDocument`] is far larger than the other arm,
    /// and this is returned once per open rather than once per frame.
    Opened(Box<ImportedDocument>, Loading),
    /// Nothing can be installed. The dialog is down and the artist has been
    /// told why.
    Refused,
}

/// Collect a finished decode: take the dialog down, and say what went wrong
/// where nothing can be installed.
///
/// `None` is "still reading", which is every frame but one.
///
/// **In `umber-app` beside the model rather than in `app.rs`**, for the reason
/// [`autosave::collect`](crate::autosave::collect) is where it is: what happens
/// to the dialog and what the artist is told are rules, and a rule that needs
/// neither a window nor a device is one a test can drive. What stays in
/// `app.rs` is the half that genuinely cannot move — installing a document
/// needs the GPU.
pub fn collect(editor: &mut Editor) -> Option<Collected> {
    let result = editor.loading.as_ref().and_then(Loading::take)?;
    // Taken out before either arm, so a refusal cannot leave the dialog up.
    let load = editor.loading.take()?;
    match result {
        Ok(imported) => Some(Collected::Opened(Box::new(imported), load)),
        Err(failed) => {
            let why = match &failed {
                Failed::Refused(error) => error.to_string(),
                Failed::WorkerVanished => "the reader stopped without answering".to_string(),
            };
            log::warn!("could not open {}: {why}", load.path.display());
            editor.notice = Some(failed.notice(&load.name));
            Some(Collected::Refused)
        }
    }
}

/// `done` in the high half, `total` in the low half.
///
/// One `AtomicU32` rather than two, because the two are read together to make a
/// fraction and a torn pair is a bar that jumps past its own end — `done` from
/// one instant over `total` from another. Sixteen bits each is far more than
/// `LayerStack::MAX`, and a file claiming more than 65535 layers is refused by
/// `check_bounds` long before this.
fn pack(done: u32, total: u32) -> u32 {
    (done.min(0xFFFF) << 16) | total.min(0xFFFF)
}

fn unpack(word: u32) -> (u32, u32) {
    (word >> 16, word & 0xFFFF)
}

impl Loading {
    /// Start reading `path`, and hand back the handle to watch it by.
    ///
    /// **When this worker ends unexpectedly, [`collect`] is what notices**, by
    /// the `Disconnected` [`Self::take`] reads off `outcome`, and it takes the
    /// modal down — on the frame [`WakeAtTheEnd`] asks for, which is why that
    /// exists and why this sentence would otherwise be true and useless. Said
    /// beside the spawn rather than only at the collector: the state a dead
    /// worker leaves is made here, and until somebody asked "who notices if the
    /// answer never comes" the answer was nobody.
    pub fn start(
        path: PathBuf,
        name: String,
        record_path: Option<PathBuf>,
        modified: bool,
        proxy: EventLoopProxy<Wake>,
    ) -> Self {
        let progress = Arc::new(AtomicU32::new(0));
        let (sender, outcome) = std::sync::mpsc::channel();

        let worker_progress = Arc::clone(&progress);
        let worker_path = path.clone();
        // Two handles: one the per-layer report moves into, and one kept for
        // the wake *after* the answer is sent. Without the second the last
        // report happens before the final layer is decoded, so nothing would
        // announce the finished document and it would sit in the channel until
        // the artist happened to move the pointer.
        //
        // **A `Drop` rather than a call at the end of the closure**, because the
        // end of the closure is exactly where a panic does not reach. Under
        // `ControlFlow::Wait` a value appearing in a channel is not an event and
        // dropping an `EventLoopProxy` wakes nothing, so a reader that died left
        // the modal on screen — correct in every respect except that nothing
        // would look at it — until some unrelated window event produced a frame.
        // Unwinding runs destructors, so this fires on the panic path and the
        // ordinary one alike, and there is now one statement of the wake instead
        // of two. See [`Finish`] for why it owns the sender.
        let wake_proxy = proxy.clone();
        let finish = Finish {
            answer: Some(sender),
            wake: Box::new(move || {
                let _ = wake_proxy.send_event(Wake);
            }),
        };
        // Named, so the line `crash::report_panic` writes for a worker says
        // which worker. It is the only trace a panic in a reader leaves, since
        // no report is written for a thread that is not `main`.
        let started = std::thread::Builder::new()
            .name("umber-open".to_owned())
            .spawn(move || {
                // `Progress` is an `Fn`, so the last reading cannot be a captured
                // `mut` — it lives in a cell of its own. An atomic rather than a
                // `Cell` because the callback has to be `Sync` too, which is what
                // lets a reader hand it to a worker of its own one day.
                let last = AtomicU32::new(u32::MAX);
                let report = move |done: u32, total: u32| {
                    let word = pack(done, total);
                    // **Only when the reading changes.** A wake is a whole frame of
                    // the interface, and a document with sixty layers would
                    // otherwise ask for sixty of them in as many milliseconds. The
                    // update flow throttles its own byte counter for the same
                    // reason.
                    if last.swap(word, Ordering::Relaxed) != word {
                        worker_progress.store(word, Ordering::Relaxed);
                        let _ = proxy.send_event(Wake);
                    }
                };
                let result = umber_core::docimport::import_reporting(&worker_path, &report);
                finish.answer(result);
            });
        if let Err(e) = started {
            // Nothing else to do, and deliberately so: the closure went with
            // the failure, taking `Finish` with it, so the sender is already
            // dropped and `take` answers `WorkerVanished` — and the wake fired
            // on the way, which costs one frame nobody needed and is the same
            // route a panic takes. `begin_open` asks for a frame straight after
            // this returns anyway, and `collect_loading` runs early in
            // `render`, so a refusal is settled before the modal is drawn once.
            log::error!("could not start the document reader: {e}");
        }

        Self {
            name,
            path,
            modified,
            record_path,
            progress,
            outcome,
        }
    }

    /// How far along, or `None` where the reader has not said yet.
    ///
    /// `None` is drawn as an empty track. A bar that guessed would be claiming
    /// to know something about somebody's file that nothing has read.
    pub fn fraction(&self) -> Option<f32> {
        let (done, total) = unpack(self.progress.load(Ordering::Relaxed));
        (total > 0).then(|| (done as f32 / total as f32).clamp(0.0, 1.0))
    }

    /// What the dialog says under the bar.
    pub fn detail(&self) -> String {
        match unpack(self.progress.load(Ordering::Relaxed)) {
            (_, 0) => "Reading the file…".to_string(),
            (done, total) => format!("Layer {} of {total}", (done + 1).min(total)),
        }
    }

    /// The document, once the worker has finished with it.
    ///
    /// **Three answers out of a channel that offers three readings**, and the
    /// bug this replaces was `try_recv().ok()`, which collapsed the last two:
    /// `Empty` is "still reading" and `Disconnected` is "the sender is gone
    /// without having sent", which cannot happen while the worker is alive.
    /// Mapping the second onto `None` made a dead reader indistinguishable from
    /// a slow one, and the modal above it has no way out. See the module docs.
    pub fn take(&self) -> Option<Result<ImportedDocument, Failed>> {
        match self.outcome.try_recv() {
            Ok(Ok(imported)) => Some(Ok(imported)),
            Ok(Err(error)) => Some(Err(Failed::Refused(error))),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(Failed::WorkerVanished)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two halves survive being packed together, which is the whole point
    /// of packing them: a torn pair is a bar past its own end.
    #[test]
    fn a_reading_survives_being_packed_into_one_word() {
        for (done, total) in [(0, 0), (0, 1), (1, 1), (7, 64), (64, 64), (65535, 65535)] {
            assert_eq!(unpack(pack(done, total)), (done, total), "{done}/{total}");
        }
    }

    /// A count past what the packing holds is clamped rather than wrapped —
    /// wrapping would put `done` into `total`'s half and read as a bar at a
    /// wild position. Unreachable in practice, since `check_bounds` refuses a
    /// stack past `LayerStack::MAX` long before this.
    #[test]
    fn an_absurd_count_clamps_rather_than_wrapping() {
        let (done, total) = unpack(pack(70000, 70000));
        assert_eq!((done, total), (0xFFFF, 0xFFFF));
    }

    // ------------------------------------------------------------- a dead worker

    /// A decode whose worker panicked, built by panicking one.
    ///
    /// A real thread rather than a channel with its sender dropped by hand:
    /// the claim under test is about a *panic*, and that the two leave the same
    /// state is exactly the thing a reader would have to check rather than
    /// assume. Joined before returning, so nothing here races the unwind.
    ///
    /// The panic message goes to stderr through the default hook, which the
    /// test harness captures. `crash::install` is never called in a test, so
    /// nothing else observes it.
    fn vanished(name: &str) -> Loading {
        let (sender, outcome) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("umber-open-test".to_owned())
            .spawn(move || {
                // Held so the unwind is what drops it, which is the whole
                // mechanism: a reader that panics part way through takes the
                // sending end down with it without having sent.
                let _sender = sender;
                panic!("a reader panicked part way through a document");
            })
            .expect("a worker thread");
        assert!(worker.join().is_err(), "the worker was supposed to panic");
        Loading {
            name: name.to_owned(),
            path: PathBuf::from(name),
            modified: false,
            record_path: None,
            progress: Arc::new(AtomicU32::new(0)),
            outcome,
        }
    }

    /// Draw one dialog and answer with every word it put on the screen.
    ///
    /// Reading egui's own output rather than the flag, because the flag is what
    /// the code sets and the words are what the artist gets: a guard that
    /// asserted `ed.loading.is_none()` and stopped could not see a modal still
    /// being drawn from somewhere else.
    ///
    /// **One dialog at a time, so "it is gone" is an empty list.** Asking both
    /// together and then searching for the absence of a phrase makes the
    /// assertion depend on `tabs::loading`'s own wording, which the code under
    /// test is not obliged to keep — reword the modal and the negative half goes
    /// quietly vacuous. Drawn in `ui::draw`'s order where both are asked for.
    fn drawn(
        ed: &mut Editor,
        body: impl Fn(&mut egui::Ui, &crate::theme::Palette, &mut Editor),
    ) -> Vec<String> {
        use crate::theme::{Palette, ThemeKind};

        let ctx = egui::Context::default();
        let palette = Palette::of(ThemeKind::Graphite);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(900.0, 600.0),
            )),
            ..Default::default()
        };
        let mut words = Vec::new();
        // Twice, for `canvasdlg`'s reason: a fresh context builds its font
        // atlas on the first pass and lays a modal out against a half-built one.
        for _ in 0..2 {
            let output = ctx.run_ui(input.clone(), |ui| body(ui, &palette, ed));
            words.clear();
            collect_text(&output.shapes, &mut words);
        }
        words
    }

    /// What the loading modal draws, which is nothing at all once it is down.
    fn loading_words(ed: &mut Editor) -> Vec<String> {
        drawn(ed, |ui, p, ed| crate::tabs::loading(ui, p, ed))
    }

    /// What the notice draws.
    fn notice_words(ed: &mut Editor) -> Vec<String> {
        drawn(ed, crate::tabs::notice)
    }

    fn collect_text(shapes: &[egui::epaint::ClippedShape], into: &mut Vec<String>) {
        fn walk(shape: &egui::Shape, into: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) => into.push(text.galley.text().to_owned()),
                egui::Shape::Vec(inner) => {
                    for shape in inner {
                        walk(shape, into);
                    }
                }
                _ => {}
            }
        }
        for clipped in shapes {
            walk(&clipped.shape, into);
        }
    }

    /// A reader that dies takes the dialog down with it and says so.
    ///
    /// **The window is what is at stake, not the document.** `tabs::loading`
    /// draws a modal with no Cancel for as long as `editor.loading` is set, and
    /// nothing but this collection ever clears it — so before the fix a panic
    /// in any of the five readers left that modal over every open tab for the
    /// life of the process, with no further wake to redraw it and no way out
    /// but killing Umber.
    ///
    /// Both halves are drawn rather than asserted about: the first pass is what
    /// makes the second mean something, since a guard that only looked
    /// afterwards would pass just as well if the modal had never been there.
    #[test]
    fn a_decode_whose_worker_vanished_does_not_hold_the_window() {
        let mut ed = Editor::default();
        ed.loading = Some(vanished("Sketch.clip"));

        let up = loading_words(&mut ed);
        assert!(
            up.iter().any(|w| w.contains("Sketch.clip")),
            "the loading modal was not drawn at all: {up:?}",
        );

        assert!(
            matches!(collect(&mut ed), Some(Collected::Refused)),
            "a worker that ended without answering was read as one still reading",
        );

        // Empty, not "does not contain some phrase": with `editor.loading`
        // cleared that modal draws nothing whatever, so this cannot be made
        // vacuous by rewording it.
        let still = loading_words(&mut ed);
        assert!(still.is_empty(), "the loading modal is still up: {still:?}");

        let said = notice_words(&mut ed);
        assert!(
            said.iter()
                .any(|w| w.contains("Could not open") && w.contains("Sketch.clip")),
            "nothing on screen says what happened: {said:?}",
        );
        // The body as well as the heading. A titled box with nothing in it
        // would pass on the line above alone.
        assert!(
            said.iter().any(|w| w.contains("Nothing was changed")),
            "the notice says nothing about what it cost: {said:?}",
        );
        // The route out. A notice with no way to dismiss it would be the same
        // defect wearing a different sentence.
        assert!(
            said.iter().any(|w| w == "Close"),
            "the notice cannot be dismissed: {said:?}",
        );
    }

    /// A file the reader *refuses* keeps the reader's own sentence.
    ///
    /// The arm the vanish guard cannot see, and the one that carries every
    /// careful sentence `docimport` writes — "the file does not hold its
    /// pixels", the canvas bound against the stack bound, the vector-layer
    /// explanation. Answering `WorkerVanished` for a refusal compiles, keeps
    /// the modal coming down, and replaces all of it with "something went
    /// wrong": a silent downgrade of every refusal in the application, which
    /// nothing else here would notice.
    #[test]
    fn a_file_the_reader_refuses_says_what_the_reader_said() {
        let (sender, outcome) = std::sync::mpsc::channel();
        sender
            .send(Err(ImportError::UnsupportedExtension("qqq".to_owned())))
            .expect("the receiver is alive");
        let mut ed = Editor::default();
        ed.loading = Some(Loading {
            name: "Sketch.qqq".to_owned(),
            path: PathBuf::from("Sketch.qqq"),
            modified: false,
            record_path: None,
            progress: Arc::new(AtomicU32::new(0)),
            outcome,
        });

        assert!(matches!(collect(&mut ed), Some(Collected::Refused)));
        let said = notice_words(&mut ed);
        let wanted = ImportError::UnsupportedExtension("qqq".to_owned()).to_string();
        assert!(
            said.contains(&wanted),
            "the reader's own sentence was replaced: wanted {wanted:?}, drew {said:?}",
        );
    }

    /// And a document that arrives is handed over rather than refused.
    ///
    /// The common path, and it had no cover at all: swap `collect`'s two arms
    /// and every interactive open in the application fails silently while the
    /// suite stays green. The refactor that split this out of `app.rs` is what
    /// made the seam, so it is the refactor's job to close it.
    #[test]
    fn a_document_that_arrives_is_handed_over_to_be_installed() {
        let (sender, outcome) = std::sync::mpsc::channel();
        sender
            .send(Ok(ImportedDocument {
                format: umber_core::docimport::SourceFormat::Png,
                size: glam::UVec2::ONE,
                layers: Vec::new(),
                active: None,
                background: umber_core::Background::Transparent,
                dpi: None,
                history: None,
                warnings: Vec::new(),
            }))
            .expect("the receiver is alive");
        let mut ed = Editor::default();
        ed.loading = Some(Loading {
            name: "Sketch.png".to_owned(),
            path: PathBuf::from("Sketch.png"),
            modified: true,
            record_path: Some(PathBuf::from("Elsewhere.ora")),
            progress: Arc::new(AtomicU32::new(0)),
            outcome,
        });

        match collect(&mut ed) {
            Some(Collected::Opened(_, load)) => {
                // The request travels with the document, because the caller
                // needs all three to make the tab: a name, where it points and
                // whether it counts as modified.
                assert_eq!(load.name, "Sketch.png");
                assert_eq!(load.record_path, Some(PathBuf::from("Elsewhere.ora")));
                assert!(load.modified);
            }
            other => panic!(
                "a document that arrived was not handed over: {:?}",
                other.is_some()
            ),
        }
        assert!(
            ed.notice.is_none(),
            "a document that opened raised a notice"
        );
        assert!(ed.loading.is_none(), "the dialog was left up");
    }

    /// A reader that panics asks for the frame that will notice it, and asks
    /// **after** the channel has gone quiet.
    ///
    /// Two claims and the second is the one a comment got wrong. Noticing is
    /// useless without a frame to notice in — the loop is `ControlFlow::Wait`
    /// and a panic reaches neither of the worker's own wakes — and the wake has
    /// to land *after* the sender is dropped, or the woken frame reads `Empty`,
    /// answers "still reading", and goes back to sleep with nothing left to
    /// wake it. The first draft declared the guard as a body local and asserted
    /// it dropped last; a captured upvar is a field of the closure environment,
    /// which is dropped after every body local, so it dropped **first**.
    ///
    /// Both halves are measured rather than restated: the wake counts itself,
    /// and it reads the receiver at the moment it fires, which is the only
    /// instant the ordering is observable.
    #[test]
    fn a_reader_that_panics_asks_for_a_frame_once_its_channel_is_quiet() {
        use std::sync::atomic::AtomicUsize;

        let (sender, outcome) = std::sync::mpsc::channel();
        let outcome = Arc::new(std::sync::Mutex::new(outcome));
        let wakes = Arc::new(AtomicUsize::new(0));
        // What the receiver answered on the frame this wake asked for.
        let seen = Arc::new(std::sync::Mutex::new(None));

        let watcher = Arc::clone(&outcome);
        let counter = Arc::clone(&wakes);
        let record = Arc::clone(&seen);
        let finish = Finish {
            answer: Some(sender),
            wake: Box::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                let reading = match watcher.lock().expect("the receiver").try_recv() {
                    Ok(_) => "answered",
                    Err(TryRecvError::Empty) => "empty",
                    Err(TryRecvError::Disconnected) => "disconnected",
                };
                *record.lock().expect("the record") = Some(reading);
            }),
        };

        let worker = std::thread::Builder::new()
            .name("umber-open-test".to_owned())
            .spawn(move || {
                let _finish = finish;
                panic!("a reader panicked part way through a document");
            })
            .expect("a worker thread");
        assert!(worker.join().is_err(), "the worker was supposed to panic");

        assert_eq!(
            wakes.load(Ordering::Relaxed),
            1,
            "a reader that died asked for no frame, so nothing would look at it",
        );
        assert_eq!(
            *seen.lock().expect("the record"),
            Some("disconnected"),
            "the wake landed before the channel went quiet, so the frame it \
             asked for would read the decode as still running",
        );
    }

    /// And a reader that is merely slow is left alone.
    ///
    /// The half that stops the fix going too far the other way: `Empty` and
    /// `Disconnected` have to be told apart, and a `take` that answered
    /// `WorkerVanished` for both would take the dialog down over a document
    /// that was still being read and then install nothing. Driven from one
    /// channel so the two readings are the same channel a moment apart.
    #[test]
    fn a_decode_still_reading_is_not_taken_for_a_dead_one() {
        let (sender, outcome) = std::sync::mpsc::channel();
        let load = Loading {
            name: "Slow.ora".to_owned(),
            path: PathBuf::from("Slow.ora"),
            modified: false,
            record_path: None,
            progress: Arc::new(AtomicU32::new(0)),
            outcome,
        };
        assert!(
            load.take().is_none(),
            "a worker that has not answered yet was read as gone",
        );
        drop(sender);
        assert!(
            matches!(load.take(), Some(Err(Failed::WorkerVanished))),
            "a worker that has gone was read as one still reading",
        );
    }
}
