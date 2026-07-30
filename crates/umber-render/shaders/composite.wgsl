// Draws the visible frame: layer + in-progress stroke, over a checkerboard.
//
// The stroke maths here must match commit.wgsl exactly. If they diverge, the
// stroke visibly jumps at pointer-up when the preview is replaced by the
// committed result.

struct View {
    // doc = screen * scale + offset
    scale: vec2<f32>,
    offset: vec2<f32>,
    doc_size: vec2<f32>,
    viewport: vec2<f32>,
    // Linear RGB; .a carries the stroke opacity.
    color: vec4<f32>,
    mode: u32,          // 0 = paint, 1 = erase
    checker: f32,       // checker square size, screen px
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> v: View;
@group(0) @binding(1) var layer_tex: texture_2d<f32>;
@group(0) @binding(2) var stroke_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

// The surface is a linear (non-sRGB) format so egui can write its already
// gamma-encoded colours without the hardware encoding them twice. That makes
// encoding the canvas our job.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let lower = c * 12.92;
    let higher = 1.055 * pow(max(c, vec3<f32>(0.0)), vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, c <= vec3<f32>(0.0031308));
}

fn linear_to_srgb_inverse(c: vec3<f32>) -> vec3<f32> {
    let lower = c / 12.92;
    let higher = pow((max(c, vec3<f32>(0.0)) + 0.055) / 1.055, vec3<f32>(2.4));
    return select(higher, lower, c <= vec3<f32>(0.04045));
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

    // Backdrop outside the canvas. Written straight to the surface, so these
    // are already-encoded display values rather than linear ones.
    if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) {
        return vec4<f32>(0.13, 0.13, 0.15, 1.0);
    }

    // textureSampleLevel rather than textureSample: sampling after a branch is
    // non-uniform control flow, which implicit-derivative sampling forbids.
    var layer = textureSampleLevel(layer_tex, samp, uv, 0.0);
    let cov = textureSampleLevel(stroke_tex, samp, uv, 0.0).r * v.color.a;

    if (v.mode == 0u) {
        // Premultiplied source-over.
        let src = vec4<f32>(v.color.rgb * cov, cov);
        layer = src + layer * (1.0 - src.a);
    } else {
        layer = layer * (1.0 - cov);
    }

    // Checker greys are specified as display values and converted to linear so
    // the composite below happens in the same space as the layer.
    let ch = (floor(screen.x / v.checker) + floor(screen.y / v.checker)) % 2.0;
    let backdrop = linear_to_srgb_inverse(mix(vec3<f32>(0.88), vec3<f32>(0.78), ch));

    // `layer` is premultiplied, so compositing over the backdrop is an add.
    let rgb = layer.rgb + backdrop * (1.0 - layer.a);
    return vec4<f32>(linear_to_srgb(rgb), 1.0);
}
