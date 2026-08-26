//! The magnifier the eyedropper aims by: what it holds and where it goes.
//!
//! A model with no drawing in it, the division `dock.rs` keeps against
//! `panels.rs` and `gesture.rs` keeps against `app.rs`. `ui::loupe` paints
//! what [`place`] decides and what a [`Patch`] holds; everything here is
//! testable with no window, no device and no screen.
//!
//! # Why there is one at all now
//!
//! `syspick`'s first draft argued there could not be a loupe: a magnifier has
//! to be drawn *at* the pointer, the pointer during a drag is on somebody
//! else's window, so it means an always-on-top borderless window moved once per
//! event — occluding the pixel it magnifies or offset from it by a hand-tuned
//! margin, and either way a second wgpu surface and a second render pass.
//!
//! Every clause of that is true and the conclusion does not follow. **A
//! magnifier does not have to be at the pointer to be useful.** [`place`] keeps
//! it inside Umber's own view: beside the pointer while the pointer is in the
//! window, which is where a painter spends nearly all of a pick, and clamped
//! against the edge once the pointer has left. So it is an ordinary egui
//! overlay in a foreground layer — no window, no surface, no pass — and the
//! outside-the-window case gets a magnifier that stays put while the pointer
//! roams, which still says what a release would take.
//!
//! # The one thing it must never do
//!
//! Over Umber's own interface and over the desktop the neighbourhood is read
//! **off the screen**, and the loupe is itself on the screen. So the rule
//! [`place`] exists to hold is that the circle is never within [`CLEARANCE`] of
//! the pointer, and [`CELLS`] is odd and small enough that half of it in points
//! is under that figure at any sane scale — which is
//! `the_loupe_never_covers_the_pixels_it_reads`. That rule carries the colour
//! as well as the picture, because the colour a release takes is the block's
//! *middle* texel: the pointer's own pixel, which is the furthest of the
//! hundred and twenty-one from the circle.
//!
//! What that does not cover, and it is worth saying rather than discovering: a
//! pointer flicked **towards** the circle by more than the gap in a single
//! frame arrives where the previous frame's loupe was still painted on the
//! screen, and that frame's block holds a ghost of it. One frame, during a
//! flick, in the outer cells only — the middle texel is the pointer's own pixel
//! and the circle is never on that, so the colour is right on every frame.
//! Excluding our own window from a screen read is not something GDI offers
//! without a layered window, which is the design this module exists to avoid.

use glam::Vec2;
use umber_core::Color;

/// How many screen pixels across the magnified neighbourhood is.
///
/// **Odd, so there is a middle texel**, and that texel is the pixel a release
/// would take. An even figure would put the sample between two cells and the
/// mark on it would be a lie about which one.
///
/// Eleven rather than more: the loupe is [`RADIUS`] points across, so this is
/// about six points a cell, which is a grid somebody can count. It is also
/// what makes the read cheap enough to state plainly — one `BitBlt` of 121
/// pixels costs what one pixel costs, where 121 `GetPixel`s would be 121
/// display refreshes. See `syspick::sample_patch`.
///
/// **More is read than is shown, and that is the safe direction.** The grid is
/// square and the window is round, so the corners of the block, and at this
/// figure the outermost row and column, fall outside the circle and are never
/// drawn. The block is this wide because a `BitBlt` of it is free, not because
/// every texel reaches the screen — and a magnifier showing *fewer* pixels than
/// it read is nothing like one showing pixels it did not.
pub const CELLS: u32 = 11;

/// The magnified grid's radius, in points.
pub const RADIUS: f32 = 33.0;

/// The lens edge around the grid, in points.
///
/// Here rather than in `ui::loupe_overlay` where it is painted, because
/// [`place`] is handed [`OUTER`] and the guard has to measure the shape that is
/// actually drawn. A rim that lived at the call site would make the clearance
/// sweep nine points optimistic about a circle it had never seen — which is
/// the "measure the output, never restate the rule" failure this codebase
/// records at four other call sites.
///
/// **It was three, and what widened it is that the rim is now a surface rather
/// than a hairline.** `ui::loupe_glass` shades it from the light end of the
/// axis at the top left to the dark end at the bottom right, which is what
/// makes the thing read as a lens instead of as a disc with a border round it,
/// and three points of that is a line pretending to be a bevel. It also has to
/// be at least one [`CELL`] wide, because the grid is drawn a cell past
/// [`RADIUS`] and this band is what hides the overhang — see `ui::loupe_cells`.
pub const RIM: f32 = 9.0;

/// One magnified texel, in points.
///
/// Named because three things need it and only one of them is the drawing: the
/// grid steps by it, the clip that fills the disc is generous by exactly it,
/// and [`RIM`] has to be at least it. A figure recomputed at each of those is
/// three that have to agree.
pub const CELL: f32 = 2.0 * RADIUS / CELLS as f32;

/// The rim has to be able to hide a whole cell of overhang, or the picture
/// spills past the lens — which is what happened when the rim's radius was
/// handed to `circle_stroke` as a mid-radius. A `const` assert rather than a
/// sentence, for `effect::BUDGET_DERIVATION`'s reason: the failure is silent
/// and directional, and only one of the two figures is likely to be edited.
const _: () = assert!(RIM >= CELL, "the rim must hide a cell of overhang");

/// What the loupe occupies: the grid plus its rim.
pub const OUTER: f32 = RADIUS + RIM;

/// The gap between the pointer and the nearest point of the circle, in points.
///
/// It is a clearance rather than a taste: the screen read is a [`CELLS`]-wide
/// block centred on the pointer, so anything closer than half of that would be
/// the loupe magnifying itself. Everything above that is legibility, and this
/// is the figure a hand can rest in without the circle sitting on the thing it
/// is aimed at.
pub const CLEARANCE: f32 = 22.0;

/// A rectangle to keep the loupe inside, in points.
///
/// Its own type rather than `egui::Rect` for the reason this module has no
/// drawing in it: the rule is testable without egui and the painter converts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub min: Vec2,
    pub max: Vec2,
}

impl View {
    /// Whether a circle of `radius` about `at` lies wholly inside.
    fn holds(&self, at: Vec2, radius: f32) -> bool {
        at.x - radius >= self.min.x
            && at.y - radius >= self.min.y
            && at.x + radius <= self.max.x
            && at.y + radius <= self.max.y
    }

    /// The nearest position a circle of `radius` may sit at, or `None` where
    /// the view is too small to hold one at all.
    fn nearest(&self, at: Vec2, radius: f32) -> Option<Vec2> {
        let (lo, hi) = (
            self.min + Vec2::splat(radius),
            self.max - Vec2::splat(radius),
        );
        (lo.x <= hi.x && lo.y <= hi.y).then(|| at.clamp(lo, hi))
    }
}

/// Where the circle's centre goes, in points, or `None` for a view too small.
///
/// **Above the pointer, then below, then to either side, then diagonally, then
/// wherever the view will take it.** The eight candidates are each exactly
/// `radius + CLEARANCE` from the pointer, so the invariant this exists for
/// holds by construction rather than by arithmetic; the fallback is the nearest
/// legal position, which is what the outside-the-window case reaches — the
/// pointer is then far outside the view and the clamp lands the circle against
/// the edge nearest it, still comfortably clear.
///
/// **The diagonals are not decoration**: the four straight ones all fail in a
/// window's corner, where one axis has no room for the offset and the other
/// none for the radius, and the clamp there lands *closer* than the reach and
/// is refused. That is the top-left of the menu bar, which is somewhere a pick
/// genuinely happens, and without them it drew nothing at all —
/// `a_corner_still_gets_a_loupe` is the guard.
///
/// The fallback is refused where it would put the circle *on* the pointer,
/// which is a view too small to hold both. That is `overlay::place_strip`'s
/// rule and for the same reason: a control drawn where it cannot work is worse
/// than one that is not drawn. Note the difference from `place_strip`, which
/// clamps and never refuses on this ground — a selection's strip carries
/// commands that would otherwise have no control at all, and a loupe carries
/// none.
pub fn place(pointer: Vec2, view: View, radius: f32) -> Option<Vec2> {
    let reach = radius + CLEARANCE;
    const DIAGONAL: f32 = std::f32::consts::FRAC_1_SQRT_2;
    for dir in [
        Vec2::new(0.0, -1.0),
        Vec2::new(0.0, 1.0),
        Vec2::new(-1.0, 0.0),
        Vec2::new(1.0, 0.0),
        Vec2::new(DIAGONAL, DIAGONAL),
        Vec2::new(-DIAGONAL, DIAGONAL),
        Vec2::new(DIAGONAL, -DIAGONAL),
        Vec2::new(-DIAGONAL, -DIAGONAL),
    ] {
        let at = pointer + dir * reach;
        if view.holds(at, radius) {
            return Some(at);
        }
    }
    let at = view.nearest(pointer, radius)?;
    (at.distance(pointer) >= reach).then_some(at)
}

/// A magnified neighbourhood: `size`² texels, row-major, top-left first.
///
/// A texel is `None` where there is nothing to show — off every monitor for a
/// screen read, off the canvas or fully transparent for a document one. That
/// is the same distinction `syspick::sample` makes with `CLR_INVALID` and
/// `pick_colour` makes with an alpha of zero, and it is what keeps the loupe
/// from drawing black over a place that has no pixels. **A loupe that showed a
/// neighbourhood it had not read would be a control that lies**, which is the
/// standard this whole codebase is held to, so the absence is carried rather
/// than filled in.
#[derive(Clone, Debug, PartialEq)]
pub struct Patch {
    size: u32,
    texels: Vec<Option<[u8; 3]>>,
}

impl Patch {
    /// Build one, refusing a run of texels that is not `size` squared.
    ///
    /// The length is checked here rather than trusted, because both producers
    /// are loops over a foreign buffer — a GDI bitmap and a GPU readback — and
    /// a short one would otherwise be a panic on the drawing path.
    pub fn new(size: u32, texels: Vec<Option<[u8; 3]>>) -> Option<Self> {
        let want = (size as usize).checked_mul(size as usize)?;
        (size > 0 && texels.len() == want).then_some(Self { size, texels })
    }

    /// From `CanvasRenderer::pick_patch`'s straight-alpha sRGB RGBA.
    ///
    /// Two things become `None`, and each was a bug the single-pixel read had
    /// already had to answer:
    ///
    /// * **An alpha of exactly zero**, which is "nothing there" rather than
    ///   black — `pick_colour`'s caller has always refused it, because taking
    ///   it would silently set the brush to black. Exactly zero and not a
    ///   fraction: a faint pixel *is* a colour and the picker will take it, so
    ///   hiding it would make the loupe disagree with what a release does.
    /// * **A texel outside the document**, where `first` is the document pixel
    ///   the top-left texel stands for and `doc` is the canvas size. The
    ///   composite is asked for a block that runs off the picture whenever the
    ///   pointer is near an edge, and what it hands back there is a property of
    ///   the sampler's addressing rather than of the document. `None` is right
    ///   whatever that turns out to be, which is the point of deciding it here.
    pub fn from_document(
        size: u32,
        rgba: &[u8],
        first: (i32, i32),
        doc: (i32, i32),
    ) -> Option<Self> {
        let want = (size as usize).checked_mul(size as usize)?;
        if rgba.len() < want * 4 {
            return None;
        }
        Self::new(
            size,
            rgba[..want * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let x = first.0 + (i % size as usize) as i32;
                    let y = first.1 + (i / size as usize) as i32;
                    let inside = x >= 0 && y >= 0 && x < doc.0 && y < doc.1;
                    (inside && c[3] != 0).then_some([c[0], c[1], c[2]])
                })
                .collect(),
        )
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// The texel at a column and a row, or `None` for nothing there — and for
    /// an index off the grid, which is the same answer and saves the painter a
    /// bounds test it would otherwise have to get right.
    pub fn at(&self, col: u32, row: u32) -> Option<[u8; 3]> {
        if col >= self.size || row >= self.size {
            return None;
        }
        self.texels[(row * self.size + col) as usize]
    }

    /// The middle column and row, which is the pixel a release takes.
    pub fn middle(&self) -> u32 {
        self.size / 2
    }
}

/// What the interface needs to draw a loupe this frame.
///
/// Held on `Editor` above the `--- documents ---` line, like `Editor::input`:
/// it describes where a pointer is and what is under it, which is a property of
/// the gesture and not of any document.
#[derive(Clone, Debug, PartialEq)]
pub struct Loupe {
    /// The pointer, in window physical pixels — `Editor::cursor`'s unit.
    pub at: Vec2,
    /// The colour a release would take, or `None` for nothing there.
    ///
    /// **It is the patch's middle texel wherever there is a patch**, and this
    /// field is still separate because it does not have to be. Both readers
    /// have a fallback the block cannot supply: off the screen a `BitBlt` that
    /// failed outright leaves `syspick::sample`'s single pixel, and on the
    /// canvas a block reaching no pixel at all is refused before the GPU is
    /// touched, so a `Loupe` can hold a colour with no picture and a picture
    /// with no colour. Deriving it at the painter would put that pair of rules
    /// in the drawing.
    ///
    /// An earlier draft carried it because the two genuinely came from
    /// different GDI calls, `GetPixel` for the colour and one `BitBlt` for the
    /// block. That measured at two display refreshes a frame; see
    /// `syspick::sample_patch` for why one is enough and what says so.
    pub taken: Option<Color>,
    /// The neighbourhood, or `None` where only the one pixel could be read.
    pub patch: Option<Patch>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIEW: View = View {
        min: Vec2::new(0.0, 0.0),
        max: Vec2::new(1280.0, 800.0),
    };

    #[test]
    fn it_sits_above_the_pointer_where_there_is_room() {
        let at = place(Vec2::new(640.0, 400.0), VIEW, OUTER).expect("room");
        assert_eq!(at, Vec2::new(640.0, 400.0 - OUTER - CLEARANCE));
    }

    #[test]
    fn it_goes_below_rather_than_being_squashed_against_the_top() {
        // The top of the window is exactly where a pick over the menu bar or
        // the tool options strip happens, so this is the ordinary case for the
        // interface half rather than an edge one.
        let pointer = Vec2::new(640.0, 8.0);
        let at = place(pointer, VIEW, OUTER).expect("room");
        assert_eq!(at, pointer + Vec2::new(0.0, OUTER + CLEARANCE));
    }

    #[test]
    fn a_corner_still_gets_a_loupe() {
        // **All four straight candidates fail in a window's corner.** Above and
        // to the left there is no room for the offset; below and to the right
        // the offset fits on one axis and the radius does not fit on the other.
        // The clamp then lands 38 points from the pointer against a reach of
        // 55, so it is refused — and this drew nothing at all until the
        // diagonals were added. The top-left of the menu bar is not an exotic
        // place to aim a picker.
        let pointer = Vec2::new(6.0, 6.0);
        let at = place(pointer, VIEW, OUTER).expect("a corner still gets one");
        assert!(
            at.x > pointer.x && at.y > pointer.y,
            "down and to the right"
        );
        assert!(
            (at.distance(pointer) - (OUTER + CLEARANCE)).abs() < 1e-3,
            "still exactly the reach away, which is what keeps the sweep below \
             true without a clamp"
        );
        // Every one of the four window corners, since each fails a different
        // pair of the straight candidates.
        for corner in [
            Vec2::new(0.0, 0.0),
            Vec2::new(1280.0, 0.0),
            Vec2::new(0.0, 800.0),
            Vec2::new(1280.0, 800.0),
        ] {
            assert!(place(corner, VIEW, OUTER).is_some(), "at {corner:?}");
        }
    }

    #[test]
    fn the_loupe_never_covers_the_pixels_it_reads() {
        // **The rule the whole module exists for.** Over the interface and over
        // the desktop the neighbourhood is a `CELLS`-wide block of the screen
        // centred on the pointer, and the loupe is drawn on that same screen.
        // So the circle must clear the pointer by more than half the block, at
        // every position and every scale.
        //
        // Swept rather than argued, and over positions *outside* the view as
        // well: that is the outside-the-window case, where the answer comes
        // from the clamp rather than from one of the eight candidates, and it
        // is the half the eight-candidate reasoning says nothing about.
        //
        // **And over small views, which is where this sweep was blind.**
        // Dropping the clearance test from the clamp — leaving `place` to
        // answer the nearest legal position however close that lands — walked
        // through an earlier version of this untouched, because a view 1280 by
        // 800 only ever reaches the clamp for a pointer far outside it, where
        // the distance is large for reasons that have nothing to do with the
        // check. The views that exercise it are the ones barely larger than the
        // circle: a window dragged short, or an interface scaled up. That is
        // CLAUDE.md's rule about a guard's inputs spanning the contract its
        // comment states, and it cost two minutes with the line deleted.
        let views = [
            VIEW,
            View {
                min: Vec2::ZERO,
                max: Vec2::splat(2.0 * OUTER + 6.0),
            },
            View {
                min: Vec2::ZERO,
                max: Vec2::new(2.0 * (OUTER + CLEARANCE) - 4.0, 300.0),
            },
            View {
                min: Vec2::new(-90.0, -40.0),
                max: Vec2::new(140.0, 96.0),
            },
        ];
        for ppp in [0.5f32, 0.75, 1.0, 1.5, 2.0, 3.0] {
            let half_block = (CELLS as f32 * 0.5) / ppp;
            for view in views {
                let (lo, hi) = (view.min - Vec2::splat(200.0), view.max + Vec2::splat(200.0));
                let mut x = lo.x;
                while x <= hi.x {
                    let mut y = lo.y;
                    while y <= hi.y {
                        let pointer = Vec2::new(x, y);
                        y += 7.0;
                        let Some(at) = place(pointer, view, OUTER) else {
                            continue;
                        };
                        let clear = at.distance(pointer) - OUTER;
                        assert!(
                            clear > half_block,
                            "at {pointer:?} in {view:?} at scale {ppp}: {clear}                              of clearance against a half-block of {half_block}"
                        );
                    }
                    x += 5.0;
                }
            }
        }
    }

    #[test]
    fn a_pointer_outside_the_window_keeps_the_loupe_inside_it() {
        // The desktop half: the pointer is on somebody else's window and the
        // loupe stays where it can be seen, against the edge nearest the hand.
        let at = place(Vec2::new(1600.0, 400.0), VIEW, OUTER).expect("room");
        assert_eq!(at, Vec2::new(1280.0 - OUTER, 400.0));
        let at = place(Vec2::new(-300.0, -300.0), VIEW, OUTER).expect("room");
        assert_eq!(at, Vec2::splat(OUTER));
    }

    #[test]
    fn a_view_too_small_draws_nothing_rather_than_something_wrong() {
        // Both refusals. A view that cannot hold the circle at all, and one
        // that can hold it only on top of the pointer — the second is the one
        // an ordinary clamp would have answered wrongly.
        let tiny = View {
            min: Vec2::ZERO,
            max: Vec2::splat(OUTER),
        };
        assert_eq!(place(Vec2::splat(10.0), tiny, OUTER), None);
        let snug = View {
            min: Vec2::ZERO,
            max: Vec2::splat(2.0 * OUTER + 4.0),
        };
        assert_eq!(place(Vec2::splat(OUTER + 2.0), snug, OUTER), None);
    }

    #[test]
    fn the_middle_texel_is_the_one_a_release_takes() {
        // `CELLS` is odd, so there is a middle; the mark the painter draws sits
        // on it, and `syspick::sample_patch` centres the block on the pixel
        // `syspick::sample` reads. If this ever became even the mark would be
        // beside the sample rather than on it.
        assert_eq!(CELLS % 2, 1);
        let patch = Patch::new(CELLS, vec![Some([1, 2, 3]); (CELLS * CELLS) as usize]).expect("ok");
        assert_eq!(patch.middle(), CELLS / 2);
        assert_eq!(patch.at(patch.middle(), patch.middle()), Some([1, 2, 3]));
    }

    #[test]
    fn a_patch_of_the_wrong_length_is_refused_rather_than_padded() {
        assert_eq!(Patch::new(3, vec![None; 8]), None);
        assert_eq!(Patch::new(3, vec![None; 10]), None);
        assert_eq!(Patch::new(0, vec![]), None);
        assert!(Patch::new(3, vec![None; 9]).is_some());
    }

    #[test]
    fn a_texel_off_the_grid_is_nothing_rather_than_a_panic() {
        let patch = Patch::new(2, vec![Some([9, 9, 9]); 4]).expect("ok");
        assert_eq!(patch.at(2, 0), None);
        assert_eq!(patch.at(0, 2), None);
        assert_eq!(patch.at(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn transparent_document_pixels_are_nothing_and_faint_ones_are_a_colour() {
        // The rule the single-pixel read already kept: an alpha of zero is
        // "there is nothing there", and anything above it is a colour the
        // picker will take. A threshold anywhere else would make the loupe
        // disagree with what a release does, which is the one thing it is for.
        let rgba = [
            10, 20, 30, 0, // nothing
            40, 50, 60, 1, // a colour, only just
            70, 80, 90, 255, //
            1, 2, 3, 128, //
        ];
        let patch = Patch::from_document(2, &rgba, (0, 0), (2, 2)).expect("ok");
        assert_eq!(patch.at(0, 0), None);
        assert_eq!(patch.at(1, 0), Some([40, 50, 60]));
        assert_eq!(patch.at(0, 1), Some([70, 80, 90]));
        assert_eq!(patch.at(1, 1), Some([1, 2, 3]));
    }

    #[test]
    fn texels_off_the_document_are_nothing_whatever_the_composite_returned() {
        // A pick at the very first pixel of the canvas: a 3-wide block centred
        // on it reaches one pixel left of and above the picture. What the
        // composite hands back out there is the sampler's addressing rather
        // than anything the document says, so every byte below is deliberately
        // opaque — a mask that read the alpha alone would pass this and still
        // magnify a smeared edge row.
        let rgba = [255u8; 3 * 3 * 4];
        let patch = Patch::from_document(3, &rgba, (-1, -1), (8, 8)).expect("ok");
        for row in 0..3 {
            assert_eq!(patch.at(0, row), None, "left column, row {row}");
            assert_eq!(patch.at(row, 0), None, "top row, column {row}");
        }
        assert_eq!(patch.at(1, 1), Some([255, 255, 255]));
        assert_eq!(patch.at(2, 2), Some([255, 255, 255]));

        // And the far corner, which is the bound the other way: a document 2
        // wide leaves the last column and row of a 3-block outside it.
        let patch = Patch::from_document(3, &rgba, (0, 0), (2, 2)).expect("ok");
        assert_eq!(patch.at(1, 1), Some([255, 255, 255]));
        assert_eq!(patch.at(2, 1), None);
        assert_eq!(patch.at(1, 2), None);
    }

    #[test]
    fn a_short_readback_is_refused_rather_than_read_off_the_end() {
        let big = (0, 0);
        let doc = (99, 99);
        assert_eq!(Patch::from_document(3, &[0u8; 35], big, doc), None);
        assert!(Patch::from_document(3, &[0u8; 36], big, doc).is_some());
        // A longer one is fine and the tail is ignored.
        assert!(Patch::from_document(3, &[0u8; 64], big, doc).is_some());
    }
}
