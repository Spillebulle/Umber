// Bakes the finished stroke into the layer, once, at pointer-up.
//
// Only the stroke's damaged rectangle is drawn. Paint and erase share this
// shader but are drawn with different blend state (see `canvas.rs`): erase
// zeroes the source factor so the layer's alpha is scaled down rather than
// accumulated. Emitting zero RGB here is what makes that reduce to
// layer * (1 - cov).
//
// # Two entry points, and why the second one exists
//
// `fs` hands the stroke to the fixed-function blender, which is source-over and
// therefore Normal. That is every stroke Umber has ever committed and it is
// untouched.
//
// A brush carrying any other blend mode cannot be committed that way: Multiply
// needs the pixel underneath, and the fixed-function blender's factors cannot
// express `B(Cb, Cs)`. `fs_blend` computes the whole result itself, out of the
// same `composite_over` in `blend.wgsl` that the preview calls, and is drawn
// with `blend: None`. Its backdrop is a **copy** of the layer over the piece
// being committed, because a colour attachment may not also be sampled — the
// same constraint `flip.wgsl` works around, for the same reason.

struct Commit {
    // The piece being drawn, in document pixels. For `fs` this spans the whole
    // damaged rectangle and the scissor decides which of it survives; for
    // `fs_blend` it is the piece itself, because the backdrop copy is
    // piece-shaped and `rect_min` is what maps a fragment into it.
    rect_min: vec2<f32>,
    rect_max: vec2<f32>,
    doc_size: vec2<f32>,
    _pad0: vec2<f32>,
    color: vec4<f32>,   // linear RGB, .a = stroke opacity
    mode: u32,          // 0 = paint, 1 = erase
    // Non-zero when the stroke carries a colour per dab — a smudging brush —
    // and `color.rgb` is therefore not the whole story. Scalar padding, not a
    // vec3: see the uniform-layout note in CLAUDE.md.
    per_dab_color: u32,
    // The brush's blend mode, in `umber_core::BlendMode`'s numbering. Read by
    // `fs_blend` alone: `fs` is the Normal path and the blender is doing it.
    // Took the place of one of the two padding words already here, so the block
    // is the size it always was.
    blend: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<uniform> u: Commit;
@group(0) @binding(1) var stroke_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
// Per-dab colour, premultiplied linear RGBA. A 1x1 placeholder unless this
// stroke smudges.
@group(0) @binding(3) var stroke_color_tex: texture_2d<f32>;
// The layer's own pixels over the piece being committed, copied out before the
// pass because the layer is this pass's colour attachment. Bound — and declared
// in the pipeline layout — only for the blended pipeline; `fs` never reads it.
@group(0) @binding(4) var backdrop_tex: texture_2d<f32>;

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

// The stroke's colour at this fragment.
//
// MUST stay identical to `stroke_rgb` in `composite.wgsl`. The preview and the
// committed result are two renderings of the same thing, and any difference
// between them shows up as the stroke visibly jumping at pointer-up.
fn stroke_rgb(uv: vec2<f32>) -> vec3<f32> {
    let picked = textureSampleLevel(stroke_color_tex, samp, uv, 0.0);
    // Un-premultiply. Where a smudging stroke has laid nothing down yet the
    // sample is all zeroes, and dividing would be a NaN, so the uniform colour
    // stands in — which is also what a dab with no pickup deposits.
    let smudged = select(u.color.rgb, picked.rgb / max(picked.a, 1e-4), picked.a > 1e-4);
    return select(u.color.rgb, smudged, u.per_dab_color != 0u);
}

// The premultiplied source this stroke lays down at this fragment.
//
// Byte for byte what `composite.wgsl` builds as `s`: the same coverage sample
// times the same once-applied stroke opacity, times the same `stroke_rgb`.
//
// The claim is about `s` and not about the whole blend. The *destination*
// differs: the preview samples the layer bilinearly at screen resolution while
// this reads one texel, so under magnification the two are blending against
// slightly different backdrops. Filtering does not commute with a non-linear
// blend, which makes that difference marginally larger for Multiply than for
// Normal. It is sub-visual, it predates blend modes, and closing it would mean
// resampling the layer to match — which is why what is promised here is the
// source rather than the result.
fn stroke_src(uv: vec2<f32>) -> vec4<f32> {
    let cov = textureSampleLevel(stroke_tex, samp, uv, 0.0).r * u.color.a;
    return vec4<f32>(stroke_rgb(uv) * cov, cov);
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.doc / u.doc_size;

    if (u.mode == 0u) {
        return stroke_src(uv);
    }
    // Erase: no colour, and the blend state does the rest.
    let cov = textureSampleLevel(stroke_tex, samp, uv, 0.0).r * u.color.a;
    return vec4<f32>(0.0, 0.0, 0.0, cov);
}

// The same commit for a brush carrying a blend mode other than Normal.
//
// Drawn with `blend: None`, so what this returns is what the layer holds. The
// destination comes out of `backdrop_tex` rather than the attachment, and it is
// read with `textureLoad` at an integer texel — not a sampler. The copy is 1:1
// with the piece and the quad covers it exactly, so a filtered tap would be a
// point sample with a chance of rounding into its neighbour; the same argument
// `flip.wgsl` makes for reading with integers.
//
// `mode` is not consulted: an eraser never reaches this pipeline, because a
// blend mode means nothing without a colour to combine — see
// `Brush::blend_applies`.
@fragment
fn fs_blend(in: VsOut) -> @location(0) vec4<f32> {
    let uv = in.doc / u.doc_size;
    let src = stroke_src(uv);
    // The quad spans exactly the piece the backdrop was copied from, so this
    // is in `0 .. piece size` and never negative.
    let texel = vec2<i32>(floor(in.doc - u.rect_min));
    let dst = textureLoad(backdrop_tex, texel, 0);
    return composite_over(dst, src, u.blend);
}
