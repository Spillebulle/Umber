// Resolving a document texel of a layer to a texel of the atlas.
//
// A layer no longer owns a canvas-sized slice. Its texels live in 256-square
// tiles scattered through an atlas of *pages*, and a page table -- a
// `texture_2d_array<u32>` indexed by (tile, slot) -- says where each of a slot's
// tiles is, or that it is not stored at all. `umber_core::tile` is the same
// arithmetic on the Rust side and `docs/perf/tiled-layer-storage.md` is the
// design.
//
// This file is concatenated in front of every shader that reads the layer array
// -- `composite.wgsl`, `effect.wgsl`, `thumbnail.wgsl` -- exactly as
// `blend.wgsl` is concatenated in front of the composite and the commit, and for
// the same reason: three hand-written copies of an unpack is three places for
// the picture to be assembled out of the wrong tiles. Two consequences to know
// before reading a shader error: naga counts lines from the start of the
// concatenated text, so a line number it reports against any of those three is
// shifted by the length of this file (and, for the composite, of `blend.wgsl`
// too); and the textures are passed as function parameters rather than being
// declared here, because the three consumers number their bindings differently.
//
// # There is no apron
//
// The physical neighbour of a tile's edge texel is an unrelated tile, so the
// hardware bilinear sampler cannot be pointed at the atlas. The usual answer is
// an apron -- a copy of the logical neighbour's edge texels around every tile --
// and `docs/perf/tiled-layer-storage.md` §8.3 calls a *stale* one "the real risk
// in the whole design": a one-texel seam that appears only at some zooms on some
// layers, because one writer forgot to refresh it.
//
// `tile_bilinear` reconstructs the tap by hand instead: four `textureLoad`s
// resolved through the page table, lerped here. That is the design's refusal 7,
// which it re-ranked from "refused" to "the fallback, and a near-peer". See
// `umber_core::tile`'s module docs for the whole argument; the short form is
// that the hardware sampler is not a text Umber maintains, so there is nothing
// for a hand lerp to drift from, and that dropping the apron makes a tile's
// pitch equal its size -- which is what lets a page be the canvas rounded up and
// never larger than a limit the canvas was already inside.
//
// `textureLoad` through an sRGB view applies the transfer function, so these are
// lerps of *linear* values, which is what the sampler does. That is checked
// rather than assumed: `flip.wgsl` reads through a deliberately non-sRGB view
// precisely because the sRGB one would decode.

// The side of one storage tile. MUST equal `umber_core::tile::TILE`;
// `the_shader_and_the_model_agree_about_a_tile` parses this line to say so.
const TILE: i32 = 256;

// The page table's "this tile is stored nowhere" sentinel. MUST equal
// `umber_core::tile::Entry::UNBACKED`, and it is all-ones rather than zero
// because zero is a real entry -- page 0, tile (0, 0).
const TILE_UNBACKED: u32 = 4294967295u;

// The packing an entry uses: `page << 16 | y << 8 | x`. MUST match
// `umber_core::tile::Entry::PACKING`, and all three are named — the x field's
// shift is zero and would be the one nobody pinned.
const TILE_PAGE_SHIFT: u32 = 16u;
const TILE_Y_SHIFT: u32 = 8u;
const TILE_X_SHIFT: u32 = 0u;

// Where in the atlas one document texel of one slot lives, given its tile's
// entry. `p` is the document texel and `t` is its tile.
fn tile_atlas_texel(entry: u32, p: vec2<i32>, t: vec2<i32>) -> vec2<i32> {
    let cell = vec2<i32>(
        i32((entry >> TILE_X_SHIFT) & 255u),
        i32((entry >> TILE_Y_SHIFT) & 255u),
    );
    return cell * TILE + (p - t * TILE);
}

// One document texel of one slot, or `empty` where its tile is not stored.
//
// `empty` is the slot's own empty value and is the caller's to supply, because
// it is not the same for every slot: a layer's is transparent black and a
// **mask's is white**, since a mask multiplies the layer's alpha and a mask
// nobody has painted on reveals everything. Taking a mask's absent tile for zero
// hides the layer everywhere nobody painted -- which is precisely the bug
// `clipstudio.rs` records fixing on the import side, in the same format, at the
// same block size.
//
// `p` must already be inside the canvas. Every caller clamps or bounds it, and
// this cannot: it has no idea how large the document is.
fn tile_load(
    atlas: texture_2d_array<f32>,
    table: texture_2d_array<u32>,
    slot: i32,
    p: vec2<i32>,
    empty: vec4<f32>,
) -> vec4<f32> {
    let t = p / TILE;
    let entry = textureLoad(table, t, slot, 0).r;
    if (entry == TILE_UNBACKED) {
        return empty;
    }
    return textureLoad(atlas, tile_atlas_texel(entry, p, t), i32(entry >> TILE_PAGE_SHIFT), 0);
}

// One texel of an atlas tile whose entry has already been resolved.
//
// The four-taps-one-entry half of `tile_bilinear`, split out because that is the
// path 99.2% of samples take and it must not pay for the page table four times.
fn tile_texel(
    atlas: texture_2d_array<f32>,
    entry: u32,
    p: vec2<i32>,
    t: vec2<i32>,
) -> vec4<f32> {
    return textureLoad(
        atlas,
        tile_atlas_texel(entry, p, t),
        i32(entry >> TILE_PAGE_SHIFT),
        0,
    );
}

// What `textureSampleLevel(layer, clamping_sampler, doc / doc_size, slot, 0)`
// used to answer, computed from four loads.
//
// `doc` is in document pixels, so the texel centre of texel `n` is `n + 0.5` --
// which is why this subtracts a half before flooring, and why a tap that lands
// on a texel centre comes out with weights of exactly 0 and 1 and returns
// exactly one stored texel. **That is a statement about the tap, not about the
// zoom**: at zoom 1 the offset is `camera.center - pivot`, and neither is
// constrained to be whole, so an ordinary pan puts a fractional weight on every
// sample. What is exact is the export, the two picks and the autosave preview,
// which composite at zoom 1 with the centre and the pivot both at the middle of
// their own target.
//
// The clamp reproduces the sampler's `ClampToEdge`, and it is what a tap at the
// canvas's own border needs: without it the two outer taps would resolve tiles
// that do not exist. It is also what makes the integer divide below safe --
// nothing negative reaches it.
//
// **The common path is one page-table read, not four.** A tap straddles a tile
// boundary only when the sample sits within half a texel of one, which is
// `1 - (255/256)^2` -- about 0.78% -- of interior samples; the other 99.2%
// resolve through a single entry. This loop runs per layer per fragment on the
// pass that is already the frame's dominant cost, so the difference is four
// dependent fetches against one, doubled again wherever a layer carries a mask.
// `docs/perf/tiled-layer-storage.md` §8.1 is where the hand lerp is argued for
// against the apron, and this is the count that argument turns on.
fn tile_bilinear(
    atlas: texture_2d_array<f32>,
    table: texture_2d_array<u32>,
    slot: i32,
    doc: vec2<f32>,
    doc_size: vec2<i32>,
    empty: vec4<f32>,
) -> vec4<f32> {
    let centred = doc - vec2<f32>(0.5);
    let base = floor(centred);
    let w = centred - base;
    let hi = doc_size - vec2<i32>(1);
    let lo = clamp(vec2<i32>(base), vec2<i32>(0), hi);
    let up = clamp(vec2<i32>(base) + vec2<i32>(1), vec2<i32>(0), hi);

    let t_lo = lo / TILE;
    let t_up = up / TILE;
    if (t_lo.x == t_up.x && t_lo.y == t_up.y) {
        // All four taps are in one tile, so one entry answers for all of them --
        // including "not stored", which makes the whole tap the empty value.
        let entry = textureLoad(table, t_lo, slot, 0).r;
        if (entry == TILE_UNBACKED) {
            return empty;
        }
        let c00 = tile_texel(atlas, entry, vec2<i32>(lo.x, lo.y), t_lo);
        let c10 = tile_texel(atlas, entry, vec2<i32>(up.x, lo.y), t_lo);
        let c01 = tile_texel(atlas, entry, vec2<i32>(lo.x, up.y), t_lo);
        let c11 = tile_texel(atlas, entry, vec2<i32>(up.x, up.y), t_lo);
        return mix(mix(c00, c10, w.x), mix(c01, c11, w.x), w.y);
    }

    // Straddling: up to four tiles, resolved one at a time. This is the case an
    // apron exists to avoid and the case that makes a seam if it is got wrong.
    let c00 = tile_load(atlas, table, slot, vec2<i32>(lo.x, lo.y), empty);
    let c10 = tile_load(atlas, table, slot, vec2<i32>(up.x, lo.y), empty);
    let c01 = tile_load(atlas, table, slot, vec2<i32>(lo.x, up.y), empty);
    let c11 = tile_load(atlas, table, slot, vec2<i32>(up.x, up.y), empty);
    return mix(mix(c00, c10, w.x), mix(c01, c11, w.x), w.y);
}
