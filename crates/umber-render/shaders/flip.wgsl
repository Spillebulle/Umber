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
// `CanvasRenderer::flip_layers` renders each slice into a scratch copy and
// copies it straight back.

struct Flip {
    // The canvas, in pixels.
    doc_size: vec2<u32>,
    // 0 mirrors x, anything else mirrors y. Scalars for the padding, not a
    // vec3: see the uniform-layout note in CLAUDE.md.
    axis: u32,
    _pad: u32,
};

@group(0) @binding(0) var<uniform> u: Flip;
@group(0) @binding(1) var src: texture_2d<f32>;

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
    var src_at = dst;
    if (u.axis == 0u) {
        src_at.x = last.x - dst.x;
    } else {
        src_at.y = last.y - dst.y;
    }
    return textureLoad(src, src_at, 0);
}
