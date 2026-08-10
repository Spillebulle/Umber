//! How much of a layer a real document actually holds.
//!
//! Umber gives every layer a full canvas-sized `Rgba8UnormSrgb` slice, so a
//! layer costs 400 MB at 20000×5000 whether the artist painted a face on it or
//! one eyelash. `docs/perf/tiled-layer-storage.md` proposes paying per 256-square
//! tile instead, and both that design and `docs/perf/import-and-limits.md` name
//! the same unknown as the thing that decides whether it is worth building:
//! **what fraction of a real layer's tiles hold anything at all.**
//!
//! This answers it, for `.clip` documents, and the whole trick is that it
//! **decodes nothing**. Clip Studio already stores a layer as a grid of
//! 256-square blocks with a present/absent word on each — see
//! [`crate::csblocks`] — so occupancy is a property of the container's framing.
//! A survey of 1.8 GB of somebody's real work is therefore a few seconds of
//! record walking rather than the 12.3 GB of canvas buffers one
//! [`super::import`] of one such file costs. `examples/survey-residency.rs` is
//! the caller.
//!
//! # What the numbers do and do not say
//!
//! - **`stored` is an upper bound.** A block the file holds may still be
//!   entirely transparent — Clip Studio writes one where the artist touched the
//!   canvas, not where paint survived. Telling the two apart needs the inflate
//!   this module exists to avoid, so a real tiled store would keep *at most*
//!   this many tiles and possibly fewer. That direction is the safe one: it
//!   cannot make tiling look better than it is.
//! - **`covered` is what Umber would pay, and it is not `stored`.** A layer's
//!   bitmap is its own rectangle at its own offset, so its grid is not aligned
//!   with the canvas's — one stored block can straddle four canvas tiles, and a
//!   block hanging off the page costs nothing at all because the blit clips it.
//!   `covered` is the union of canvas tiles the stored blocks reach, which is
//!   the figure `covered × 256² × 4` bytes is a real answer to.
//! - **Only `.clip` is read**, and that is scope rather than an oversight. It
//!   is the format the 33 real documents this was written against are in, and
//!   its 256 block *is* the tile size the design proposes. `.kra` stores tiles
//!   too and could be measured the same way; `.ora` stores one trimmed PNG per
//!   layer, which answers a related but different question.
//!
//! Nothing here calls `check_bounds`, deliberately: the document that provoked
//! the design is one Umber currently **refuses**, 54 layers at 20000×5000, and
//! a survey that could not read the file that motivated it would be useless.

use glam::UVec2;

use super::{ImportError, clipstudio};
use crate::csblocks::BLOCK;

/// How far a residency walk will follow a layer chain.
///
/// Deliberately larger than [`crate::layer::LayerStack::MAX`]: this measures
/// documents Umber cannot open, and a stack Umber refuses is exactly the one
/// worth a figure. Still bounded, because the chain comes out of somebody
/// else's file.
pub const MAX_ENTRIES: usize = 4096;

/// One slice of one document: a layer's pixels, or a layer's mask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SliceResidency {
    pub layer: String,
    /// Whether this is the layer's mask rather than the layer itself.
    pub mask: bool,
    /// The layer's own bitmap, which is its rectangle and not the canvas.
    pub bitmap: UVec2,
    /// That bitmap's block grid, in blocks.
    pub grid: (usize, usize),
    /// Blocks the file actually holds. See the module docs: an upper bound.
    pub stored: usize,
    /// Canvas tiles those blocks reach, once placed and clipped to the page.
    /// This is what a tiled Umber would allocate for this slice.
    pub covered: usize,
    /// What an absent block holds, in [`crate::csblocks::Fill`]'s words.
    ///
    /// A mask's states all-ones, which a tiled store answers with a default
    /// rather than with allocated tiles; a raster layer's states nothing.
    /// `unknown` is an `Attribute` layout this reader could not locate the
    /// section in, and is read as empty everywhere else.
    pub fill: &'static str,
}

/// One document's worth, plus what could not be measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentResidency {
    pub size: UVec2,
    /// Entries in the stack, folders included.
    pub entries: usize,
    pub folders: usize,
    pub slices: Vec<SliceResidency>,
    /// Layers whose bitmap could not be measured, and why — a vector layer, a
    /// correction layer, a packing this reader will not guess at. Reported
    /// rather than passed over, because a layer missing from the sample is a
    /// layer missing from the occupancy figure.
    pub skipped: Vec<(String, String)>,
}

impl DocumentResidency {
    /// 256-tiles one canvas-sized slice is divided into.
    pub fn canvas_tiles(&self) -> usize {
        (self.size.x as usize).div_ceil(BLOCK) * (self.size.y as usize).div_ceil(BLOCK)
    }

    /// What Umber allocates today: one whole canvas per slice that has one.
    ///
    /// Counted over the slices that were *measured*, so it is the cost of
    /// exactly the pixels [`Self::tiled_bytes`] is the tiled cost of. A skipped
    /// layer is in neither, which is why `skipped` has to be read beside them.
    pub fn dense_bytes(&self) -> u64 {
        u64::from(self.size.x) * u64::from(self.size.y) * 4 * self.slices.len() as u64
    }

    /// What a tiled store would allocate for the same slices.
    pub fn tiled_bytes(&self) -> u64 {
        let per_tile = (BLOCK * BLOCK * 4) as u64;
        self.slices.iter().map(|s| s.covered as u64).sum::<u64>() * per_tile
    }

    /// Tiles held against tiles a dense store would allocate.
    ///
    /// `None` for a document with nothing measurable in it, rather than a zero
    /// that would read as "wonderfully sparse".
    pub fn occupancy(&self) -> Option<f64> {
        let canvas = self.canvas_tiles();
        if canvas == 0 || self.slices.is_empty() {
            return None;
        }
        let covered: usize = self.slices.iter().map(|s| s.covered).sum();
        Some(covered as f64 / (canvas * self.slices.len()) as f64)
    }
}

impl SliceResidency {
    /// This slice's own share of its canvas.
    pub fn occupancy(&self, canvas_tiles: usize) -> f64 {
        if canvas_tiles == 0 {
            return 0.0;
        }
        self.covered as f64 / canvas_tiles as f64
    }
}

/// Measure a Clip Studio document already in memory.
///
/// Named for the one format it reads rather than dispatching on an extension,
/// because there is nothing here to fall back to: a caller handed a `.psd` has
/// to be told that this question has not been answered for that format, and an
/// `UnsupportedExtension` out of a function called `survey` would read as a
/// file that could not be opened.
pub fn clip_studio(bytes: &[u8]) -> Result<DocumentResidency, ImportError> {
    clipstudio::residency(bytes)
}

/// Which canvas tiles one stored block reaches.
///
/// Split out and given a test of its own because it is the only arithmetic in
/// this module and every figure above rests on it. The block is 256 square in
/// the *bitmap*, whose right and bottom edges are padding the blit throws away;
/// the bitmap sits at `origin` in the document, which is generally not a
/// multiple of 256; and the canvas clips both ends. Get any of the three wrong
/// and the occupancy is quietly out.
///
/// The answer is a half-open tile range per axis, empty where the block lands
/// entirely off the page.
pub(super) fn tiles_touched(
    block: (usize, usize),
    bitmap: UVec2,
    origin: (i64, i64),
    canvas: UVec2,
) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
    let empty = (0..0, 0..0);
    let axis = |index: usize, extent: u32, at: i64, page: u32| -> Option<std::ops::Range<usize>> {
        // The block's own span inside the bitmap, with the padding past the
        // bitmap's edge taken off — that padding is not the picture and
        // `colour`'s blit does not copy it.
        let start = (index * BLOCK) as i64;
        let end = i64::from(extent).min(start + BLOCK as i64);
        if end <= start {
            return None;
        }
        // Placed, then clipped to the page.
        let (lo, hi) = ((at + start).max(0), (at + end).min(i64::from(page)));
        if hi <= lo {
            return None;
        }
        Some((lo as usize / BLOCK)..((hi as usize - 1) / BLOCK + 1))
    };
    match (
        axis(block.0, bitmap.x, origin.0, canvas.x),
        axis(block.1, bitmap.y, origin.1, canvas.y),
    ) {
        (Some(x), Some(y)) => (x, y),
        _ => empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(block: (usize, usize), bitmap: UVec2, origin: (i64, i64), canvas: UVec2) -> usize {
        let (x, y) = tiles_touched(block, bitmap, origin, canvas);
        x.len() * y.len()
    }

    /// A block aligned with the canvas grid is one tile; one offset by a pixel
    /// on both axes is four.
    ///
    /// This is the whole reason `covered` is not `stored`: a real Clip Studio
    /// layer sits wherever the artist dragged it, so the aligned case is the
    /// rare one and a survey reporting `stored` would understate what Umber
    /// pays. Both readings are asserted rather than the difference being
    /// described, because a rule restated inside its own test agrees with
    /// itself.
    #[test]
    fn an_unaligned_block_reaches_four_canvas_tiles_where_an_aligned_one_reaches_one() {
        let canvas = UVec2::new(1024, 1024);
        let bitmap = UVec2::new(512, 512);
        assert_eq!(count((0, 0), bitmap, (0, 0), canvas), 1);
        assert_eq!(count((1, 1), bitmap, (0, 0), canvas), 1);
        assert_eq!(count((0, 0), bitmap, (1, 1), canvas), 4);
        // One axis offset is two, which is what says the two axes are
        // independent rather than a square being scaled.
        assert_eq!(count((0, 0), bitmap, (1, 0), canvas), 2);
    }

    /// The page clips, and a block wholly off it costs nothing.
    ///
    /// A `.clip` layer routinely hangs past the canvas — the reader's own
    /// bitmap bound is stated in those terms — so a survey that charged for the
    /// overhang would report a document as *denser* than it is, which is the
    /// direction that argues against building the thing.
    #[test]
    fn a_block_off_the_page_is_charged_nothing_and_one_straddling_it_is_charged_the_part_on() {
        let canvas = UVec2::new(512, 512);
        let bitmap = UVec2::new(1024, 1024);
        // Entirely to the left, and entirely below.
        assert_eq!(count((0, 0), bitmap, (-256, 0), canvas), 0);
        assert_eq!(count((3, 3), bitmap, (0, 0), canvas), 0);
        // Straddling the left edge: the part on the page is the first column.
        let (x, y) = tiles_touched((1, 0), bitmap, (-300, 0), canvas);
        assert_eq!((x, y), (0..1, 0..1));
    }

    /// The bitmap's own edge is padding, not picture.
    ///
    /// A 300-wide bitmap is a two-column grid whose second column holds 44 real
    /// pixels, so that block reaches one tile and not two — the same clip
    /// `colour`'s `within` applies before it copies a byte.
    #[test]
    fn the_padding_past_a_bitmaps_edge_reaches_no_tile_of_its_own() {
        let canvas = UVec2::new(1024, 1024);
        let bitmap = UVec2::new(300, 300);
        let (x, y) = tiles_touched((1, 1), bitmap, (0, 0), canvas);
        assert_eq!((x, y), (1..2, 1..2));
        // Placed so the padding *would* cross a tile boundary if it counted:
        // the real pixels end at 300, so at an origin of 200 the block covers
        // 456..500 and stops inside the second tile.
        let (x, _) = tiles_touched((1, 0), bitmap, (200, 0), canvas);
        assert_eq!(x, 1..2);
    }
}
