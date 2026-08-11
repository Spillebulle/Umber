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
//! **It does not cover the upload.** The gate this wording belongs to is the
//! layer array allocation alone. `Queue::write_texture`'s staging buffer goes
//! through wgpu's *fatal* error path, so an out-of-memory there loses the device
//! before any error scope sees it; banding and the per-layer submit in
//! `install_import` bound how much can stand at once but do not make it
//! catchable. So a document that passes this gate can still die on its pixels,
//! and no sentence here may imply otherwise.

use umber_core::docimport::gigabytes;
use umber_render::Vram;

use crate::tabs::Notice;

/// What both sentences end on: two levers, in the order they cost the artist.
///
/// One statement of it rather than two, because it is the only part an artist
/// can act on and the two refusals would otherwise drift into offering
/// different advice for the same cause. "Flattening or removing" first, because
/// it keeps the canvas; the canvas second, because each halving of the width and
/// height quarters the figure and that is the larger lever.
const REMEDY: &str = "Flattening or removing some layers, or working at a smaller canvas, will bring it \
     within reach.";

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
    fn reported(held: u32) -> Vram {
        let doc_size = UVec2::new(20000, 5000);
        Vram {
            slices: 21,
            held,
            slice_bytes: u64::from(doc_size.x) * u64::from(doc_size.y) * 4,
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
    /// card "has", how much is "available", "free", "left" or "remaining", and
    /// the claim that it is "out of" memory (which names a state rather than
    /// this allocation). It sweeps both sentences and the title as well as the
    /// body, since a title is where a figure would be most tempting.
    #[test]
    fn no_refusal_states_what_the_card_holds() {
        let forbidden = [
            "has ",
            "available",
            "free",
            " left",
            "remaining",
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
    /// — 21 slices at 400 MB each. A growth holds the array it replaces as well,
    /// so its figure is `c + n`; reading `bytes` there would understate by the
    /// whole of the document already on the card. This measures the strings that
    /// are drawn rather than the accessors: swapping `peak_bytes` for `bytes` in
    /// `slice_refused` compiles, reads plausibly, and is caught here.
    #[test]
    fn each_refusal_names_the_figure_the_device_declined() {
        let open = open_refused("sketch.clip", 21, &reported(0));
        assert!(
            open.lines[0].contains("8.4 GB"),
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
            grown.lines[0].contains("16.8 GB"),
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
            assert!(
                notice.lines[0].contains("Flattening")
                    && notice.lines[0].contains("smaller canvas"),
                "no lever in: {}",
                notice.lines[0]
            );
        }
    }
}
