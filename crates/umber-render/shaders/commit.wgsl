// Bakes the finished stroke into the layer, once, at pointer-up.
//
// Only the stroke's damaged rectangle is drawn. Paint and erase share this
// shader but are drawn with different blend state (see `canvas.rs`): erase
// zeroes the source factor so the layer's alpha is scaled down rather than
// accumulated. Emitting zero RGB here is what makes that reduce to
// layer * (1 - cov).

struct Commit {
    rect_min: vec2<f32>,
    rect_max: vec2<f32>,
    doc_size: vec2<f32>,
    _pad0: vec2<f32>,
    color: vec4<f32>,   // linear RGB, .a = stroke opacity
    mode: u32,          // 0 = paint, 1 = erase
    _pad1: f32,
    _pad2: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: Commit;
@group(0) @binding(1) var stroke_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) doc: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(0.0, 0.0),
        vec2<f32>(1.0, 0.0),
        vec2<f32>(0.0, 1.0),
        vec2<f32>(1.0, 1.0),
    );
    let c = corners[vi];
    let doc = mix(u.rect_min, u.rect_max, c);
    let ndc = vec2<f32>(
        doc.x / u.doc_size.x * 2.0 - 1.0,
        1.0 - doc.y / u.doc_size.y * 2.0,
    );

    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.doc = doc;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.doc / u.doc_size;
    let cov = textureSampleLevel(stroke_tex, samp, uv, 0.0).r * u.color.a;

    if (u.mode == 0u) {
        return vec4<f32>(u.color.rgb * cov, cov);
    }
    return vec4<f32>(0.0, 0.0, 0.0, cov);
}
