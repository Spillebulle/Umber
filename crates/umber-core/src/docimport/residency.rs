//! How much of a layer a real document actually holds.
//!
//! Umber gives every layer a full canvas-sized `Rgba8UnormSrgb` slice, so a
//! layer costs 400 MB at 20000×5000 whether the artist painted a face on it or
//! one eyelash. `docs/perf/tiled-layer-storage.md` proposes paying per 256-square
//! tile instead, and both that design and `docs/perf/import-and-limits.md` name
//! the same unknown as the thing that decides whether it is worth building:
//! **what fraction of a real layer's tiles hold anything at all.**
//!
//! This answers it, for `.clip` documents, and it answers it **twice** — which
//! is the whole shape of the module.
//!
//! Clip Studio already stores a layer as a grid of 256-square blocks with a
//! present/absent word on each — see [`crate::csblocks`] — so a first reading
//! needs no decode at all: it is a walk of the container's own framing, a few
//! seconds over 1.8 GB of somebody's real work against the 12.3 GB of canvas
//! buffers one [`super::import`] of one such file costs.
//!
//! **That reading over-reports, and by how much is the thing that has to be
//! measured rather than assumed.** Clip Studio writes a block where the artist
//! *touched* the canvas, not where paint survived, so a stored block can be
//! entirely transparent and a tiled store would not back that tile at all. If
//! presence and content diverge by two, every figure here moves by two —
//! precisely where the decision is marginal. So [`Reading::Contents`] decodes
//! each block, asks whether one texel of its first plane differs from what an
//! absent block would hold, and throws the block away. That is bounded work
//! with a fixed footprint: **one 256-square block live at a time, and never a
//! canvas buffer**, which is the part of "do not decode" that actually mattered.
//!
//! # What the numbers do and do not say
//!
//! - **`stored` is blocks the file holds** — the cheap upper bound. `live` is
//!   the same count with the blank ones taken out, and is `None` under
//!   [`Reading::Presence`] rather than being quietly equal to `stored`.
//! - **`covered` is what Umber would pay, and it is not `stored`.** A layer's
//!   bitmap is its own rectangle at its own offset, so its grid is not aligned
//!   with the canvas's — one stored block can straddle four canvas tiles, and a
//!   block hanging off the page costs nothing at all because the blit clips it.
//!   `covered` is the union of canvas tiles the stored blocks reach, which is
//!   the figure `covered × 256² × 4` bytes is a real answer to. `live_covered`
//!   is the same union over the non-blank blocks alone, and is the honest
//!   answer to what a tiled store would allocate.
//! - **Blank is measured against the *fill*, not against zero.** An absent
//!   block of a raster layer is transparent and an absent block of a **mask**
//!   is all-ones, because a Clip Studio mask begins revealing everything — so a
//!   mask block of all-ones is exactly as redundant as a layer block of
//!   all-zeroes, and testing both against zero would report every full-reveal
//!   mask tile as live.
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

/// How hard a residency walk looks.
///
/// Two answers rather than one because they are genuinely different questions
/// and the gap between them is itself a finding: if they agree, every future
/// survey can be the cheap one, and if they do not, the cheap one is not
/// admissible evidence about storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reading {
    /// The container's present/absent words alone. No inflate.
    Presence,
    /// Every stored block inflated, tested against the fill and dropped.
    Contents,
}

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
    pub covered: usize,
    /// Of [`Self::stored`], the blocks that differ from the fill somewhere.
    ///
    /// `None` under [`Reading::Presence`], deliberately, rather than a copy of
    /// `stored` — a caller that cannot tell the two apart would report a number
    /// it never measured.
    pub live: Option<usize>,
    /// Canvas tiles the non-blank blocks reach. This is what a tiled Umber
    /// would actually allocate for this slice.
    pub live_covered: Option<usize>,
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
    /// Which of the two readings produced this. Carried rather than left to the
    /// caller to remember, because every `live_*` figure is `None` under one of
    /// them and a report has to say which question it answered.
    pub reading: Reading,
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

    /// What a tiled store would allocate for the same slices, charging every
    /// stored block. The upper bound; [`Self::live_bytes`] is the real answer.
    pub fn tiled_bytes(&self) -> u64 {
        Self::TILE_BYTES * self.covered() as u64
    }

    /// The same over the blocks that hold something. `None` under
    /// [`Reading::Presence`].
    pub fn live_bytes(&self) -> Option<u64> {
        Some(Self::TILE_BYTES * self.live_covered()? as u64)
    }

    /// One tile of a layer slice.
    pub const TILE_BYTES: u64 = (BLOCK * BLOCK * 4) as u64;

    /// Canvas tiles the stored blocks reach, over every slice.
    pub fn covered(&self) -> usize {
        self.slices.iter().map(|s| s.covered).sum()
    }

    /// The same over the non-blank blocks. `None` — rather than zero — where no
    /// slice was decoded, so an undecoded document cannot read as an empty one.
    pub fn live_covered(&self) -> Option<usize> {
        self.slices.iter().map(|s| s.live_covered).sum()
    }

    /// Tiles a dense store allocates for the slices that were measured.
    pub fn dense_tiles(&self) -> usize {
        self.canvas_tiles() * self.slices.len()
    }

    /// Tiles held against tiles a dense store would allocate.
    ///
    /// `None` for a document with nothing measurable in it, rather than a zero
    /// that would read as "wonderfully sparse".
    pub fn occupancy(&self) -> Option<f64> {
        (self.dense_tiles() > 0).then(|| self.covered() as f64 / self.dense_tiles() as f64)
    }

    /// The same over the blocks that hold something.
    pub fn live_occupancy(&self) -> Option<f64> {
        (self.dense_tiles() > 0)
            .then(|| Some(self.live_covered()? as f64 / self.dense_tiles() as f64))
            .flatten()
    }
}

impl SliceResidency {
    /// This slice's own share of its canvas, charging every stored block.
    pub fn occupancy(&self, canvas_tiles: usize) -> f64 {
        if canvas_tiles == 0 {
            return 0.0;
        }
        self.covered as f64 / canvas_tiles as f64
    }

    /// The same over the blocks that hold something.
    pub fn live_occupancy(&self, canvas_tiles: usize) -> Option<f64> {
        (canvas_tiles > 0).then(|| Some(self.live_covered? as f64 / canvas_tiles as f64))?
    }
}

/// Measure a Clip Studio document already in memory.
///
/// Named for the one format it reads rather than dispatching on an extension,
/// because there is nothing here to fall back to: a caller handed a `.psd` has
/// to be told that this question has not been answered for that format, and an
/// `UnsupportedExtension` out of a function called `survey` would read as a
/// file that could not be opened.
pub fn clip_studio(bytes: &[u8], reading: Reading) -> Result<DocumentResidency, ImportError> {
    clipstudio::residency(bytes, reading)
}

/// The part of one stored block that actually reaches the canvas.
///
/// This is the only arithmetic in the module and every figure above rests on
/// it, which is why **both** readings are derived from one placement rather
/// than computed twice. The block is 256 square in the *bitmap*, whose right
/// and bottom edges are padding the blit throws away; the bitmap sits at
/// `origin` in the document, which is generally not a multiple of 256; and the
/// canvas clips both ends. Two functions doing that separately is one of them
/// scanning a region the other did not charge for — a block reported as
/// covering a tile because of texels nobody can see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Placed {
    /// Half-open document span, clipped to the bitmap and to the page.
    x: std::ops::Range<i64>,
    y: std::ops::Range<i64>,
    /// Where this block's own `(0, 0)` sits in the document.
    at: (i64, i64),
}

impl Placed {
    /// Canvas tiles this block reaches. Half-open, per axis.
    pub(super) fn tiles(&self) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let range = |span: &std::ops::Range<i64>| {
            (span.start as usize / BLOCK)..((span.end as usize - 1) / BLOCK + 1)
        };
        (range(&self.x), range(&self.y))
    }

    /// The same region in the block's **own** coordinates, which is what a scan
    /// of its decompressed bytes indexes by.
    pub(super) fn local(&self) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        let range = |span: &std::ops::Range<i64>, at: i64| {
            ((span.start - at) as usize)..((span.end - at) as usize)
        };
        (range(&self.x, self.at.0), range(&self.y, self.at.1))
    }
}

/// Place one block, or `None` where none of it can be seen.
pub(super) fn place(
    block: (usize, usize),
    bitmap: UVec2,
    origin: (i64, i64),
    canvas: UVec2,
) -> Option<Placed> {
    let axis = |index: usize, extent: u32, at: i64, page: u32| {
        // The block's own span inside the bitmap, with the padding past the
        // bitmap's edge taken off — that padding is not the picture and
        // `colour`'s blit does not copy it.
        let start = (index * BLOCK) as i64;
        let end = i64::from(extent).min(start + BLOCK as i64);
        // Placed, then clipped to the page.
        let (lo, hi) = ((at + start).max(0), (at + end).min(i64::from(page)));
        (end > start && hi > lo).then_some(lo..hi)
    };
    Some(Placed {
        x: axis(block.0, bitmap.x, origin.0, canvas.x)?,
        y: axis(block.1, bitmap.y, origin.1, canvas.y)?,
        at: (
            origin.0 + (block.0 * BLOCK) as i64,
            origin.1 + (block.1 * BLOCK) as i64,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiles_touched(
        block: (usize, usize),
        bitmap: UVec2,
        origin: (i64, i64),
        canvas: UVec2,
    ) -> (std::ops::Range<usize>, std::ops::Range<usize>) {
        place(block, bitmap, origin, canvas).map_or((0..0, 0..0), |p| p.tiles())
    }

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

    /// **The scan reads exactly the region the tiles were charged for.**
    ///
    /// This is what the two answers coming off one `Placed` buys, and it is not
    /// cosmetic: a `.clip` block is padded past the bitmap's edge with bytes
    /// that are nobody's picture — the reader's own fixture writes `0x5a` there
    /// deliberately — so a content scan over the whole 256 square would call a
    /// blank layer live on the strength of padding. The same is true of the
    /// part hanging off the page.
    #[test]
    fn the_scanned_region_is_the_one_the_tiles_were_charged_for() {
        let canvas = UVec2::new(1024, 1024);
        // A 300-square bitmap: the second column holds 44 real pixels.
        let placed = place((1, 1), UVec2::new(300, 300), (0, 0), canvas).expect("on the page");
        assert_eq!(placed.local(), (0..44, 0..44));
        assert_eq!(placed.tiles(), (1..2, 1..2));

        // Hanging off the left edge: the first 100 columns of the block are off
        // the page, so the scan starts at 100.
        let placed = place((0, 0), UVec2::new(512, 512), (-100, 0), canvas).expect("on the page");
        assert_eq!(placed.local(), (100..256, 0..256));

        // Wholly off the page is no region at all rather than an empty one, so
        // a caller cannot scan it by mistake.
        assert!(place((0, 0), UVec2::new(512, 512), (-600, 0), canvas).is_none());
    }
}
