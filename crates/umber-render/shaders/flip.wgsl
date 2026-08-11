// Mirrors one layer slice left-to-right or top-to-bottom.
//
// This pass exists to be **exactly reversible**, because undoing a flip is
// another flip: the history entry stores no pixels at all (see
// `umber_core::history::EditBody::Flip`), so any loss here would compound every
// time somebody flipped and undid. Three things make it a pure permutation of
// texels rather than a resampling:
//
// * `textureLoad` with integer coordinates. No sampler is bound at all, so
//   there is no filtering to round and no edge rule to get wrong.
// * The views on both sides are **non-sRGB** views of the `Rgba8UnormSrgb`
//   layer array. A raw u8 read as `n / 255` and written back is exact in f32;
//   decoding to linear and re-encoding would be a promise about rounding, which
//   is exactly what the rest of this renderer refuses to make.
// * No blending. The pipeline's target has `blend: None`, so the fragment is
//   the destination.
//
// The source is a *different* texture from the target — a texture cannot be its
// own render attachment, and `copy_texture_to_texture` cannot mirror — so
// `CanvasRenderer::flip_layers` renders each slot into a page-sized scratch and
// copies the tiles it wants straight back.
//
// # Why this reads the page table where `resize` does not
//
// A layer's texels live in tiles scattered through an atlas (see `tiles.wgsl`),
// and a resize can move them with `copy_texture_to_texture` alone because every
// move is a *translation*. A mirror is not, and `copy_texture_to_texture` cannot
// mirror — so this is the one storage path that genuinely needs a pass, and
// therefore the one that needs `tile_load`. The scratch is laid out at
// **identity** page positions, which is what lets one pass do a whole slot: a
// fragment at page position `p` is document pixel `p`, mirrors to a source
// document pixel, and `tile_load` finds wherever that is stored.
//
// The atlas is bound through a **non-sRGB** array view for the reason above, and
// so is the scratch. `tiles.wgsl`'s own docs say a `textureLoad` through an sRGB
// view decodes; here that is exactly what must not happen.

struct Flip {
    // The canvas, in pixels.
    doc_size: vec2<u32>,
    // 0 mirrors x, anything else mirrors y.
    axis: u32,
    // Which slot of the page table to resolve through.
    slot: u32,
    // What an unbacked tile of that slot reads as: transparent for a layer,
    // white for a mask. The caller's, because this shader cannot know — see
    // `SlotClass`.
    empty: vec4<f32>,
};

@group(0) @binding(0) var<uniform> u: Flip;
@group(0) @binding(1) var atlas: texture_2d_array<f32>;
@group(0) @binding(2) var page_table: texture_2d_array<u32>;

// One oversized triangle covering the whole target. The same shape
// `composite.wgsl` uses; a quad would need a diagonal seam this pass has no
// reason to have.
@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    var pts = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(pts[vi], 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Fragment positions are pixel *centres*, so the truncation is the pixel
    // index and never lands on a boundary.
    let dst = vec2<i32>(i32(pos.x), i32(pos.y));
    let last = vec2<i32>(i32(u.doc_size.x) - 1, i32(u.doc_size.y) - 1);
    // The scratch is a *page*, which is the canvas rounded up to whole tiles, so
    // the fragments past the canvas edge are padding no copy reaches. They still
    // have to be answered without asking `tile_load` for a texel outside the
    // document, which is the one thing it says it cannot check for itself.
    if (dst.x > last.x || dst.y > last.y) {
        return u.empty;
    }
    var src_at = dst;
    if (u.axis == 0u) {
        src_at.x = last.x - dst.x;
    } else {
        src_at.y = last.y - dst.y;
    }
    return tile_load(atlas, page_table, i32(u.slot), src_at, u.empty);
}
