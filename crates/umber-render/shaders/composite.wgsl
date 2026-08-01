// Draws the visible frame: the whole layer stack plus the in-progress stroke,
// over the document background, over a checkerboard.
//
// The entire stack composites in ONE pass. Layers live in a texture array and
// this shader walks them bottom to top, so adding layers costs a loop iteration
// rather than a render pass and a full-screen bandwidth round trip each. The
// document background joins that pass rather than getting one of its own: it is
// one multiply-add after the loop, and doing it here is what makes the PNG
// export, the eyedropper and a smudging brush's canvas probe — all of which
// reuse this shader — see the same thing the screen does, with no second copy
// of the maths to keep in step.
//
// The stroke maths here must match commit.wgsl exactly. If they diverge, the
// stroke visibly jumps at pointer-up when the preview is replaced by the
// committed result.

// Mirrored by `LayerStack::MAX` in umber-core. Raising one means raising both.
const MAX_LAYERS: u32 = 64u;

struct View {
    // doc = screen * scale + offset
    scale: vec2<f32>,
    offset: vec2<f32>,
    doc_size: vec2<f32>,
    // Screen point the camera centre sits on — the middle of the canvas
    // region, not of the window, since panels take a bite out of it.
    pivot: vec2<f32>,
    // Linear RGB; .a carries the stroke opacity.
    stroke_color: vec4<f32>,
    // Surround colour in *display* space, written straight out so it matches
    // the egui panels exactly.
    backdrop: vec4<f32>,
    // The document background, premultiplied linear, composited UNDER the
    // stack. All zeroes means transparent, and the blend below is then the
    // exact identity — which is why this needs no flag of its own.
    //
    // Not to be confused with `backdrop`, which is the surround *outside* the
    // document and is display-space.
    background: vec4<f32>,
    layer_count: u32,
    stroke_mode: u32,     // 0 = paint, 1 = erase
    active_index: u32,    // stack position receiving the stroke
    checker: f32,         // checker square size, screen px
    // 1 when rendering for export: no checkerboard, no surround, and straight
    // alpha out. Sharing the pass with the on-screen path is deliberate —
    // a separate export shader would be a second copy of the blend maths to
    // keep in step, and exports that differ from the screen are a classic bug.
    // Named with a prefix because `export` is a reserved word in WGSL.
    is_export: u32,
    // Non-zero when the stroke carries a colour per dab — a smudging brush —
    // and `stroke_color.rgb` is therefore not the whole story.
    per_dab_color: u32,
    // Two scalars, not a vec2/vec3<u32>: a vec3 carries 16-byte alignment,
    // which would push it to the next 16-byte boundary and leave the struct 16
    // bytes longer than the Rust side. Scalars are 4-aligned and pack as
    // intended.
    _pad1: u32,
    _pad2: u32,
    // Per stack position, bottom first: (opacity, blend mode, slot, visible).
    // Packed as floats to dodge std140's array-stride rules; every value is a
    // small integer or a 0..1 float, so the round trip is exact.
    layers: array<vec4<f32>, MAX_LAYERS>,
};

@group(0) @binding(0) var<uniform> v: View;
@group(0) @binding(1) var layer_tex: texture_2d_array<f32>;
@group(0) @binding(2) var stroke_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
// Per-dab colour, premultiplied linear RGBA. A 1x1 placeholder unless the
// stroke in progress smudges.
@group(0) @binding(4) var stroke_color_tex: texture_2d<f32>;

// The stroke's colour at this fragment.
//
// MUST stay identical to `stroke_rgb` in `commit.wgsl`. The preview and the
// committed result are two renderings of the same thing, and any difference
// between them shows up as the stroke visibly jumping at pointer-up.
fn stroke_rgb(uv: vec2<f32>) -> vec3<f32> {
    let picked = textureSampleLevel(stroke_color_tex, samp, uv, 0.0);
    // Un-premultiply. Where a smudging stroke has laid nothing down yet the
    // sample is all zeroes, and dividing would be a NaN, so the uniform colour
    // stands in — which is also what a dab with no pickup deposits.
    let smudged = select(v.stroke_color.rgb, picked.rgb / max(picked.a, 1e-4), picked.a > 1e-4);
    return select(v.stroke_color.rgb, smudged, v.per_dab_color != 0u);
}

// The surface is a linear (non-sRGB) format so egui can write its already
// gamma-encoded colours without the hardware encoding them twice. That makes
// encoding the canvas our job.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lower = c * 12.92;
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let lower = c / 12.92;
    let higher = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(higher, lower, c <= vec3<f32>(0.04045));
}

// Separable blend functions, operating on straight (un-premultiplied) colour.
fn blend_rgb(mode: u32, cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {
    switch mode {
        case 1u: {  // Multiply
            return cb * cs;
        }
        case 2u: {  // Screen
            return cb + cs - cb * cs;
        }
        case 3u: {  // Overlay — Hard Light with the operands swapped
            let lo = 2.0 * cb * cs;
            let hi = 1.0 - 2.0 * (1.0 - cb) * (1.0 - cs);
            return select(hi, lo, cb <= vec3<f32>(0.5));
        }
        case 4u: {  // Add
            return min(cb + cs, vec3<f32>(1.0));
        }
        default: {  // Normal
            return cs;
        }
    }
}

// W3C compositing, with both operands premultiplied:
//   Co = (1 - ab)*Sc + as*ab*B(Cb, Cs) + (1 - as)*Bc
//   ao = as + ab*(1 - as)
// For Normal this collapses to plain source-over, as it should.
fn composite_over(dst: vec4<f32>, src: vec4<f32>, mode: u32) -> vec4<f32> {
    if (src.a <= 0.0) {
        return dst;
    }
    let cs = src.rgb / src.a;
    let cb = select(vec3<f32>(0.0), dst.rgb / max(dst.a, 1e-5), dst.a > 0.0);
    let blended = blend_rgb(mode, cb, cs);

    let co = (1.0 - dst.a) * src.rgb + src.a * dst.a * blended + (1.0 - src.a) * dst.rgb;
    let ao = src.a + dst.a * (1.0 - src.a);
    return vec4<f32>(co, ao);
}

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Oversized triangle covering the viewport — cheaper than a quad and
    // avoids the diagonal seam two triangles produce.
    var pos = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 3.0, -1.0),
        vec2<f32>(-1.0,  3.0),
    );
    return vec4<f32>(pos[vi], 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) frag: vec4<f32>) -> @location(0) vec4<f32> {
    let screen = frag.xy;
    let doc = screen * v.scale + v.offset;
    let uv = doc / v.doc_size;

    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        if (v.is_export == 1u) {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
        return vec4<f32>(v.backdrop.rgb, 1.0);
    }

    // textureSampleLevel rather than textureSample: sampling inside a loop and
    // after a branch is non-uniform control flow, which implicit-derivative
    // sampling forbids.
    let coverage = textureSampleLevel(stroke_tex, samp, uv, 0.0).r;

    var acc = vec4<f32>(0.0);
    for (var i = 0u; i < v.layer_count; i = i + 1u) {
        let params = v.layers[i];
        let opacity = params.x;
        let mode = u32(params.y);
        let slot = i32(params.z);
        let visible = params.w > 0.5;

        if (!visible || opacity <= 0.0) {
            continue;
        }

        var lay = textureSampleLevel(layer_tex, samp, uv, slot, 0.0);

        // The in-progress stroke belongs to one layer, and must be blended
        // inside the stack rather than on top of the finished composite —
        // otherwise painting under a Multiply layer would preview wrongly.
        if (i == v.active_index) {
            let cov = coverage * v.stroke_color.a;
            if (v.stroke_mode == 0u) {
                let s = vec4<f32>(stroke_rgb(uv) * cov, cov);
                lay = s + lay * (1.0 - s.a);
            } else {
                lay = lay * (1.0 - cov);
            }
        }

        // Scaling a premultiplied colour by opacity is correct as-is.
        acc = composite_over(acc, lay * opacity, mode);
    }

    // The document background, under everything the stack put down. Both sides
    // are premultiplied, so source-over is an add — and with an all-zero
    // background it is `acc + 0`, exactly what this shader did before there was
    // one. It is applied before the export branch on purpose: an export of a
    // white-backed document must come out opaque white, and a transparent one
    // must still come out with its alpha, and both fall out of the same line.
    acc = vec4<f32>(
        acc.rgb + v.background.rgb * (1.0 - acc.a),
        acc.a + v.background.a * (1.0 - acc.a),
    );

    if (v.is_export == 1u) {
        // PNG wants straight alpha, so undo the premultiply. Fully transparent
        // pixels have no colour to recover and would divide by zero.
        if (acc.a <= 0.0) {
            return vec4<f32>(0.0, 0.0, 0.0, 0.0);
        }
        return vec4<f32>(linear_to_srgb(acc.rgb / acc.a), acc.a);
    }

    let ch = (floor(screen.x / v.checker) + floor(screen.y / v.checker)) % 2.0;
    let backdrop = srgb_to_linear(mix(vec3<f32>(0.88), vec3<f32>(0.78), ch));

    // `acc` is premultiplied, so compositing over the backdrop is an add.
    let rgb = acc.rgb + backdrop * (1.0 - acc.a);
    return vec4<f32>(linear_to_srgb(rgb), 1.0);
}
