//! What a layer's thumbnail shows.
//!
//! The list used to draw a checkerboard chip on every row, which said nothing
//! about the layer beyond "there is one". A thumbnail of the *whole canvas*
//! shrunk into 24 points is barely better: a sketch on a 2048² canvas becomes
//! four grey pixels, and eight layers of it are eight identical chips.
//!
//! So a thumbnail shows the layer's **content** — the bounding box of whatever
//! is not transparent, scaled to fill the frame, with a margin so the mark does
//! not touch the edges. Two layers holding the same mark in different corners
//! then draw the same picture, which is the point: what the row has to answer is
//! "which layer is the tree on", not "where on the canvas is it".
//!
//! # Why it takes two passes
//!
//! Finding that bounding box means knowing where the layer's alpha is, and only
//! the GPU knows. The route is therefore:
//!
//! 1. reduce the whole slice to a [`SIZE`]-square grid of the **greatest** alpha
//!    in each cell, and read that back — 16 KB, no stall;
//! 2. [`content_rect`] turns those cells into a document rectangle, and
//!    [`framed`] turns that into the region to draw;
//! 3. reduce *that* region to the same grid, this time as a mean, and read it
//!    back as the picture.
//!
//! The first reduction is a **maximum and not a mean**, and that is the one
//! thing here that is easy to get wrong. A pencil line one pixel wide averaged
//! over a 32×32 cell is an alpha of 1/1024, which rounds to zero in the eight
//! bits the readback carries — so a mean would report a sketched layer as empty
//! and draw nothing at all. A maximum reports the cell as covered by exactly the
//! texel that covers it. The *second* reduction is a mean, because by then the
//! region is the content's own and averaging is what downscaling a picture is.
//!
//! Nothing here needs a device, which is why the arithmetic lives in this crate
//! rather than beside the shader: what a thumbnail *shows* is a rule, and rules
//! are testable without a window.

use crate::geom::{PixelRect, Rect};
use glam::{UVec2, Vec2};

/// The side of both the bounds grid and the picture, in texels.
///
/// One number rather than two because the two passes share a render target and
/// a staging buffer, and a second size would be a second buffer for no gain.
/// 64 is comfortably above the 24 points the layer list draws at 2× — a chip
/// that is upscaled looks soft, and a thumbnail nobody can read is the state
/// this replaces — while a row of 64 RGBA texels is exactly the 256-byte copy
/// alignment, so the readback has no padding to stride over.
pub const SIZE: u32 = 64;

/// Fraction of the frame left empty on each side, so the content does not run
/// into the chip's own edge.
pub const PADDING: f32 = 0.08;

/// The document rectangle covered by whatever cells of a bounds pass carry any
/// alpha at all, or `None` where the layer is empty.
///
/// `rgba` is the readback of the bounds pass: `row_bytes` per row, `grid` cells
/// square, alpha in the fourth byte of each cell. `doc` is the canvas the grid
/// was reduced from.
///
/// **Any alpha counts.** The pass reduces by maximum, so a cell reads non-zero
/// exactly when some texel under it does — there is no threshold to pick and
/// therefore no faint layer to lose. The rectangle is the *union of the cells*,
/// so it is a superset of the true content bounds by at most one cell on each
/// side; that is a margin the frame was going to add anyway.
pub fn content_rect(rgba: &[u8], row_bytes: usize, grid: UVec2, doc: UVec2) -> Option<PixelRect> {
    if grid.x == 0 || grid.y == 0 || doc.x == 0 || doc.y == 0 {
        return None;
    }
    let (mut min, mut max) = (UVec2::new(u32::MAX, u32::MAX), UVec2::ZERO);
    let mut found = false;
    for j in 0..grid.y {
        let row = j as usize * row_bytes;
        for i in 0..grid.x {
            let Some(alpha) = rgba.get(row + i as usize * 4 + 3) else {
                continue;
            };
            if *alpha == 0 {
                continue;
            }
            found = true;
            min = min.min(UVec2::new(i, j));
            max = max.max(UVec2::new(i, j));
        }
    }
    if !found {
        return None;
    }

    // Cell `i` covers `i * doc / grid ..= (i + 1) * doc / grid`, rounded
    // outwards so the rectangle covers every pixel the cell could have seen.
    // In `u64`, because a 10000² canvas times a 64 grid overflows `u32`.
    let span = |lo: u32, hi: u32, cells: u32, size: u32| -> (u32, u32) {
        let cells = u64::from(cells);
        let size = u64::from(size);
        let a = (u64::from(lo) * size / cells) as u32;
        let b = ((u64::from(hi) + 1) * size).div_ceil(cells).min(size) as u32;
        (a, b.max(a + 1).min(size as u32))
    };
    let (x0, x1) = span(min.x, max.x, grid.x, doc.x);
    let (y0, y1) = span(min.y, max.y, grid.y, doc.y);
    Some(PixelRect {
        x: x0,
        y: y0,
        width: x1 - x0,
        height: y1 - y0,
    })
}

/// The document region to draw, so that `content` fills a `thumb`-shaped frame
/// with [`PADDING`] to spare.
///
/// Deliberately **not clamped to the canvas**. A mark in the corner has to sit
/// in the middle of its own thumbnail like any other, so the region runs off
/// the edge and the pass reads nothing there — which is transparent, which is
/// what the margin should look like. Clamping instead would shove the mark into
/// a corner of the chip and squash the aspect.
///
/// The scale is capped at 1:1: a single dab is drawn as a single dab and not
/// magnified into a square filling the row. Beyond that cap the mark simply
/// looks small, which is the truth about it.
pub fn framed(content: PixelRect, thumb: UVec2) -> Rect {
    let thumb = Vec2::new(thumb.x.max(1) as f32, thumb.y.max(1) as f32);
    let size = Vec2::new(content.width.max(1) as f32, content.height.max(1) as f32);

    // The region has the frame's aspect, so nothing is stretched: whichever
    // axis of the content is proportionally longer decides the scale.
    let fill = (size / thumb).max_element() / (1.0 - 2.0 * PADDING).max(1e-3);
    // One document pixel per thumbnail texel is as far in as this goes.
    let region = thumb * fill.max(1.0);

    let centre = Vec2::new(
        content.x as f32 + size.x * 0.5,
        content.y as f32 + size.y * 0.5,
    );
    Rect::new(centre - region * 0.5, centre + region * 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid with `cells` set, as the bounds pass would hand it over.
    fn grid(size: u32, cells: &[(u32, u32)]) -> Vec<u8> {
        let mut out = vec![0u8; (size * size * 4) as usize];
        for (i, j) in cells {
            out[((j * size + i) * 4 + 3) as usize] = 255;
        }
        out
    }

    #[test]
    fn an_empty_layer_has_no_content() {
        let cells = grid(8, &[]);
        assert_eq!(
            content_rect(&cells, 32, UVec2::splat(8), UVec2::splat(256)),
            None
        );
    }

    /// The rectangle covers the cells that answered, mapped back to the canvas
    /// they were reduced from.
    #[test]
    fn the_content_rect_covers_the_cells_that_answered() {
        let cells = grid(8, &[(2, 3), (5, 3)]);
        let rect = content_rect(&cells, 32, UVec2::splat(8), UVec2::splat(256)).unwrap();
        // Cells are 32 canvas pixels square: x spans cells 2..=5, y cell 3.
        assert_eq!(rect.x, 64);
        assert_eq!(rect.width, 4 * 32);
        assert_eq!(rect.y, 96);
        assert_eq!(rect.height, 32);
    }

    /// One texel of alpha anywhere is content. The bounds pass reduces by
    /// *maximum* precisely so that this holds — a mean over a 32×32 cell would
    /// round a one-pixel line to zero and report a sketch as an empty layer.
    #[test]
    fn a_single_faint_cell_is_still_content() {
        let mut cells = grid(8, &[]);
        cells[(4 * 8 + 4) * 4 + 3] = 1;
        let rect = content_rect(&cells, 32, UVec2::splat(8), UVec2::splat(256)).unwrap();
        assert_eq!(rect.width, 32);
        assert_eq!(rect.height, 32);
    }

    /// A canvas that does not divide evenly by the grid still yields a
    /// rectangle inside it, and never a zero-area one.
    #[test]
    fn the_content_rect_stays_inside_an_awkward_canvas() {
        let cells = grid(8, &[(7, 7)]);
        let doc = UVec2::new(1001, 37);
        let rect = content_rect(&cells, 32, UVec2::splat(8), doc).unwrap();
        assert!(rect.x + rect.width <= doc.x);
        assert!(rect.y + rect.height <= doc.y);
        assert!(rect.width > 0 && rect.height > 0);
    }

    /// The whole point: the content fills the frame on its longer axis, with a
    /// margin, and is centred on both.
    #[test]
    fn content_fills_the_frame_and_is_centred_in_it() {
        let content = PixelRect {
            x: 100,
            y: 400,
            width: 400,
            height: 200,
        };
        let frame = framed(content, UVec2::splat(SIZE));
        let size = frame.max - frame.min;
        assert!((size.x - size.y).abs() < 1e-3, "the frame is square");

        // The long axis fills all but the padding.
        let filled = content.width as f32 / size.x;
        assert!(
            (filled - (1.0 - 2.0 * PADDING)).abs() < 1e-3,
            "content filled {filled} of the frame"
        );

        let centre = (frame.min + frame.max) * 0.5;
        assert!((centre.x - 300.0).abs() < 1e-3);
        assert!((centre.y - 500.0).abs() < 1e-3);
    }

    /// A mark against the canvas edge is centred like any other, so the region
    /// leaves the canvas — which reads as transparent and is exactly the margin
    /// every other thumbnail has.
    #[test]
    fn a_mark_in_the_corner_is_still_centred() {
        let content = PixelRect {
            x: 0,
            y: 0,
            width: 300,
            height: 300,
        };
        let frame = framed(content, UVec2::splat(SIZE));
        assert!(frame.min.x < 0.0 && frame.min.y < 0.0, "{frame:?}");
        let centre = (frame.min + frame.max) * 0.5;
        assert!((centre.x - 150.0).abs() < 1e-3);
        assert!((centre.y - 150.0).abs() < 1e-3);
    }

    /// A single dab is a single dab. Magnifying it to fill the row would say
    /// the layer holds a large square, which is the one thing it does not.
    #[test]
    fn a_tiny_mark_is_never_magnified() {
        let content = PixelRect {
            x: 500,
            y: 500,
            width: 1,
            height: 1,
        };
        let frame = framed(content, UVec2::splat(SIZE));
        let size = frame.max - frame.min;
        assert!(
            (size.x - SIZE as f32).abs() < 1e-3,
            "one document pixel per texel is the limit, got {size:?}"
        );
    }

    /// A frame that is not square keeps its own proportions rather than
    /// squashing the picture into them.
    #[test]
    fn a_wide_frame_stays_wide() {
        let content = PixelRect {
            x: 0,
            y: 0,
            width: 100,
            height: 100,
        };
        let frame = framed(content, UVec2::new(128, 64));
        let size = frame.max - frame.min;
        assert!((size.x / size.y - 2.0).abs() < 1e-3, "{size:?}");
        // The square content is bounded by the frame's *short* axis.
        assert!((100.0 / size.y - (1.0 - 2.0 * PADDING)).abs() < 1e-3);
    }
}
