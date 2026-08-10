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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use umber_core::docimport::{ImportError, ImportedDocument};
use winit::event_loop::EventLoopProxy;

use crate::app::Wake;

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
        let finished = proxy.clone();
        std::thread::spawn(move || {
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
            // The send can fail only if the application has dropped the handle,
            // which is a document nobody is waiting for any more.
            let _ = sender.send(result);
            // The wake that matters most: the last per-layer report happened
            // before the final layer was decoded, so this is the only thing
            // that says the document is ready.
            let _ = finished.send_event(Wake);
        });

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
    pub fn take(&self) -> Option<Result<ImportedDocument, ImportError>> {
        self.outcome.try_recv().ok()
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
}
