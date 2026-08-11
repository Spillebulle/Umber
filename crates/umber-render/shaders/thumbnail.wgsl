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
// A full box filter reads every texel of the region exactly once across the
// whole target, so this is a bound on pathological loops rather than a budget.
//
// **What stepping costs is not the same in the two passes, and the difference is
// the whole reason the constant had to move.** In the *picture* pass it is
// aliasing: the mean divides by `taps`, which counts only what was visited, so a
// stepped result is a correctly normalised mean of a subsample — worse, not
// wrong. In the *bounds* pass it is a wrong answer, because that reduction is a
// maximum taken precisely so a one-pixel line survives being shrunk into a cell.
// A step of two visits every other column, so the line falls between the taps
// and the layer reports empty. A painted layer drawing a blank thumbnail is what
// that looks like, and it is unrecoverable rather than merely soft.
//
// So the constant has to be past the widest span any canvas Umber admits can
// produce, and it is. **It was not, and the comment that stood here explained
// why in a way that was false.** It said the clamp was reached only by a canvas
// over 16384 wide, "which is past `max_texture_dimension_2d` on the limits Umber
// requests" — but `Gpu::using_resolution` raises exactly that limit from the
// adapter, and `Document::MAX_EDGE` is 32768. An RTX 3080 on Vulkan reports
// 32768. A 20000-wide document is one somebody has, and its bounds pass stepped
// by two.
//
// The derivation, at `dest` of `thumbnail::SIZE`:
//
//   * the **bounds** pass reduces the whole slice, so a destination texel spans
//     `MAX_EDGE / SIZE` source texels, plus one for the floor and ceil either
//     side — 513 at 32768;
//   * the **picture** pass reduces the region `thumbnail::framed` chose, and
//     that is the content inflated by `1 / (1 - 2 * PADDING)` so the mark does
//     not touch the edge of the chip. Content can be the whole canvas, so the
//     region reaches `MAX_EDGE / (1 - 2 * PADDING)` and the span reaches 611.
//     **This is the larger of the two and it is the one nobody looked at**: it
//     bites from a content box of about 13710 px, which is inside 16384 and
//     therefore reachable on hardware that caps there — every D3D12 and Metal
//     device, WARP and lavapipe included. Only fidelity is lost there, per the
//     paragraph above, but 611 is the figure the constant has to clear.
//
// 1024 is the next power of two above 611, so a change to `SIZE` or `PADDING`
// does not silently re-arm the clamp. `the_thumbnail_pass_never_steps_over_a_
// texel_on_any_canvas_umber_admits` in `canvas.rs` computes the real bound from
// those constants and reads this line back out of the shader text, which is the
// only way a WGSL constant can be checked against a Rust one.
//
// **What it costs, said out loud, because the ceiling moved rather than being
// removed.** Wherever the clamp used to arm the loop now runs to completion, so
// the worst pass is four to six times the work it was: at 32768 the bounds pass
// reads about 1.07 G texels and the picture pass about 1.52 G, against a
// previous cap near 268 M each. That is the honest cost of a thumbnail that
// tells the truth, and it is paid once per job rather than per frame — but it is
// recorded into the *frame's* encoder, so on a canvas of that size it is a hitch
// somebody would feel. If it ever bites, the answer is a compute reduction or a
// mip chain, not a step: `content_rect` needs the maximum it is denied by one.
const MAX_TAPS: i32 = 1024;

@group(0) @binding(0) var<uniform> u: Thumb;
@group(0) @binding(1) var layers: texture_2d_array<f32>;
// Where each of each slot's tiles lives. `layers` is a tile atlas, so a document
// texel of a slot is `tile_load`'s to find. See `tiles.wgsl`, concatenated in
// front of this file.
@group(0) @binding(2) var page_table: texture_2d_array<u32>;

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
                // A thumbnail is only ever taken of a *layer*, never of a mask,
                // so the empty value is transparent — which is also what the
                // bounds test above already substitutes outside the canvas.
                texel = tile_load(layers, page_table, i32(u.slot), at, vec4<f32>(0.0));
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
