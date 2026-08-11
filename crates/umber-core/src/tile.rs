//! Where a layer's texels are actually stored.
//!
//! A layer occupies a full canvas-sized slice of one texture array, so it costs
//! 400 MB at 20000×5000 whether it holds a portrait or a signature. Measured
//! over the artist's 33 real documents by `examples/survey-residency.rs`,
//! **13.5% of a dense store's tiles hold paint**, and the bigger the document
//! the sparser it is: the 20000×5000 file that prompted this is 6.5% covered,
//! 21.20 GB dense against 1.42 GB of content. `docs/perf/tiled-layer-storage.md`
//! is the design.
//!
//! This module is the *arithmetic* half of it — a grid, an entry encoding and a
//! decomposition of a document rectangle into per-tile fragments — and it holds
//! no GPU type, so every rule here is testable without a device. The same
//! division `CanvasCopy::plan`, `Clip::place`, `ScrollSpan` and `band_rows`
//! already keep. `umber-render`'s `Atlas` is what turns these answers into
//! copies.
//!
//! # The shape, and the two choices that make it small
//!
//! - **A tile is [`TILE`] square, which is `.clip`'s own block size.** A
//!   `.clip` layer is stored as 256-square blocks and the file states which are
//!   absent, so residency for the documents that prompted this falls out of the
//!   file with no decode at all — `docimport::residency` is that reading.
//!   [`TILE`] is also a multiple of `damage::TILE`, so every damage cell lies
//!   wholly inside one storage tile and "which tiles did this stroke touch" is
//!   a shift of coordinates the `TileMask` already holds rather than a second
//!   rasterisation of the stroke.
//! - **A page is the canvas rounded up to whole tiles**, so a page holds
//!   exactly one layer's worth of tiles and the *identity* mapping — page `n`
//!   holding slot `n`'s tiles at their own coordinates — is byte for byte the
//!   layout Umber had before there was a page table at all. That is what makes
//!   the first stage of this work checkable against the second, and it is why
//!   nothing about growth, reservation, `Vram`'s refusal or `resize` changed
//!   shape: a page *is* what a slice was.
//!
//! # There is no apron, and that is a decision
//!
//! `docs/perf/tiled-layer-storage.md` §8.3 calls a stale apron "the real risk in
//! the whole design" — a one-texel seam that appears only at some zooms on some
//! layers. An apron is a copy of the logical neighbour's edge texels, which
//! exists so the *hardware* bilinear sampler can be pointed at an atlas whose
//! physical neighbours are unrelated tiles. It has to be refreshed by whoever
//! writes a tile, and forgetting one writer is exactly that seam.
//!
//! `composite.wgsl` reconstructs the bilinear tap by hand instead — four
//! `textureLoad`s resolved through the page table, lerped in the shader — which
//! is that document's refusal 7, re-ranked there from "refused" to "the
//! fallback, and a near-peer". Three things follow and all three are why it is
//! taken here rather than the apron:
//!
//! - **There is nothing to keep in step.** The drift this codebase refuses is
//!   between two *texts it maintains* — `blend.wgsl` compiled twice,
//!   `render_float` called twice. The hardware sampler is not such a text.
//! - **A tile's pitch equals its size**, so a page is the canvas rounded up and
//!   never larger. With an apron the pitch is 258 and a canvas sitting on the
//!   device's `max_texture_dimension_2d` — 32768, which `Document::MAX_EDGE`
//!   reaches — would need a page past it. That is not a case an apron width can
//!   be tuned out of.
//! - **`textureLoad` through an sRGB view decodes**, so a hand lerp is a lerp of
//!   linear values, exactly what the sampler does. That is not assumed:
//!   `flip.wgsl` goes out of its way to read through a *non*-sRGB view
//!   precisely because the sRGB one would decode.
//!
//! What it costs is instruction count on the composite's loop, and what it
//! cannot promise is the sampler's last bit at a magnified antialiased edge —
//! the hardware's interpolation weights are fixed-point where these are `f32`.
//! Where a bilinear tap lands on a texel centre, which is every sample at zoom
//! 1, both are exact and the comparison may promise bytes.

use crate::geom::PixelRect;
use glam::UVec2;

/// The side of one storage tile, in document pixels.
///
/// 256 because that is `csblocks::BLOCK` — a Clip Studio layer is stored in
/// 256-square blocks and the file says which are absent, so residency for the
/// documents this exists for is readable without inflating anything. It is also
/// four times `damage::TILE`, which [`TILE_OVER_DAMAGE`] asserts.
pub const TILE: u32 = 256;

/// A storage tile is a whole number of damage cells.
///
/// What it buys: every damage cell lies wholly inside one storage tile, so a
/// stroke-derived piece decomposes into whole intra-tile rectangles with no
/// partial-cell arithmetic, and the set of tiles a stroke touched is a shift of
/// the cell coordinates `TileMask` already holds.
///
/// A `const` assertion rather than a comment, because the failure is silent:
/// with a tile that is not a multiple of the cell, a piece straddles tiles in a
/// way the decomposition below still handles correctly and every figure about
/// residency stops being derivable from damage.
pub const TILE_OVER_DAMAGE: () = assert!(TILE.is_multiple_of(crate::damage::TILE));

/// The largest tile grid a page table will be asked for, per axis.
///
/// `Document::MAX_EDGE / TILE`. [`Entry`] packs a tile's position within its
/// page into a byte per axis, so this has to fit in one — 128 does, with the
/// same headroom again.
pub const MAX_TILES_PER_AXIS: u32 = crate::document::Document::MAX_EDGE / TILE;
const _: () = assert!(MAX_TILES_PER_AXIS <= 256);

/// Where one logical tile of one slot is stored, or that it is stored nowhere.
///
/// Packed into a `u32` because the page table is a `texture_2d_array<u32>` that
/// a fragment shader reads with one `textureLoad`: `page << 16 | y << 8 | x`,
/// with [`Entry::UNBACKED`] the sentinel. The shader has the same three lines,
/// and [`Entry::PACKING`] is what pins them against each other — a rename or a
/// shift here that the WGSL did not follow is a picture assembled out of the
/// wrong tiles, which looks like corruption rather than like a bug.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry(pub u32);

impl Entry {
    /// This tile holds nothing, and reads as the slot's own empty value.
    ///
    /// **Not zero**, which is a legitimate entry — page 0, tile (0, 0) — and is
    /// what a freshly created `R32Uint` texture would hold. The sentinel has to
    /// be a value the packing cannot produce, and all-ones is one because
    /// `page` is bounded by 256.
    pub const UNBACKED: Self = Self(u32::MAX);

    /// The packing, as the shader has to spell it: `(page, y, x)` in bits
    /// `(16.., 8..16, 0..8)`.
    ///
    /// Named so `an_entry_packs_the_way_the_shader_unpacks_it` can drive it,
    /// and so the shader's own constants can be compared against these rather
    /// than against a sentence.
    pub const PACKING: (u32, u32, u32) = (16, 8, 0);

    /// A tile living at `(x, y)` within `page`.
    pub fn at(page: u32, x: u32, y: u32) -> Self {
        debug_assert!(page < 256 && x < 256 && y < 256);
        Self((page << 16) | (y << 8) | x)
    }

    pub fn is_backed(self) -> bool {
        self != Self::UNBACKED
    }

    pub fn page(self) -> u32 {
        self.0 >> 16
    }

    /// The tile's position within its page, in tiles.
    pub fn cell(self) -> (u32, u32) {
        ((self.0 >> 8) & 0xff, self.0 & 0xff)
    }

    /// The tile's top-left corner within its page, in texels.
    pub fn origin(self) -> (u32, u32) {
        let (y, x) = self.cell();
        (x * TILE, y * TILE)
    }
}

/// One tile's share of a document rectangle.
///
/// The unit every copy is issued in. `doc` is the sub-rectangle in document
/// pixels, `tile` is which logical tile it lies in, and `within` is where it
/// starts inside that tile — so a caller that has resolved the tile to an
/// [`Entry`] adds `entry.origin()` to `within` and has the atlas texel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fragment {
    pub doc: PixelRect,
    pub tile: (u32, u32),
    pub within: (u32, u32),
}

/// The tile grid of one canvas.
///
/// Cheap to copy and carried by the renderer beside the canvas size, because
/// every one of these answers is a function of that size alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub doc_size: UVec2,
    pub tiles: UVec2,
}

impl Grid {
    pub fn new(doc_size: UVec2) -> Self {
        Self {
            doc_size,
            tiles: UVec2::new(doc_size.x.div_ceil(TILE), doc_size.y.div_ceil(TILE)),
        }
    }

    /// How many tiles one page holds — and therefore how many one layer needs
    /// to be wholly resident.
    ///
    /// A page is the canvas rounded up to whole tiles, so these are the same
    /// number, which is what makes the identity mapping in the first stage a
    /// statement about *nothing having moved* rather than about a coincidence.
    pub fn tiles_per_page(&self) -> u32 {
        self.tiles.x * self.tiles.y
    }

    /// The page texture's dimensions: the canvas rounded up to whole tiles.
    ///
    /// Never larger than the device's `max_texture_dimension_2d`, and that is
    /// derived rather than hoped for: every value that limit takes is a
    /// multiple of [`TILE`], and the canvas is already inside it, so rounding
    /// the canvas up to a multiple of 256 cannot cross it.
    pub fn page_size(&self) -> UVec2 {
        self.tiles * TILE
    }

    /// Which tile a document pixel lies in.
    pub fn tile_of(&self, x: u32, y: u32) -> (u32, u32) {
        (x / TILE, y / TILE)
    }

    /// The index a tile takes in a slot's row-major page-table slice, and in the
    /// identity page layout.
    pub fn index(&self, tx: u32, ty: u32) -> usize {
        (ty * self.tiles.x + tx) as usize
    }

    /// The document rectangle one whole tile covers, clipped to the canvas.
    ///
    /// Clipped, because the right and bottom tiles of a canvas whose size is not
    /// a multiple of [`TILE`] are partial. The *storage* is a full tile either
    /// way — a page is rounded up — so the padding exists and is never read,
    /// which is what lets a tile be relocated into any free slot of any page
    /// without asking how big it is.
    pub fn tile_rect(&self, tx: u32, ty: u32) -> PixelRect {
        let x = tx * TILE;
        let y = ty * TILE;
        PixelRect {
            x,
            y,
            width: TILE.min(self.doc_size.x.saturating_sub(x)),
            height: TILE.min(self.doc_size.y.saturating_sub(y)),
        }
    }

    /// Cut a document rectangle into the tiles it crosses.
    ///
    /// Row-major, and clipped to the canvas — a rectangle reaching past the
    /// edge yields nothing for the part outside, because there is no tile there
    /// and a copy naming one would be a validation error.
    ///
    /// The order matters to one caller and not the rest: a readback stitches the
    /// fragments back into a tightly packed buffer by copying each fragment's
    /// rows to their own offsets, so any order would do — but a *write* of a
    /// banded upload walks these in the order it was handed them, and row-major
    /// is what keeps that a forward scan of the caller's bytes.
    pub fn fragments(&self, rect: PixelRect) -> Vec<Fragment> {
        let x0 = rect.x.min(self.doc_size.x);
        let y0 = rect.y.min(self.doc_size.y);
        let x1 = rect.x.saturating_add(rect.width).min(self.doc_size.x);
        let y1 = rect.y.saturating_add(rect.height).min(self.doc_size.y);
        if x1 <= x0 || y1 <= y0 {
            return Vec::new();
        }

        let (tx0, ty0) = self.tile_of(x0, y0);
        let (tx1, ty1) = self.tile_of(x1 - 1, y1 - 1);
        let mut out = Vec::with_capacity(((tx1 - tx0 + 1) * (ty1 - ty0 + 1)) as usize);
        for ty in ty0..=ty1 {
            let ry0 = y0.max(ty * TILE);
            let ry1 = y1.min((ty + 1) * TILE);
            for tx in tx0..=tx1 {
                let rx0 = x0.max(tx * TILE);
                let rx1 = x1.min((tx + 1) * TILE);
                out.push(Fragment {
                    doc: PixelRect {
                        x: rx0,
                        y: ry0,
                        width: rx1 - rx0,
                        height: ry1 - ry0,
                    },
                    tile: (tx, ty),
                    within: (rx0 - tx * TILE, ry0 - ty * TILE),
                });
            }
        }
        out
    }

    /// Every tile a document rectangle touches, row-major and deduplicated.
    ///
    /// What an allocator is asked for before a commit: [`Self::fragments`]
    /// answers the same question and carries the geometry, so this exists only
    /// where the geometry is not wanted and building it would be waste.
    pub fn tiles_over(&self, rect: PixelRect) -> Vec<(u32, u32)> {
        let x1 = rect.x.saturating_add(rect.width).min(self.doc_size.x);
        let y1 = rect.y.saturating_add(rect.height).min(self.doc_size.y);
        if x1 <= rect.x || y1 <= rect.y {
            return Vec::new();
        }
        let (tx0, ty0) = self.tile_of(rect.x, rect.y);
        let (tx1, ty1) = self.tile_of(x1 - 1, y1 - 1);
        let mut out = Vec::new();
        for ty in ty0..=ty1 {
            for tx in tx0..=tx1 {
                out.push((tx, ty));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
        PixelRect {
            x,
            y,
            width,
            height,
        }
    }

    /// The shader unpacks this by hand, so the packing is pinned as the three
    /// shifts rather than as a round trip — a round trip is self-consistent
    /// under any rearrangement, which is exactly the failure `BlendMode`'s
    /// serialised names record.
    #[test]
    fn an_entry_packs_the_way_the_shader_unpacks_it() {
        assert_eq!(Entry::PACKING, (16, 8, 0));
        let e = Entry::at(3, 5, 7);
        assert_eq!(e.0, (3 << 16) | (7 << 8) | 5);
        assert_eq!(e.page(), 3);
        assert_eq!(e.cell(), (7, 5));
        assert_eq!(e.origin(), (5 * TILE, 7 * TILE));
        assert!(e.is_backed());
    }

    /// Zero is a real entry — page 0, tile (0, 0), which is where the very first
    /// tile of the very first layer lives — so the sentinel may not be it. A
    /// fresh `R32Uint` texture reads as zeroes, so a zero sentinel would make an
    /// uninitialised page table say "every tile is the first one" instead of
    /// "nothing is backed".
    #[test]
    fn the_unbacked_sentinel_is_not_a_reachable_entry() {
        assert!(!Entry::UNBACKED.is_backed());
        assert!(Entry::at(0, 0, 0).is_backed());
        assert_eq!(Entry::at(0, 0, 0).0, 0);
        assert_ne!(Entry::UNBACKED, Entry::at(255, 255, 255));
    }

    #[test]
    fn a_page_is_the_canvas_rounded_up_to_whole_tiles() {
        let g = Grid::new(UVec2::new(20000, 5000));
        assert_eq!(g.tiles, UVec2::new(79, 20));
        assert_eq!(g.page_size(), UVec2::new(20224, 5120));
        assert_eq!(g.tiles_per_page(), 1580);

        // Exactly on a tile boundary rounds to itself, which is what keeps a
        // canvas sitting on the device's own ceiling representable.
        let g = Grid::new(UVec2::new(32768, 32768));
        assert_eq!(g.page_size(), UVec2::new(32768, 32768));
        assert_eq!(g.tiles, UVec2::splat(MAX_TILES_PER_AXIS));
    }

    /// The claim the whole page geometry rests on: rounding a canvas up to a
    /// multiple of the tile cannot take it past a device limit it was already
    /// inside, because every value that limit takes is itself a multiple of the
    /// tile. Swept rather than argued, over the figures `measure-limits`
    /// actually reports from real adapters.
    #[test]
    fn rounding_a_canvas_up_to_tiles_never_passes_the_device_limit() {
        for limit in [2048u32, 4096, 8192, 16384, 32768] {
            assert!(limit.is_multiple_of(TILE), "{limit} is not a whole tile");
            for edge in [1u32, 2, 255, 256, 257, limit - 1, limit] {
                let g = Grid::new(UVec2::new(edge, edge));
                assert!(
                    g.page_size().x <= limit,
                    "canvas {edge} on a {limit} device wants a {} page",
                    g.page_size().x
                );
            }
        }
    }

    #[test]
    fn a_rectangle_inside_one_tile_is_one_fragment() {
        let g = Grid::new(UVec2::new(1000, 1000));
        let f = g.fragments(rect(10, 20, 30, 40));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].doc, rect(10, 20, 30, 40));
        assert_eq!(f[0].tile, (0, 0));
        assert_eq!(f[0].within, (10, 20));
    }

    #[test]
    fn a_rectangle_crossing_a_boundary_is_cut_at_it() {
        let g = Grid::new(UVec2::new(1000, 1000));
        let f = g.fragments(rect(250, 250, 12, 12));
        assert_eq!(f.len(), 4);
        // Row-major: (0,0), (1,0), (0,1), (1,1).
        assert_eq!(f[0].tile, (0, 0));
        assert_eq!(f[0].doc, rect(250, 250, 6, 6));
        assert_eq!(f[0].within, (250, 250));
        assert_eq!(f[1].tile, (1, 0));
        assert_eq!(f[1].doc, rect(256, 250, 6, 6));
        assert_eq!(f[1].within, (0, 250));
        assert_eq!(f[3].tile, (1, 1));
        assert_eq!(f[3].within, (0, 0));
    }

    /// Every fragment lies wholly inside its own tile and the fragments tile the
    /// rectangle exactly — no pixel copied twice, none missed. A copy issued
    /// past a tile's own bounds would read a neighbour's texels, which is the
    /// characteristic failure of this whole design and does not look like a bug
    /// so much as like corruption.
    #[test]
    fn fragments_cover_a_rectangle_exactly_once() {
        let g = Grid::new(UVec2::new(700, 600));
        for r in [
            rect(0, 0, 700, 600),
            rect(1, 1, 698, 598),
            rect(255, 255, 2, 2),
            rect(512, 512, 188, 88),
            rect(300, 0, 1, 600),
        ] {
            let mut seen = vec![0u8; (700 * 600) as usize];
            for f in g.fragments(r) {
                assert!(f.within.0 + f.doc.width <= TILE);
                assert!(f.within.1 + f.doc.height <= TILE);
                assert_eq!(f.doc.x, f.tile.0 * TILE + f.within.0);
                assert_eq!(f.doc.y, f.tile.1 * TILE + f.within.1);
                for y in f.doc.y..f.doc.y + f.doc.height {
                    for x in f.doc.x..f.doc.x + f.doc.width {
                        seen[(y * 700 + x) as usize] += 1;
                    }
                }
            }
            for y in r.y..r.y + r.height {
                for x in r.x..r.x + r.width {
                    assert_eq!(seen[(y * 700 + x) as usize], 1, "{r:?} at {x},{y}");
                }
            }
        }
    }

    /// A rectangle reaching past the canvas yields nothing for the part outside.
    /// Not tidiness: there is no tile there, and a copy naming one is a
    /// validation error, which is fatal.
    #[test]
    fn nothing_outside_the_canvas_is_a_fragment() {
        let g = Grid::new(UVec2::new(300, 300));
        let f = g.fragments(rect(200, 200, 400, 400));
        let far = f
            .iter()
            .map(|f| (f.doc.x + f.doc.width, f.doc.y + f.doc.height))
            .fold((0, 0), |a, b| (a.0.max(b.0), a.1.max(b.1)));
        assert_eq!(far, (300, 300));
        assert!(g.fragments(rect(300, 0, 10, 10)).is_empty());
        assert!(g.fragments(rect(0, 0, 0, 10)).is_empty());
    }

    /// The right and bottom tiles of a canvas that is not a whole number of
    /// tiles are partial in the *document* and whole in *storage*. Both halves
    /// matter: the document half is what a copy is sized by, and the storage
    /// half is what lets any tile go in any free slot.
    #[test]
    fn an_edge_tile_is_partial_in_the_document_and_whole_in_storage() {
        let g = Grid::new(UVec2::new(300, 300));
        assert_eq!(g.tile_rect(0, 0), rect(0, 0, 256, 256));
        assert_eq!(g.tile_rect(1, 0), rect(256, 0, 44, 256));
        assert_eq!(g.tile_rect(1, 1), rect(256, 256, 44, 44));
        assert_eq!(g.page_size(), UVec2::new(512, 512));
    }

    #[test]
    fn tiles_over_names_the_same_tiles_the_fragments_do() {
        let g = Grid::new(UVec2::new(1000, 800));
        for r in [
            rect(0, 0, 1000, 800),
            rect(100, 700, 300, 90),
            rect(0, 0, 1, 1),
        ] {
            let from_fragments: Vec<_> = g.fragments(r).iter().map(|f| f.tile).collect();
            assert_eq!(g.tiles_over(r), from_fragments);
        }
    }
}
