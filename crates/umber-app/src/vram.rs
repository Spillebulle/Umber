//! What Umber says when the graphics card will not hold a document's layers.
//!
//! `umber-render`'s [`Vram`] is the reading and this is the sentence, which is
//! the division `ScrollSpan` and `Clip::place` already keep: the rule is
//! testable without a window and `app.rs` only shows it. It is in `umber-app`
//! rather than `umber-core` for the reason `install_import`'s existing
//! device-limit refusal is — `umber-core` may not learn about the adapter.
//!
//! **Nothing here may state what the card holds.** wgpu exposes no total-memory
//! query: `AdapterInfo` carries none, `Device::generate_allocator_report`
//! reports Umber's *own* sub-allocations rather than the card's capacity or what
//! another process is using, and the only route to a real figure is
//! `Adapter::as_hal`, which costs `ash` and `windows` as direct dependencies, is
//! `unsafe`, is per backend, and is untestable on a runner with no card. A first
//! draft of this wording printed "and this GPU has 10.0 GB" and it is withdrawn
//! — `docs/perf/slot-lifecycle-and-vram.md` §7.2, adopted verbatim by
//! `docs/perf/import-and-limits.md` §5.3 rather than rivalled. The "needs"
//! figure is exactly computable and stays; the "has" figure is not and goes.
//!
//! **Four bounds can refuse a document now and they are genuinely different**,
//! which is why this is its own sentence and not a fifth spelling of an existing
//! one. `CanvasTooLarge` is one edge past what Umber opens; the device's
//! `max_texture_dimension_2d` is one edge past what this card holds;
//! `StackTooLarge` is every layer's pixels in *host* memory; this one is the
//! layer array on the card. Umber has already sent an artist to shrink a canvas
//! that was not the problem once, and a refusal that names the wrong bound is
//! worse than a vague one.
//!
//! **It does not cover the upload, and it makes the upload slightly more likely
//! to fail.** Both halves, because the first on its own reads as a neutral gap
//! and it is not one. The gate this wording belongs to is the layer array
//! allocation alone: `Queue::write_texture`'s staging buffer goes through wgpu's
//! *fatal* error path, so an out-of-memory there loses the device before any
//! error scope sees it, and banding and the per-layer submit in `install_import`
//! bound how much can stand at once without making it catchable. What the
//! second half adds is that `gpu::MEMORY_BUDGET_PERCENT` is charged on
//! **buffers as well as textures**, so a staging allocation that the driver
//! would previously have attempted is now refused at ninety percent of the
//! reported budget — and refused there means the device is gone. The window is
//! narrow, since a band is at most `readback_limit` and a budget too full for
//! that is one the array reservation would already have declined, but it is a
//! trade rather than a free win. So a document that passes this gate can still
//! die on its pixels, and no sentence here may imply otherwise.

//! **What is tested here is the wording, not that anything says it.** The three
//! call sites — `install_import`, `add_layer` and `add_mask` — are guarded by
//! nothing: delete the reservation from any one of them and the whole suite
//! stays green while that path goes back to producing the crash box. That is the
//! "a guard on a model is not a guard on the panel" failure this project records
//! three times over, and it is recorded rather than closed because reaching
//! those call sites means building an `UmberApp` — a window, a device, a
//! surface — which nothing in this crate does today. The honest statement is
//! that the sentences are right and their reachability rests on review.

use umber_core::docimport::gigabytes;
use umber_render::Vram;

use crate::tabs::Notice;

/// What both sentences end on: three levers, cheapest first.
///
/// One statement of it rather than two, because it is the only part an artist
/// can act on and the two refusals would otherwise drift into offering
/// different advice for the same cause.
///
/// **Closing other applications leads, and it was missing from the first
/// draft.** What is being refused is a share of the *budget the driver reports*,
/// which on both Vulkan and D3D12 is what is left after everything else running
/// on the machine — so a browser or a game is a common cause, and it is the only
/// remedy here that costs the artist nothing. Offering flattening first told
/// somebody to take their document apart when quitting something else would have
/// done, which is the refusal naming the wrong bound this module exists to
/// avoid. It states no figure, because a lever is not a measurement.
///
/// Then flattening, which keeps the canvas, and then the canvas, where each
/// halving of the width and the height quarters the figure.
const REMEDY: &str = "Closing other applications that use the graphics card may be enough. \
     Otherwise, flattening or removing some layers, or working at a smaller canvas, will \
     bring it within reach.";

/// A document that could not be given its layer storage.
///
/// The figure is [`Vram::bytes`] — the array on its own — because nothing of
/// this document was resident beside it. The layer count is the artist's own
/// reading of their file and comes from the caller rather than from `Vram`,
/// which counts *slices*: a masked layer is two of those, and telling somebody
/// they have forty-two layers when the panel would show twenty-one is a refusal
/// they cannot check.
pub fn open_refused(name: &str, layers: usize, refused: &Vram) -> Notice {
    Notice {
        title: format!("Could not open “{name}”"),
        lines: vec![format!(
            "This document needs {needed} of graphics memory for its {layers} layers at \
             {w} × {h}, and this graphics card could not provide it. {REMEDY}",
            needed = gigabytes(refused.bytes()),
            w = refused.doc_size.x,
            h = refused.doc_size.y,
        )],
    }
}

/// A layer or a mask that could not be given a slice of an existing document.
///
/// A different sentence from [`open_refused`] and deliberately so: nothing has
/// failed to open, one layer has failed to appear, and the artist's picture is
/// still in front of them.
///
/// The figure is [`Vram::peak_bytes`] rather than [`Vram::bytes`], because
/// growing the array is what was refused and a growth holds the array it is
/// replacing *and* the one it is making — the copy between them is recorded
/// against both. Reporting only the new array would name a figure smaller than
/// the one the device actually declined, on a document whose layers are already
/// resident, which reads as the card refusing something it plainly has room for.
pub fn slice_refused(what: &str, refused: &Vram) -> Notice {
    Notice {
        title: format!("Could not add {what}"),
        lines: vec![format!(
            "Making room for it needs {needed} of graphics memory at {w} × {h}, and this \
             graphics card could not provide it. {REMEDY}",
            needed = gigabytes(refused.peak_bytes()),
            w = refused.doc_size.x,
            h = refused.doc_size.y,
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::UVec2;

    /// 21 slices of a 20000 × 5000 canvas, which is the document the whole of
    /// Stage 1 was written for.
    ///
    /// **The slice figure is a *page*, and it was the canvas until the tile
    /// atlas landed.** A page is the canvas rounded up to whole tiles —
    /// 20224 × 5120 — so this fixture said 400 MB where the renderer produces
    /// 414.2, and the two figures the sentence test pins were 8.4 and 16.8 GB
    /// against a real 8.7 and 17.4. It went on passing, because a fixture that
    /// builds its own arithmetic is a test of the fixture — `umber-render`'s
    /// `a_refused_reservation_states_both_the_array_and_the_transient` was
    /// re-derived and this was not. Taken from `tile::Grid` now, which is the
    /// same source `canvas::slice_bytes` takes it from.
    fn reported(held: u32) -> Vram {
        let doc_size = UVec2::new(20000, 5000);
        let page = umber_core::tile::Grid::new(doc_size).page_size();
        Vram {
            slices: 21,
            held,
            slice_bytes: u64::from(page.x) * u64::from(page.y) * 4,
            doc_size,
        }
    }

    /// **No sentence here may state what the card holds**, because wgpu cannot
    /// say. The module docs have the argument; this is the enforcement, in the
    /// shape `no_row_claims_a_copy_is_complete` and
    /// `no_stage_calls_anything_verified` already take — a build failure on a
    /// word rather than a rule somebody has to remember.
    ///
    /// The forbidden words are the ways a capacity gets stated: a figure the
    /// card "has" or "holds", a "total", how much is "available", "free",
    /// "left" or "remaining", "only" this much, and the claim that it is "out
    /// of" memory (which names a state rather than this allocation). It sweeps
    /// both sentences and the title as well as the body, since a title is where
    /// a figure would be most tempting.
    ///
    /// **"holds" and "only" were missing from the first list**, and "holds" is
    /// the word the module's own first paragraph uses for the thing being
    /// refused — so "this card holds only 10.0 GB" would have walked straight
    /// through the guard written to refuse it. A word list is only as good as
    /// the words somebody would actually reach for.
    #[test]
    fn no_refusal_states_what_the_card_holds() {
        let forbidden = [
            "has ",
            "holds",
            "total",
            "available",
            "free",
            " left",
            "remaining",
            "only",
            "out of memory",
            " of vram",
            "capacity",
        ];
        let notices = [
            open_refused("sketch.clip", 21, &reported(0)),
            slice_refused("a layer", &reported(21)),
            slice_refused("a mask", &reported(21)),
        ];
        for notice in &notices {
            for text in std::iter::once(&notice.title).chain(&notice.lines) {
                let lower = text.to_lowercase();
                for word in forbidden {
                    assert!(
                        !lower.contains(word),
                        "“{text}” claims “{word}”, which wgpu cannot report"
                    );
                }
            }
        }
    }

    /// The figure is the one the device actually declined, and the two refusals
    /// need different ones.
    ///
    /// An open has nothing resident beside the array, so its figure is the array
    /// — 21 slices at 414 MB each, which is what a *page* of this canvas costs.
    /// A growth holds the array it replaces as well,
    /// so its figure is `c + n`; reading `bytes` there would understate by the
    /// whole of the document already on the card. This measures the strings that
    /// are drawn rather than the accessors: swapping `peak_bytes` for `bytes` in
    /// `slice_refused` compiles, reads plausibly, and is caught here.
    #[test]
    fn each_refusal_names_the_figure_the_device_declined() {
        let open = open_refused("sketch.clip", 21, &reported(0));
        assert!(
            open.lines[0].contains("8.7 GB"),
            "an open names the array it asked for: {}",
            open.lines[0]
        );
        assert!(
            open.lines[0].contains("21 layers") && open.lines[0].contains("20000 × 5000"),
            "an open names the stack and the canvas: {}",
            open.lines[0]
        );

        let grown = slice_refused("a layer", &reported(21));
        assert!(
            grown.lines[0].contains("17.4 GB"),
            "a growth names the array it replaces as well as the one it makes: {}",
            grown.lines[0]
        );
    }

    /// Both refusals end on something to do. A sentence saying only that the
    /// card said no leaves the artist with a dialog and no next step, which is
    /// the failure `StackTooLarge`'s own wording was rewritten for.
    #[test]
    fn every_refusal_offers_a_lever() {
        for notice in [
            open_refused("sketch.clip", 21, &reported(0)),
            slice_refused("a layer", &reported(21)),
        ] {
            let line = notice.lines[0].to_lowercase();
            // All three, and the first is the one that costs nothing: what was
            // refused is a share of a budget shared with everything else on the
            // machine, so another application is a common cause and quitting it
            // is a remedy the artist need not take their picture apart for.
            // Dropping it and keeping the other two would still pass a guard
            // that only asked whether *a* lever was offered.
            for lever in ["other applications", "flattening", "smaller canvas"] {
                assert!(line.contains(lever), "no “{lever}” in: {line}");
            }
        }
    }
}
