// Reduces a rectangle of one layer slice to a small square, for the layer
// list's thumbnails.
//
// It is one pass run twice, with `reduce` deciding which of the two questions
// the layer list has to ask:
//
//   * `reduce == 1` — the **greatest** alpha under each destination texel, over
//     the whole slice. That is how the content bounding box is found, and it has
//     to be a maximum rather than a mean: a one-pixel line averaged over a 32×32
//     cell is an alpha of 1/1024, which is zero in the eight bits the readback
//     carries, so a mean would report every sketched layer as empty. See
//     `umber_core::thumbnail`.
//   * `reduce == 0` — the **mean** over the region the first pass found, which
//     is what downscaling a picture is.
//
// `textureLoad` rather than a sampler, for two reasons that are both the
// selection mask's. Bilinear filtering at a reduction of 30:1 is a point sample
// with extra steps — it would drop nearly every texel, which is the failure the
// maximum exists to avoid — and the region deliberately runs off the edge of the
// canvas, where clamp-to-edge would smear the boundary row across the margin.
// Outside the slice is decided arithmetically and reads as transparent.

struct Thumb {
    // The region to reduce, in document pixels. May extend outside the canvas.
    src_min: vec2<f32>,
    src_size: vec2<f32>,
    // The target, in texels.
    dest: vec2<u32>,
    // The slice, in texels.
    layer_size: vec2<u32>,
    slot: u32,
    // 0 = mean of the region, 1 = greatest alpha in it.
    reduce: u32,
    // Scalars, not a vec2 pad: see the uniform-layout note in CLAUDE.md.
    _pad0: u32,
    _pad1: u32,
};

// The most taps taken along one axis of one destination texel.
//
// A full box filter reads every texel of the slice exactly once across the whole
// target, which is the same bandwidth the composite pass spends every frame — so
// this is a bound on pathological loops rather than a budget. At 64 texels of
// destination it is reached only by a canvas over 16384 wide, which is past
// `max_texture_dimension_2d` on the limits Umber requests.
const MAX_TAPS: i32 = 256;

@group(0) @binding(0) var<uniform> u: Thumb;
@group(0) @binding(1) var layers: texture_2d_array<f32>;

fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lower = c * 12.92;
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

// One oversized triangle, as every other full-target pass here uses.
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
    let px = vec2<f32>(floor(pos.x), floor(pos.y));
    let dest = vec2<f32>(f32(u.dest.x), f32(u.dest.y));

    // The slab of the region this destination texel stands for.
    let lo = u.src_min + u.src_size * (px / dest);
    let hi = u.src_min + u.src_size * ((px + vec2<f32>(1.0)) / dest);

    let first = vec2<i32>(i32(floor(lo.x)), i32(floor(lo.y)));
    let last = vec2<i32>(i32(ceil(hi.x)), i32(ceil(hi.y)));
    let span = max(last - first, vec2<i32>(1, 1));
    // Only ever above one on a canvas larger than the device will make.
    let step = max((span + MAX_TAPS - 1) / MAX_TAPS, vec2<i32>(1, 1));

    let bounds = vec2<i32>(i32(u.layer_size.x), i32(u.layer_size.y));
    var acc = vec4<f32>(0.0);
    var peak = 0.0;
    var taps = 0.0;
    for (var y = 0; y < span.y; y += step.y) {
        for (var x = 0; x < span.x; x += step.x) {
            let at = first + vec2<i32>(x, y);
            // Outside the slice is transparent, and counts towards the mean:
            // that is what leaves the margin around a mark in the corner.
            var texel = vec4<f32>(0.0);
            if (at.x >= 0 && at.y >= 0 && at.x < bounds.x && at.y < bounds.y) {
                texel = textureLoad(layers, at, i32(u.slot), 0);
            }
            acc += texel;
            peak = max(peak, texel.a);
            taps += 1.0;
        }
    }

    if (u.reduce == 1u) {
        // Only the alpha is read back. The colour is not merely unused — a
        // maximum of premultiplied colour is not a colour of anything.
        return vec4<f32>(0.0, 0.0, 0.0, peak);
    }

    let mean = acc / max(taps, 1.0);
    if (mean.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    // Straight alpha, sRGB encoded — the same form `composite.wgsl`'s export
    // branch hands back, and what `Color32::from_rgba_unmultiplied` wants. The
    // target is a non-sRGB format, so the encode is this shader's to do.
    return vec4<f32>(linear_to_srgb(mean.rgb / mean.a), mean.a);
}
