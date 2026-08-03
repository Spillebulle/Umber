//! Umber's own clipboard: a rectangle of pixels, copied and pasted.
//!
//! # What is on it
//!
//! **Straight-alpha sRGB RGBA8** — the form every interchange format stores and
//! the form `docimport::srgb` already converts to and from. Not the layer
//! texture's own bytes, and deliberately so: a clipboard is the boundary where
//! a picture stops belonging to one layer, and the day this grows a system
//! clipboard the bytes it has to hand over are these. Keeping the conversion at
//! copy and paste rather than at that boundary would mean discovering it twice.
//!
//! Both directions go through `srgb::encode_pixel` and `srgb::decode_pixel`,
//! which are exact inverses over every reachable (colour, alpha) pair — the
//! same pair a save and a reopen use, and pinned by the same test. Copying a
//! region and pasting it straight back therefore restores the bytes it started
//! with.
//!
//! # What a paste does
//!
//! [`Clip::place`] answers it, and it answers it here rather than in the
//! interface because "where does the picture go" is a rule and rules are
//! testable without a window:
//!
//! * It lands **centred on the point the caller names** — the middle of the
//!   selection where there is one, and otherwise the middle of what the artist
//!   is looking at. Pasting into the corner of a canvas somebody is not
//!   currently looking at is how a paste appears to have done nothing.
//! * If it would hang off an edge and it *fits*, it is nudged back on. A paste
//!   that immediately needs dragging into view is worse than one that arrived
//!   somewhere slightly different from where the pointer was.
//! * If it is **larger than the canvas** it is centred and **cropped** to what
//!   fits, on each axis independently. That is a real loss and it is stated
//!   here rather than hidden: a floating region lives in canvas-sized storage,
//!   so pixels beyond the edge have nowhere to be held, and the alternative —
//!   silently scaling the picture down to fit — would change what was copied.
//! * If nothing at all would land on the canvas, [`Clip::place`] answers `None`
//!   and the caller has nothing to do.
//!
//! # What is not here
//!
//! No system clipboard, no formats, no history of what was copied before. Each
//! is a real feature; the first has to answer for a dependency on every
//! platform Umber ships to and does not have one yet.

use crate::docimport::srgb;
use crate::geom::PixelRect;
use crate::selection::Selection;
use glam::{UVec2, Vec2};

/// A rectangle of pixels taken off a layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    width: u32,
    height: u32,
    /// `width * height * 4`, row-major, straight-alpha sRGB.
    pixels: Vec<u8>,
}

/// A paste, resolved against a canvas: where it goes and what actually reaches
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placed {
    pub rect: PixelRect,
    /// `rect`-sized, in **layer-texture form** — sRGB with alpha premultiplied
    /// in linear space — which is what `write_texture` wants.
    pub pixels: Vec<u8>,
}

/// A cut: what went on the clipboard, and what the layer is left holding.
///
/// The two halves are produced by one pass over one buffer, deliberately —
/// see [`Clip::cut_from_layer`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cut {
    pub clip: Clip,
    /// `rect`-sized, in **layer-texture form** — sRGB with alpha premultiplied
    /// in linear space — ready for `write_layer_rect`.
    pub remainder: Vec<u8>,
}

impl Clip {
    /// Take `rect` of a layer, clipped by `mask`.
    ///
    /// `bytes` is `rect`-sized in layer-texture form, straight off
    /// `read_layer_rect`. `None` when there is nothing worth copying — a
    /// zero-area rectangle, or a buffer that does not match it.
    ///
    /// The mask **bounds** alpha rather than scaling it, and it does so on the
    /// straight-alpha side of the conversion. Two rules, and each was a bug:
    ///
    /// * `min(alpha, coverage)`, not `alpha * coverage`. Painting is clipped by
    ///   the selection in the dab pass, so a pixel the selection half covers
    ///   already holds half a stroke's alpha; multiplying by the coverage again
    ///   would take a quarter of it and the copy would come back with an edge
    ///   fainter than the mark it was taken from — which also made the module's
    ///   own promise that a copy and a paste straight back restore the bytes
    ///   they started with false for anything painted inside a selection. Of the
    ///   alpha that is there, the part inside the selection is at most the
    ///   coverage, and this takes it to be exactly that. `transform.wgsl`'s
    ///   `fs_mask` is the same rule for the lift, and the two must agree: a copy
    ///   and a cut of the same selection differ in what they leave behind, never
    ///   in what they pick up.
    /// * On the straight-alpha side. Scaling the stored bytes instead is wrong
    ///   by a full gamma curve, which is the same trap `srgb`'s module docs
    ///   describe for an import — and it is invisible on anything fully opaque,
    ///   so it would ship.
    pub fn from_layer(rect: PixelRect, bytes: &[u8], mask: Option<&Selection>) -> Option<Self> {
        Self::take(rect, bytes, mask, false).map(|cut| cut.clip)
    }

    /// The same take, plus **what the layer must be left holding**: a cut.
    ///
    /// The removal has to be the exact complement of what the copy took, or a
    /// cut leaves the ghost outline a masked lift used to leave — the copy
    /// keeping `alpha × coverage` while the layer gave up `alpha × (1 −
    /// coverage)` computed separately, the two rounding the same way and a rim
    /// of coverage surviving in both places.
    ///
    /// So it is not computed separately. One pass produces both, the share that
    /// leaves is subtracted from the alpha that was there, and `taken + left ==
    /// before` is therefore true byte for byte rather than to within a rounding
    /// rule — `a_cut_takes_exactly_what_it_leaves_behind` drives every
    /// (alpha, coverage) pair through it.
    ///
    /// `None` on exactly the terms [`Clip::from_layer`] answers `None`: nothing
    /// was taken, so there is nothing to remove either and the layer must not
    /// be written to.
    pub fn cut_from_layer(rect: PixelRect, bytes: &[u8], mask: Option<&Selection>) -> Option<Cut> {
        Self::take(rect, bytes, mask, true)
    }

    /// The one implementation of "what a selection takes off a layer".
    ///
    /// `remove` decides only whether the other half — what is left — is built
    /// as well; the share taken is arrived at identically either way, which is
    /// what makes a copy and a cut agree about the edge of a soft selection.
    fn take(rect: PixelRect, bytes: &[u8], mask: Option<&Selection>, remove: bool) -> Option<Cut> {
        if rect.width == 0 || rect.height == 0 || bytes.len() as u64 != rect.area() * 4 {
            return None;
        }
        let mut pixels = Vec::with_capacity(bytes.len());
        let mut remainder = Vec::with_capacity(if remove { bytes.len() } else { 0 });
        for row in 0..rect.height {
            for col in 0..rect.width {
                let i = ((row * rect.width + col) * 4) as usize;
                let mut px =
                    srgb::decode_pixel([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
                let before = px[3];
                if let Some(mask) = mask {
                    // `min`, not a multiply: painting inside a selection is
                    // already clipped by this same coverage in the dab pass, so
                    // scaling by it again is the double application that left a
                    // ghost of the outline behind. See the transform lift.
                    px[3] = px[3].min(mask.coverage_at(rect.x + col, rect.y + row));
                }
                if remove {
                    // The colour is untouched — a cut is about coverage, not
                    // about what was painted — and only the alpha moves. By
                    // *subtraction*, so the two halves add back up to the pixel
                    // that was there whatever the rounding did, and whatever
                    // rule above decided how much the cut takes.
                    let mut left = px;
                    left[3] = before - px[3];
                    remainder.extend_from_slice(&srgb::encode_pixel(left));
                }
                pixels.extend_from_slice(&px);
            }
        }
        // Everything the mask covered was transparent, or the mask covered
        // nothing of it. An empty clipboard and no clipboard are the same thing
        // to every caller.
        if pixels.chunks_exact(4).all(|px| px[3] == 0) {
            return None;
        }
        Some(Cut {
            clip: Self {
                width: rect.width,
                height: rect.height,
                pixels,
            },
            remainder,
        })
    }

    /// Build one from straight-alpha sRGB bytes — what an image on a system
    /// clipboard would be, and what a test writes by hand.
    pub fn from_rgba(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        if width == 0 || height == 0 || pixels.len() as u64 != width as u64 * height as u64 * 4 {
            return None;
        }
        Some(Self {
            width,
            height,
            pixels,
        })
    }

    pub fn size(&self) -> UVec2 {
        UVec2::new(self.width, self.height)
    }

    /// The pixels as they are held: straight-alpha sRGB.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Resolve a paste centred on `centre` against a canvas of `doc`.
    ///
    /// See the module docs for the three cases this decides between.
    pub fn place(&self, doc: UVec2, centre: Vec2) -> Option<Placed> {
        let x = axis(self.width, doc.x, centre.x)?;
        let y = axis(self.height, doc.y, centre.y)?;
        let rect = PixelRect {
            x: x.on_canvas,
            y: y.on_canvas,
            width: x.span,
            height: y.span,
        };

        let mut pixels = Vec::with_capacity((rect.area() * 4) as usize);
        for row in 0..rect.height {
            let src_row = row + y.in_clip;
            for col in 0..rect.width {
                let i = ((src_row * self.width + col + x.in_clip) * 4) as usize;
                pixels.extend_from_slice(&srgb::encode_pixel([
                    self.pixels[i],
                    self.pixels[i + 1],
                    self.pixels[i + 2],
                    self.pixels[i + 3],
                ]));
            }
        }
        Some(Placed { rect, pixels })
    }
}

/// One axis of [`Clip::place`]: where the paste starts on the canvas, where it
/// starts within the clip, and how much of it lands.
struct Axis {
    on_canvas: u32,
    in_clip: u32,
    span: u32,
}

/// Centre `len` on `centre` within `doc`, nudging it on where it fits and
/// cropping it where it does not.
///
/// `None` when nothing of it lands, which cannot happen for a clip that fits
/// but can for one dragged well past the edge by a strange centre.
fn axis(len: u32, doc: u32, centre: f32) -> Option<Axis> {
    if len == 0 || doc == 0 {
        return None;
    }
    // Rounded rather than truncated: a 9-pixel clip centred on 20.0 should
    // start at 15.5 -> 16, not at 15, or an odd clip drifts half a pixel
    // upwards every time it is pasted.
    let ideal = (centre - len as f32 * 0.5).round();
    let start = if len <= doc {
        // It fits, so put it on. Clamping both ways in this order is what makes
        // "nudge it back on" fall out of one line.
        (ideal.max(0.0) as u32).min(doc - len)
    } else {
        // It does not fit, so the ideal position is ignored: an oversized paste
        // is centred on the canvas, because that is the only placement where
        // what is lost is lost evenly.
        return Some(Axis {
            on_canvas: 0,
            in_clip: (len - doc) / 2,
            span: doc,
        });
    };
    Some(Axis {
        on_canvas: start,
        in_clip: 0,
        span: len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    const DOC: UVec2 = UVec2::splat(64);

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Clip {
        let pixels = px
            .iter()
            .copied()
            .cycle()
            .take((w * h * 4) as usize)
            .collect();
        Clip::from_rgba(w, h, pixels).expect("a clip")
    }

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    /// Copy a region and paste it straight back: the bytes have to be the ones
    /// that came off the layer. Both directions go through the same exact
    /// inverse pair a save and a reopen use, so anything less than equality
    /// means a document would drift every time somebody moved something.
    #[test]
    fn a_copy_and_a_paste_are_exact_inverses() {
        // Layer-texture form: premultiplied, so no component may exceed alpha.
        let layer: Vec<u8> = vec![
            200, 100, 50, 255, // opaque
            0, 0, 0, 0, // clear
            90, 40, 20, 128, // half covered
            33, 33, 33, 200,
        ];
        let clip = Clip::from_layer(rect(0, 0, 2, 2), &layer, None).expect("a clip");
        let placed = clip.place(DOC, vec2(32.0, 32.0)).expect("somewhere to go");
        assert_eq!(placed.pixels, layer);
    }

    /// The mask scales alpha, and on the straight-alpha side of the conversion.
    /// Scaling the stored bytes is the tempting one-liner and is wrong by a
    /// gamma curve on everything that is not fully opaque.
    #[test]
    fn a_copy_takes_only_what_the_selection_covers() {
        let selection =
            Selection::rectangle(vec2(0.0, 0.0), vec2(2.0, 4.0), DOC).expect("a selection");
        let layer: Vec<u8> = (0..16).flat_map(|_| [200, 100, 50, 255]).collect();
        let clip = Clip::from_layer(rect(0, 0, 4, 4), &layer, Some(&selection)).expect("a clip");
        let px = |x: usize, y: usize| {
            let i = (y * 4 + x) * 4;
            [
                clip.pixels()[i],
                clip.pixels()[i + 1],
                clip.pixels()[i + 2],
                clip.pixels()[i + 3],
            ]
        };
        assert_eq!(px(0, 0)[3], 255, "inside the selection");
        assert_eq!(px(3, 0)[3], 0, "outside it");
        // And the colour is untouched where it was kept: masking is about
        // coverage, not about what colour the artist painted.
        assert_eq!(px(0, 0)[0], srgb::decode_pixel([200, 100, 50, 255])[0]);
    }

    /// Paint made *through* a selection is copied whole, and pasting it back
    /// restores the bytes it came from.
    ///
    /// The mask used to scale alpha, which applied it a second time to pixels
    /// that had already been clipped by it in the dab pass: an edge painted at
    /// half coverage came back at a quarter. That is the module's own promise —
    /// a copy and a paste straight back restore what they started with — broken
    /// for every antialiased boundary, and the same arithmetic that left a ghost
    /// of the outline behind a lift.
    #[test]
    fn a_copy_of_paint_made_inside_the_selection_takes_it_whole() {
        // The boundary falls down the middle of column 1, so it is covered
        // 128/255 and the paint there is 128/255 — exactly what painting
        // through this selection produces.
        let selection =
            Selection::rectangle(vec2(0.0, 0.0), vec2(1.5, 4.0), DOC).expect("a selection");
        assert_eq!(selection.coverage_at(0, 0), 255, "column 0 is fully in");
        let edge = selection.coverage_at(1, 0);
        assert!(
            (1..255).contains(&edge),
            "the selection's edge is not antialiased, so this test would pass \
             on the arithmetic it exists to refuse"
        );

        // A layer holding exactly what a white stroke clipped to this selection
        // leaves: alpha is the coverage, premultiplied and encoded.
        let mut layer: Vec<u8> = Vec::new();
        for y in 0..4 {
            for x in 0..4 {
                layer.extend_from_slice(&srgb::encode_pixel([
                    255,
                    255,
                    255,
                    selection.coverage_at(x, y),
                ]));
            }
        }

        let clip = Clip::from_layer(rect(0, 0, 4, 4), &layer, Some(&selection)).expect("a clip");
        // Alpha of pixel (1, 0) — the half-covered column.
        assert_eq!(
            clip.pixels()[7],
            edge,
            "the selection was applied to paint it had already clipped"
        );

        let placed = clip.place(DOC, vec2(2.0, 2.0)).expect("somewhere to go");
        assert_eq!(placed.pixels, layer, "a paste straight back moved a pixel");
    }

    /// Nothing selected but nothing under it either. An empty clipboard and no
    /// clipboard are the same thing, so a paste after this cannot put an
    /// invisible rectangle down and call it an edit.
    #[test]
    fn copying_nothing_leaves_the_clipboard_alone() {
        let layer = vec![0u8; 4 * 4 * 4];
        assert!(Clip::from_layer(rect(0, 0, 4, 4), &layer, None).is_none());
        assert!(Clip::from_layer(rect(0, 0, 0, 4), &[], None).is_none());
        // A buffer that does not match its rectangle is a caller bug, and
        // reading past it would be worse than refusing.
        assert!(Clip::from_layer(rect(0, 0, 4, 4), &[1, 2, 3, 4], None).is_none());
    }

    /// **The invariant a cut lives by.** What goes on the clipboard and what
    /// stays on the layer are the two halves of the pixel that was there, so
    /// they must add back up to it — exactly, on every pixel, for every
    /// coverage a soft edge can produce. Computing the removal separately as
    /// `alpha × (1 − coverage)` looks equivalent and is not: both sides round
    /// to nearest, so a rim of the selection's edge survives in the copy *and*
    /// on the layer, which is the ghost outline a masked lift used to leave.
    #[test]
    fn a_cut_takes_exactly_what_it_leaves_behind() {
        const N: u32 = 16;
        let doc = UVec2::splat(N);
        // A triangle, so the edge runs diagonally across the pixel grid and
        // every intermediate coverage the rasteriser can make is exercised.
        let selection = Selection::from_rings(
            vec![vec![vec2(1.0, 1.0), vec2(14.5, 3.5), vec2(4.5, 14.0)]],
            doc,
        )
        .expect("a selection");

        // Every alpha from 0 to 255 across the rectangle, at a colour that is
        // not grey so a channel swap would show up too.
        let layer: Vec<u8> = (0..N * N)
            .flat_map(|i| {
                let a = (i * 255 / (N * N - 1)) as u8;
                srgb::encode_pixel([200, 90, 30, a])
            })
            .collect();
        let r = rect(0, 0, N, N);
        let cut = Clip::cut_from_layer(r, &layer, Some(&selection)).expect("a cut");

        assert_eq!(cut.remainder.len(), layer.len());
        let mut partials = 0;
        for i in (0..layer.len()).step_by(4) {
            let before = layer[i + 3];
            let taken = cut.clip.pixels()[i + 3];
            let left = cut.remainder[i + 3];
            assert_eq!(
                taken as u32 + left as u32,
                before as u32,
                "pixel {} lost or gained coverage",
                i / 4
            );
            if taken > 0 && left > 0 {
                partials += 1;
            }
        }
        assert!(
            partials > 0,
            "the fixture found no partly covered pixels, so it is testing nothing"
        );

        // And the copy is the same copy: a cut must not take a different share
        // from what Ctrl+C would have taken.
        let copied = Clip::from_layer(r, &layer, Some(&selection)).expect("a clip");
        assert_eq!(cut.clip, copied);
    }

    /// No selection means the whole rectangle, so the layer is left with
    /// nothing — which is what "cut everything" has to mean. Stated because the
    /// complement rule above is what produces it rather than a special case.
    #[test]
    fn a_cut_with_nothing_selected_empties_the_rectangle() {
        let layer: Vec<u8> = (0..4).flat_map(|_| [90, 40, 20, 128]).collect();
        let cut = Clip::cut_from_layer(rect(0, 0, 2, 2), &layer, None).expect("a cut");
        assert!(
            cut.remainder.iter().all(|b| *b == 0),
            "something was left behind"
        );
        // And what it took is byte for byte what a copy of the same rectangle
        // would have taken, which is the round trip pinned above.
        let placed = cut.clip.place(DOC, vec2(32.0, 32.0)).expect("somewhere");
        assert_eq!(placed.pixels, layer);
    }

    /// A cut over empty canvas must not write to the layer at all. `None` on
    /// exactly the terms a copy answers `None`: there is nothing to put on the
    /// clipboard, so there is nothing to take off the layer, and an undo entry
    /// for it would be a row that restores pixels nothing changed.
    #[test]
    fn cutting_nothing_leaves_the_layer_alone() {
        let layer = vec![0u8; 4 * 4 * 4];
        assert!(Clip::cut_from_layer(rect(0, 0, 4, 4), &layer, None).is_none());
        assert!(Clip::cut_from_layer(rect(0, 0, 0, 4), &[], None).is_none());
    }

    /// A paste arrives where the artist is looking, and comes back on to the
    /// canvas rather than hanging off it — a paste that has to be dragged into
    /// view before it can be seen looks like a paste that did nothing.
    #[test]
    fn a_paste_lands_centred_and_is_nudged_back_on_to_the_canvas() {
        let clip = solid(10, 10, [255, 0, 0, 255]);
        let middle = clip.place(DOC, vec2(32.0, 32.0)).expect("somewhere to go");
        assert_eq!(middle.rect, rect(27, 27, 10, 10));

        let corner = clip.place(DOC, vec2(1.0, 1.0)).expect("somewhere to go");
        assert_eq!(corner.rect, rect(0, 0, 10, 10), "not nudged back on");

        let far = clip
            .place(DOC, vec2(200.0, 200.0))
            .expect("somewhere to go");
        assert_eq!(far.rect, rect(54, 54, 10, 10), "not nudged back on");
    }

    /// A clip bigger than the canvas is centred and cropped, one axis at a
    /// time. The loss is real and this is where it is stated: a floating region
    /// lives in canvas-sized storage, so there is nowhere to hold what hangs
    /// off — and scaling the picture down instead would change what was copied.
    #[test]
    fn a_paste_larger_than_the_canvas_is_centred_and_cropped() {
        let clip = solid(100, 20, [0, 255, 0, 255]);
        let placed = clip.place(DOC, vec2(32.0, 32.0)).expect("somewhere to go");
        assert_eq!(placed.rect, rect(0, 22, 64, 20));
        assert_eq!(placed.pixels.len() as u64, placed.rect.area() * 4);
        // The cropped columns come off both sides evenly: (100 - 64) / 2 = 18.
        // Asserted through a clip whose first column is distinguishable.
        let mut striped: Vec<u8> = clip.pixels().to_vec();
        for row in 0..20 {
            let i = (row * 100) * 4;
            striped[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
        let striped = Clip::from_rgba(100, 20, striped).expect("a clip");
        let placed = striped.place(DOC, vec2(32.0, 32.0)).expect("somewhere");
        assert_eq!(
            &placed.pixels[0..4],
            &[0, 255, 0, 255],
            "the crop did not start 18 columns in"
        );
    }

    /// The complement rule again, through a **feathered** selection.
    ///
    /// A separate test rather than a second fixture inside the one above,
    /// because what it exercises is different in kind: an antialiased edge is a
    /// one-pixel band of partial coverage round an interior of 255, where a
    /// feather is almost nothing *but* partial coverage — so `min(alpha,
    /// coverage)` and the subtraction that gives the other half are being asked
    /// for every share rather than for the two ends and a sliver. If a feather
    /// could break `taken + left == before` it would break it here.
    #[test]
    fn a_cut_through_a_feathered_selection_still_takes_what_it_leaves() {
        const N: u32 = 24;
        let doc = UVec2::splat(N);
        let selection = Selection::from_rings(
            vec![vec![vec2(6.0, 6.0), vec2(18.0, 8.0), vec2(9.0, 18.0)]],
            doc,
        )
        .expect("a selection")
        .feathered(3.0, doc)
        .expect("a soft selection");

        let layer: Vec<u8> = (0..N * N)
            .flat_map(|i| {
                let a = (i * 255 / (N * N - 1)) as u8;
                srgb::encode_pixel([200, 90, 30, a])
            })
            .collect();
        let r = rect(0, 0, N, N);
        let cut = Clip::cut_from_layer(r, &layer, Some(&selection)).expect("a cut");

        let mut partials = 0;
        for i in (0..layer.len()).step_by(4) {
            let before = u32::from(layer[i + 3]);
            let taken = u32::from(cut.clip.pixels()[i + 3]);
            let left = u32::from(cut.remainder[i + 3]);
            assert_eq!(
                taken + left,
                before,
                "pixel {} lost or gained coverage",
                i / 4
            );
            if taken > 0 && left > 0 {
                partials += 1;
            }
        }
        // A feather has to produce *many* of these, not the handful an
        // antialiased edge does — otherwise this is the same test again.
        assert!(
            partials > 40,
            "only {partials} pixels were partly cut, so the feather is not \
             being exercised"
        );

        // And a copy takes the identical share, which is the guarantee that
        // makes `cut_from_layer` and `from_layer` one function.
        let copied = Clip::from_layer(r, &layer, Some(&selection)).expect("a clip");
        assert_eq!(cut.clip, copied);
    }

    /// An odd-sized clip must not creep. Truncating rather than rounding moves
    /// it half a pixel every time it is pasted, which is visible after three.
    #[test]
    fn an_odd_sized_paste_does_not_drift() {
        let clip = solid(9, 9, [255, 0, 0, 255]);
        let a = clip.place(DOC, vec2(32.0, 32.0)).expect("somewhere");
        let centre = a.rect.x as f32 + 4.5;
        assert!((centre - 32.0).abs() <= 0.5, "landed at {centre}");
    }
}
