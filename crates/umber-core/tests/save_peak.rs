//! What a save costs the host, measured rather than argued.
//!
//! `docs/perf/formats-and-host-memory.md` §10.1 and §10.2 are about one number:
//! writing a document used to hold every layer in host memory at once, so a
//! twenty-four-slice document at 400 MB a slice was ten gigabytes — every five
//! minutes, unattended, for the autosave. The remedy is
//! [`docformat::Canvas::Deferred`] and a [`docformat::Canvases`] source, and the
//! claim it makes is not "less memory" but something sharper and checkable:
//! **the peak stops following the layer count.**
//!
//! Nothing else in the suite can see that. Every other guard here asks what the
//! file holds, and a save that quietly allocated the whole stack would write a
//! byte-perfect archive. So this binary installs a counting allocator and
//! measures, which is what a `#[global_allocator]` is doing in a test at all —
//! it applies to this test binary alone and to nothing else in the workspace.
//!
//! **It asserts a shape, never a figure.** The absolute peak depends on the PNG
//! encoder's own buffers and on how much of the canvas survives `trim`, both of
//! which are free to change; that the deferred peak is *flat in the number of
//! layers* is the property the design turns on. There is no wall-clock
//! assertion anywhere in here, so it means the same thing on a CI runner as on
//! a desktop.

use std::alloc::{GlobalAlloc, Layout, System};
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};

use glam::UVec2;
use umber_core::docformat::{self, Canvas, Canvases, SaveDocument, SaveError, SaveLayer};
use umber_core::document::Background;
use umber_core::layer::BlendMode;

/// Live bytes, and the most there have ever been since the last reset.
///
/// `saturating_sub` because the counter is reset partway through a run, so
/// allocations made before a reset are freed after it; the peak is what is
/// being read and an underflowing live count would only make it *lower*.
///
/// **These are process-globals and nothing locks them, which is safe only
/// because this binary holds exactly one `#[test]`.** Add a second and the
/// harness runs them on parallel threads, every reading becomes the sum of two
/// tests' allocations, and — because the assertion below is an *upper bound on
/// a difference* — the likely outcome is that it passes while measuring
/// nothing. That is CLAUDE.md's "a test that writes a process-global must take
/// a lock, and the harness will not tell you it does not", and the cheapest
/// answer here is the one-test rule rather than a mutex, because a second test
/// would also have to run alone to mean anything.
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let _ = LIVE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |live| {
            Some(live.saturating_sub(layout.size()))
        });
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Small enough to be quick and large enough that a canvas dwarfs the
/// bookkeeping round it: one slice is 640 KB.
const SIDE: u32 = 400;

/// Deliberately not flat, and deliberately opaque everywhere. `trim` crops to
/// the non-transparent box, so a mostly empty layer would make the deferred
/// path look good for a reason that has nothing to do with this change.
fn canvas(seed: usize) -> Vec<u8> {
    (0..SIDE * SIDE)
        .flat_map(|i| {
            let v = (i.wrapping_mul(seed as u32 + 7) % 251) as u8;
            [v, v / 2, 255 - v, 255]
        })
        .collect()
}

/// A source that builds each buffer when it is asked for and hands over
/// ownership, so the caller is holding none of them.
///
/// This is `app.rs`'s `SaveSource` in miniature: there a buffer arrives from
/// `read_layer_rect`, here from arithmetic, and in both the writer is the only
/// thing holding one.
struct OneAtATime;

impl Canvases for OneAtATime {
    fn layer(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
        Ok(Cow::Owned(canvas(index)))
    }

    fn mask(&mut self, index: usize) -> Result<Cow<'_, [u8]>, SaveError> {
        Ok(Cow::Owned(canvas(index + 100)))
    }

    fn merged(&mut self) -> Result<Cow<'_, [u8]>, SaveError> {
        Ok(Cow::Owned(canvas(999)))
    }
}

fn scratch() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("umber-save-peak-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The peak while a document of `layers` layers is written with nothing held.
fn deferred_peak(path: &std::path::Path, layers: usize) -> usize {
    let stack: Vec<SaveLayer<'_>> = (0..layers)
        .map(|_| SaveLayer::new("Ink", BlendMode::Normal, Canvas::Deferred))
        .collect();
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    docformat::save_from(
        path,
        &SaveDocument {
            size: UVec2::new(SIDE, SIDE),
            layers: &stack,
            active: 0,
            background: Background::Transparent,
            dpi: 72.0,
            merged: Canvas::Deferred,
            history: None,
        },
        &mut OneAtATime,
    )
    .expect("save");
    PEAK.load(Ordering::Relaxed)
}

/// The peak while the same document is written the way it used to be: every
/// buffer in host memory, and the archive assembled in a `Vec<u8>`.
fn held_peak(path: &std::path::Path, layers: usize) -> usize {
    let buffers: Vec<Vec<u8>> = (0..layers).map(canvas).collect();
    let merged = canvas(999);
    let stack: Vec<SaveLayer<'_>> = buffers
        .iter()
        .map(|p| SaveLayer::new("Ink", BlendMode::Normal, p))
        .collect();
    LIVE.store(0, Ordering::Relaxed);
    PEAK.store(0, Ordering::Relaxed);
    let (bytes, _) = docformat::encode(&SaveDocument {
        size: UVec2::new(SIDE, SIDE),
        layers: &stack,
        active: 0,
        background: Background::Transparent,
        dpi: 72.0,
        merged: Canvas::Held(&merged),
        history: None,
    })
    .expect("encode");
    docformat::write_encoded(path, &bytes).expect("write");
    // The buffers themselves are live for the whole of the above and are
    // *deliberately* not counted: the reset happens after they are built, so
    // what is measured is only what the writer added on top of them. That makes
    // the comparison below strictly conservative — the real old peak was this
    // plus the whole stack.
    drop(buffers);
    PEAK.load(Ordering::Relaxed)
}

/// **The deferred peak does not grow by a canvas a layer, and the held one
/// does.**
///
/// Both halves are needed. Without the second, a save that had quietly stopped
/// allocating anything at all would pass; without the first, there is no claim.
///
/// It does grow a little, and by what is worth naming: a `<layer>` element and
/// a ZIP central-directory record per entry, both of which the writer has to
/// hold until it knows where every entry landed. Measured at 400², that is
/// about 27 KB a layer against a 640 KB slice.
#[test]
fn a_deferred_save_costs_the_same_whatever_the_stack_is() {
    let dir = scratch();
    let path = dir.join("peak.ora");
    let slice = SIDE as usize * SIDE as usize * 4;

    let few = deferred_peak(&path, 2);
    let many = deferred_peak(&path, 24);

    // Twenty-two more layers, for less than two slices in total, where the path
    // this replaced cost twenty-two. The honest margin is about 2.2x — the
    // measured growth is a little over half a slice — and it is a bound on the
    // *shape* rather than a tight one: a per-layer cost that had crept up to a
    // tenth of a canvas would still trip it. No figure is quoted here on
    // purpose; the `println!` below prints the ones somebody would want, which
    // is the only way a comment cannot go stale against the code beside it.
    assert!(
        many >= few,
        "the peak fell as layers were added, which means the reading is noise \
         rather than a measurement: {few} for 2 layers, {many} for 24"
    );
    assert!(
        many - few < 2 * slice,
        "22 more layers cost {} bytes, which is a slice ({slice}) or more each: \
         {few} for 2 layers, {many} for 24",
        many - few,
    );

    // And the shape it is being compared against is real: written the old way,
    // twelve times the layers costs several times the memory. This is measured
    // *without* the caller's own buffers in the count, so it is the weaker of
    // the two readings and still separates them.
    let held_few = held_peak(&path, 2);
    let held_many = held_peak(&path, 24);
    assert!(
        held_many > held_few * 2,
        "the held path stopped following the layer count, so this test compares \
         nothing: {held_few} bytes for 2 layers, {held_many} for 24"
    );

    // Not assertions — the figures to quote, and to re-measure rather than
    // repeat from a comment.
    println!(
        "{SIDE}x{SIDE}, one slice {} KB\n  \
         deferred: {few} -> {many} bytes for 2 -> 24 layers ({} B per extra layer)\n  \
         held:     {held_few} -> {held_many} bytes ({} B per extra layer, \
         and the caller's own {} B of buffers on top)",
        slice / 1024,
        (many - few) / 22,
        (held_many - held_few) / 22,
        22 * slice,
    );

    let _ = std::fs::remove_file(&path);
}
