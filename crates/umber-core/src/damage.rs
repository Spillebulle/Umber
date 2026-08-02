//! Which parts of the canvas a stroke actually touched.
//!
//! The undo history records the pixels a stroke replaced, and the obvious
//! description of "where" is the stroke's bounding box. A box is a very poor
//! description of a diagonal: a thin line drawn corner to corner of a 10000²
//! canvas touches a few million pixels and reserves **four hundred megabytes**
//! to record them, which is one undo step for the entire document.
//!
//! So damage is accumulated on a grid of [`TILE`]-pixel cells as well as into a
//! box, and the history keeps only the cells a dab actually reached. The box is
//! still wanted — it is what the commit pass's quad spans, and what a patch
//! reports as the region it belongs to — but it is no longer what gets stored.
//!
//! Two properties make this safe to swap in wholesale:
//!
//! - **Every cell is clipped to the box**, so the pixels kept are always a
//!   *subset* of what the bounding box held. A small scribble whose box is
//!   smaller than one cell comes out exactly as it used to; nothing gets worse.
//! - **Adjacent cells in a row are merged into one piece**, but cells are never
//!   merged *down* the canvas. That keeps a piece to at most one row of cells —
//!   8 MB at the largest canvas Umber will make — so a piece always fits the
//!   staging buffer a readback is allowed to allocate, and the banding in
//!   `read_texture_rows` never has to reach inside one.
//!
//! The cells a patch holds are also the cells the commit pass is scissored to,
//! which is what makes "undo restores every pixel the stroke changed" a
//! structural fact rather than a hope about blend rounding. See
//! `CanvasRenderer::commit_stroke`.

use glam::Vec2;

use crate::geom::PixelRect;

/// The side of one damage cell, in document pixels.
///
/// The trade is between following a stroke's shape closely, which wants small
/// cells, and the per-piece cost of a rectangle that has to be copied off the
/// GPU, held, and written into a file, which wants large ones. Measured with
/// `examples/measure-undo.rs`, one stroke's patch:
///
/// | | box | 64 | 128 | 256 |
/// |---|---|---|---|---|
/// | thin diagonal, 10000² | 381.5 MB | **6.8 MB** | 11.0 MB | 19.8 MB |
/// | thin diagonal, 2048² | 16.0 MB | **1.3 MB** | 2.1 MB | 3.8 MB |
/// | small scribble | 0.4 MB | **0.2 MB** | 0.4 MB | 0.4 MB |
/// | broad wash, 10000² | 381.5 MB | 381.5 MB | 381.5 MB | 381.5 MB |
///
/// 64 is the best of them everywhere and its cost is a piece count that stays
/// small anyway, because neighbours in a row merge: 157 pieces for a stroke
/// across the largest canvas Umber makes, against 79 at 128. A wash is the
/// case no cell size helps — it really did touch every pixel — and the row it
/// is in is why this is a large improvement rather than an unbounded one.
///
/// A multiple of 64, so a full cell's rows are exactly the 256 bytes a texture
/// copy has to be padded to, and a full cell needs no padding at all.
pub const TILE: u32 = 64;

/// The largest cell index the grid will name, from
/// [`Document::MAX_EDGE`](crate::document::Document::MAX_EDGE).
///
/// A dab may be centred far off the canvas — the pointer is not confined to it
/// — and turning a wild coordinate into a cell range is what would otherwise
/// loop for a very long time indeed.
fn cell_cap(tile: u32) -> u32 {
    crate::document::Document::MAX_EDGE / tile
}

/// How many cell marks accumulate before the list is first sorted and
/// deduplicated.
///
/// Only a bound on the transient: the same cells are pushed over and over as a
/// stroke works within one, and the "same as the last dab" check below catches
/// most of that. This catches a stroke that scrubs back and forth between two
/// of them for a minute.
///
/// The threshold then **doubles past whatever survived**, which is the whole
/// of what keeps this off the drawing path's bill. A fixed threshold is a
/// sorting pass per dab once a broad stroke has genuinely reached that many
/// cells — measured, 3.5 seconds of marking for one wash across a 10000²
/// canvas, all of it while the artist was still holding the pointer down.
const COMPACT_AT: usize = 4096;

/// The cells of the canvas a stroke has reached, accumulated one dab at a time.
///
/// Nothing here allocates per dab in the ordinary case: a dab lands in the same
/// cells as the one before it, which is a four-integer comparison and no push
/// at all.
#[derive(Clone, Debug)]
pub struct TileMask {
    tile: u32,
    /// Cells as `(y << 16) | x`, in the order they were first reached. Sorted
    /// only when read — the drawing path must not pay for order it never uses.
    cells: Vec<u32>,
    /// The cell range the previous dab marked, as `[x0, y0, x1, y1]` inclusive.
    last: Option<[u32; 4]>,
    /// Length at which to compact next. See [`COMPACT_AT`].
    compact_at: usize,
}

impl Default for TileMask {
    fn default() -> Self {
        Self::new(TILE)
    }
}

impl TileMask {
    /// A mask on a grid of `tile`-pixel cells. The engine uses [`TILE`];
    /// the argument exists so the measuring example can sweep it.
    pub fn new(tile: u32) -> Self {
        Self {
            tile: tile.max(1),
            cells: Vec::new(),
            last: None,
            compact_at: COMPACT_AT,
        }
    }

    pub fn tile(&self) -> u32 {
        self.tile
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn clear(&mut self) {
        self.cells.clear();
        self.last = None;
        self.compact_at = COMPACT_AT;
    }

    /// Mark the cells an axis-aligned box — a dab's quad — reaches.
    ///
    /// Rounded outwards exactly as [`Rect::to_pixels_clamped`](crate::geom::Rect::to_pixels_clamped)
    /// rounds the bounding box, so the cells cover every pixel the box does.
    /// Marking a cell the stroke turns out not to have touched costs a little
    /// memory; missing one loses pixels an undo was supposed to restore, and
    /// the rounding here is the whole of the difference.
    pub fn mark(&mut self, centre: Vec2, half: Vec2) {
        let cap = cell_cap(self.tile);
        // `as u32` saturates at both ends in Rust, which is what makes a dab
        // hundreds of thousands of pixels off the canvas cost nothing.
        let lo = |v: f32| (v.floor().max(0.0) as u32 / self.tile).min(cap);
        // The damaged range ends *before* the ceiling, so the last cell is the
        // one holding that pixel — and a box that ends at or before zero has
        // nothing in it but is still named cell zero, which the clip to the
        // stroke's rect then throws away.
        let hi = |v: f32| ((v.ceil().max(0.0) as u32).saturating_sub(1) / self.tile).min(cap);

        let x0 = lo(centre.x - half.x);
        let y0 = lo(centre.y - half.y);
        let x1 = hi(centre.x + half.x).max(x0);
        let y1 = hi(centre.y + half.y).max(y0);

        // The common case by a wide margin: a stroke lays dabs a fraction of a
        // radius apart, so most of them land inside the cells the last one
        // already named.
        if let Some([lx0, ly0, lx1, ly1]) = self.last
            && x0 >= lx0
            && y0 >= ly0
            && x1 <= lx1
            && y1 <= ly1
        {
            return;
        }
        self.last = Some([x0, y0, x1, y1]);

        for y in y0..=y1 {
            for x in x0..=x1 {
                self.cells.push((y << 16) | x);
            }
        }
        if self.cells.len() >= self.compact_at {
            self.cells.sort_unstable();
            self.cells.dedup();
            self.compact_at = (self.cells.len() * 2).max(COMPACT_AT);
        }
    }

    /// The rectangles a patch is made of: the marked cells, clipped to `rect`,
    /// with neighbours in the same row merged.
    ///
    /// Row-major, non-overlapping, and every one of them inside `rect` — which
    /// is what makes the total no larger than the bounding box ever was.
    ///
    /// An empty mask answers with the whole of `rect`. That cannot arise from a
    /// stroke, which marks a cell for every dab it emits, but a patch built
    /// from a file or by a test has no mask at all and must still describe
    /// itself.
    pub fn pieces(&self, rect: PixelRect) -> Vec<PixelRect> {
        if self.cells.is_empty() {
            return vec![rect];
        }
        let mut cells = self.cells.clone();
        cells.sort_unstable();
        cells.dedup();

        let mut out: Vec<PixelRect> = Vec::new();
        let mut run: Option<(u32, u32, u32)> = None; // (y, first x, last x)
        for id in cells {
            let (x, y) = (id & 0xFFFF, id >> 16);
            match run {
                Some((ry, first, last)) if ry == y && x == last + 1 => {
                    run = Some((ry, first, x));
                }
                Some((ry, first, last)) => {
                    push_run(&mut out, self.tile, ry, first, last, rect);
                    run = Some((y, x, x));
                }
                None => run = Some((y, x, x)),
            }
        }
        if let Some((ry, first, last)) = run {
            push_run(&mut out, self.tile, ry, first, last, rect);
        }
        // Only reachable if every marked cell fell outside the clamped rect —
        // a stroke made entirely of dabs off the edge of the canvas. The rect
        // is still the region the commit spans, so it is what the patch has to
        // describe.
        if out.is_empty() {
            out.push(rect);
        }
        out
    }
}

/// Turn one run of cells in row `y` into a rectangle clipped to `rect`.
fn push_run(out: &mut Vec<PixelRect>, tile: u32, y: u32, first: u32, last: u32, rect: PixelRect) {
    let cell = PixelRect {
        x: first.saturating_mul(tile),
        y: y.saturating_mul(tile),
        width: (last - first + 1).saturating_mul(tile),
        height: tile,
    };
    if let Some(clipped) = intersect(cell, rect) {
        out.push(clipped);
    }
}

/// The overlap of two rectangles, or `None` where they do not meet.
pub fn intersect(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    // `then`, not `then_some`: the arms of the latter are evaluated whatever
    // the condition, and these two subtractions underflow precisely when the
    // rectangles do not meet.
    (x1 > x0 && y1 > y0).then(|| PixelRect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::vec2;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    /// The whole point: a diagonal stroke stores the cells along it, not the
    /// square it spans. Anything else is the bug this module exists to fix.
    #[test]
    fn a_diagonal_keeps_the_cells_it_crosses_and_not_the_box() {
        let mut mask = TileMask::new(64);
        for i in 0..16 {
            let p = i as f32 * 64.0 + 32.0;
            mask.mark(vec2(p, p), Vec2::splat(4.0));
        }
        let bounds = rect(0, 0, 1024, 1024);
        let pieces = mask.pieces(bounds);

        let kept: u64 = pieces.iter().map(PixelRect::area).sum();
        assert!(
            kept * 8 < bounds.area(),
            "a diagonal kept {kept} of {} pixels",
            bounds.area()
        );
        // And every dab is inside one of them.
        for i in 0..16 {
            let p = i as f32 * 64.0 + 32.0;
            assert!(
                pieces
                    .iter()
                    .any(|r| r.x as f32 <= p && p < (r.x + r.width) as f32),
                "the dab at {p} is in no piece"
            );
        }
    }

    /// A run of neighbours is one rectangle, because a hundred adjacent
    /// rectangles are a hundred texture copies to read exactly the same pixels.
    #[test]
    fn neighbours_in_a_row_merge_into_one_piece() {
        let mut mask = TileMask::new(64);
        for i in 0..8 {
            mask.mark(vec2(i as f32 * 64.0 + 32.0, 32.0), Vec2::splat(8.0));
        }
        let pieces = mask.pieces(rect(0, 0, 1024, 1024));
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0], rect(0, 0, 512, 64));
    }

    /// Cells never merge downwards, so one piece is at most a row of them. That
    /// is what bounds a single readback, whatever the canvas.
    #[test]
    fn a_piece_is_never_taller_than_one_cell() {
        let mut mask = TileMask::new(64);
        for y in 0..8 {
            for x in 0..8 {
                mask.mark(vec2(x as f32 * 64.0, y as f32 * 64.0), Vec2::splat(30.0));
            }
        }
        let pieces = mask.pieces(rect(0, 0, 1024, 1024));
        assert!(!pieces.is_empty());
        for p in &pieces {
            assert!(p.height <= 64, "a piece is {} tall", p.height);
        }
    }

    /// Clipping to the stroke's own rect is what stops a small mark costing
    /// more than it used to. The mark below straddles a cell boundary, so it
    /// comes back as two pieces — but between them they are the box and not one
    /// pixel more, which is the property that matters: **tiles can never make a
    /// patch bigger than the bounding box it replaced.**
    #[test]
    fn a_small_mark_never_costs_more_than_its_box() {
        for centre in [(300.0, 200.0), (300.0, 220.0), (32.0, 32.0)] {
            let mut mask = TileMask::default();
            mask.mark(vec2(centre.0, centre.1), Vec2::splat(10.0));
            let bounds = rect(centre.0 as u32 - 10, centre.1 as u32 - 10, 20, 20);
            let pieces = mask.pieces(bounds);
            let kept: u64 = pieces.iter().map(PixelRect::area).sum();
            assert_eq!(kept, bounds.area(), "{centre:?} -> {pieces:?}");
            for p in &pieces {
                assert!(
                    intersect(*p, bounds) == Some(*p),
                    "{p:?} escapes {bounds:?}"
                );
            }
        }
    }

    /// A dab off the edge of the canvas must not be able to name a cell way
    /// outside it, and must not cost a loop over the whole grid to say so.
    #[test]
    fn a_dab_off_the_canvas_marks_nothing_that_survives_the_clip() {
        let mut mask = TileMask::default();
        mask.mark(vec2(-1e9, -1e9), Vec2::splat(4.0));
        mask.mark(vec2(1e9, 1e9), Vec2::splat(4.0));
        let bounds = rect(512, 512, 128, 128);
        // Both marks are outside the rect, so the clip leaves nothing and the
        // rect itself stands in.
        assert_eq!(mask.pieces(bounds), vec![bounds]);
    }

    /// The pieces have to cover every pixel the box does when a stroke really
    /// did cover it — a wash must not come back with holes in it.
    #[test]
    fn a_stroke_covering_everything_keeps_everything() {
        let mut mask = TileMask::new(64);
        for y in 0..4 {
            for x in 0..4 {
                mask.mark(
                    vec2(x as f32 * 64.0 + 32.0, y as f32 * 64.0 + 32.0),
                    Vec2::splat(32.0),
                );
            }
        }
        let bounds = rect(0, 0, 256, 256);
        let kept: u64 = mask.pieces(bounds).iter().map(PixelRect::area).sum();
        assert_eq!(kept, bounds.area());
    }

    /// The pieces must not overlap, or a patch would hold the same pixel twice
    /// and the budget would be counting bytes nobody needs.
    #[test]
    fn pieces_never_overlap() {
        let mut mask = TileMask::new(64);
        let mut rng = 12345u64;
        for _ in 0..200 {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let x = (rng % 1000) as f32;
            let y = ((rng >> 20) % 1000) as f32;
            mask.mark(vec2(x, y), Vec2::splat(20.0));
        }
        let pieces = mask.pieces(rect(0, 0, 1024, 1024));
        for (i, a) in pieces.iter().enumerate() {
            for b in &pieces[i + 1..] {
                assert!(intersect(*a, *b).is_none(), "{a:?} overlaps {b:?}");
            }
        }
    }

    /// Marking is not allowed to grow without bound when a stroke scrubs back
    /// and forth over the same few cells for a long time.
    #[test]
    fn scrubbing_over_the_same_cells_does_not_grow_forever() {
        let mut mask = TileMask::default();
        for i in 0..100_000 {
            let x = if i % 2 == 0 { 10.0 } else { 200.0 };
            mask.mark(vec2(x, 10.0), Vec2::splat(4.0));
        }
        assert!(mask.cells.len() < COMPACT_AT * 2, "{}", mask.cells.len());
        // Two cells, far enough apart not to merge, whatever [`TILE`] is.
        assert_eq!(mask.pieces(rect(0, 0, 1024, 1024)).len(), 2);
    }

    /// The other end of the same rule: a broad stroke over a very large canvas
    /// genuinely reaches tens of thousands of cells, and the list has to stay
    /// within a constant factor of them. Compacting at a fixed length instead
    /// keeps the length pinned there and pays a sort per dab to do it — which
    /// is a stroke that takes seconds to draw. The bound here is the memory
    /// half of that; the amortisation is the half it stands in for.
    #[test]
    fn a_broad_stroke_over_a_huge_canvas_stays_within_a_factor_of_its_cells() {
        let mut mask = TileMask::new(64);
        for y in 0..160 {
            for x in 0..160 {
                mask.mark(
                    vec2(x as f32 * 64.0, y as f32 * 64.0),
                    Vec2::splat(200.0), // a broad brush: 7 × 7 cells a dab
                );
            }
        }
        let distinct = {
            let mut c = mask.cells.clone();
            c.sort_unstable();
            c.dedup();
            c.len()
        };
        assert!(distinct > 20_000, "the fixture is not testing anything");
        assert!(
            mask.cells.len() < distinct * 3,
            "{} marks for {distinct} cells",
            mask.cells.len()
        );
    }
}
