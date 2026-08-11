// Moves, scales and rotates a floating region of a layer.
//
// Two passes share this file and its uniform block, because they are two halves
// of one operation and drift between them would be a mask applied in one place
// and not the other:
//
// * `fs_mask` writes a *share* into the alpha channel and nothing into the
//   colour. What it *does* is entirely the blend state's — `dst * share` lifts
//   the selected pixels into the floating copy, `dst * (1 - share)` punches the
//   hole they left. See `canvas.rs`, and see `fs_mask` for why the share is not
//   simply the selection's coverage.
// * `fs_sample` is the resampler. It walks the destination rectangle and asks
//   the **inverse** transform where each pixel came from, then takes one
//   bilinear tap — the sampler is `Linear`, so the filter is free and is the
//   same one the composite pass uses.
//
// There is no blending maths in here at all, and that is deliberate. The
// preview and the commit are the *same two passes* pointed at different
// targets (`CanvasRenderer::render_float`), so unlike the stroke there is no
// second implementation to keep in step: what the screen shows during a drag is
// what gets written when the pointer comes up, by construction.

struct Xf {
    // The rectangle being drawn, in document pixels.
    rect_min: vec2<f32>,
    rect_max: vec2<f32>,
    doc_size: vec2<f32>,
    // The inverse affine, as two columns and a translation:
    //   source = inv_x * dest.x + inv_y * dest.y + inv_t
    // Three vec2s rather than a mat2x2, because a matrix in a uniform block
    // carries a 16-byte column stride on this side and a packed Rust array does
    // not. See the uniform-layout note in CLAUDE.md.
    inv_x: vec2<f32>,
    inv_y: vec2<f32>,
    inv_t: vec2<f32>,
    // Where the selection mask is mapped to, in document pixels, and its size.
    // `(1, 1)` with no mask, so the divide below is never by zero even though
    // its result is thrown away.
    mask_min: vec2<f32>,
    mask_size: vec2<f32>,
    // Non-zero when a real mask is bound. Like the dab pass's `use_selection`
    // this cannot be folded into a placeholder: a 1x1 mask read outside its own
    // rectangle is *zero*, which would mean nothing is selected anywhere.
    use_mask: u32,
    // Scalar padding, not a vec3: see the uniform-layout note in CLAUDE.md.
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<uniform> u: Xf;
// What the pass reads. For `fs_sample` it is the floating pixels at identity:
// canvas-sized, zero outside the region that was lifted or pasted. For the two
// mask passes it is the **layer's own slice**, untouched, because the share
// they compute is a share of what is already there — and neither of their
// targets, the base and the floating copy, may be bound for sampling while it
// is a colour attachment.
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
// The selection's coverage over its own bounding rectangle, or a 1x1
// placeholder that `use_mask` keeps out of the arithmetic.
@group(0) @binding(3) var mask_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) doc: vec2<f32>,
};

// A quad over `rect_min .. rect_max` in document space. The same shape
// `commit.wgsl` uses, and for the same reason: only the damaged rectangle is
// worth rasterising.
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

// The share of what this fragment already holds that the selection lays claim
// to, as alpha. The colour is zero because both blend states this feeds zero
// the source factor — nothing here is added to the target, only scaled out of
// it.
//
// **The share is not the selection's coverage**, and taking it to be was a real
// bug: it left a one-pixel ghost of the outline behind every lift. Painting is
// clipped by the same mask — that is `dab.wgsl`'s whole job — so a pixel the
// selection half covers already holds half a stroke's alpha. Scaling that by a
// half *again* carries a quarter into the float and leaves a quarter on the
// layer, in a ring right round the selection. The mask was being applied twice.
//
// So the lift is a `min`, not a multiply. Of the alpha `a` that is there, the
// part lying inside the selection can be no more than the selection's own
// coverage `m`, and this takes it to be exactly `min(a, m)`: the paint sits in
// the selected part of the pixel wherever it can. That is precisely true for
// anything painted through this selection, and it can never take more than is
// there. Three cases, and it is the same expression for all of them:
//
// * `a == m` — painted through this selection. The float takes all of it and
//   the hole is exactly zero. No ghost.
// * `a == 1` — opaque pixels lassoed out of a picture. `min(a, m)` is `m`, so
//   this is the old behaviour unchanged: the moved edge carries the selection's
//   own antialiasing and the hole carries its complement.
// * `a < m` — the content's own soft edge inside the selection. The float takes
//   it whole, with that falloff intact rather than multiplied by the mask's.
//
// One number drives both passes — the float is scaled by the share and the hole
// by its complement — so the two cannot disagree about where the paint went,
// for the same reason `render_float` is one function called twice.
@fragment
fn fs_mask(in: VsOut) -> @location(0) vec4<f32> {
    let uv = (in.doc - u.mask_min) / u.mask_size;
    // Outside the mask's own rectangle is outside the selection, decided
    // arithmetically rather than by clamping — clamp-to-edge would smear the
    // boundary texels across the canvas. Same rule as `dab.wgsl`.
    let inside = uv.x >= 0.0 && uv.y >= 0.0 && uv.x < 1.0 && uv.y < 1.0;
    let sampled = select(0.0, textureSampleLevel(mask_tex, samp, uv, 0.0).r, inside);
    let cov = select(1.0, sampled, u.use_mask != 0u);
    // Alpha survives the sRGB view unchanged — the transfer function is on the
    // colour channels only — so this is the same linear 0..1 the coverage is.
    //
    // **`textureLoad`, not a sampler**, and that is the same argument
    // `fs_blend` and `flip.wgsl` make: this quad covers the rectangle 1:1, so a
    // fragment centre lands exactly on a texel centre and a bilinear tap is a
    // point sample with a chance of rounding into its neighbour. It is also what
    // makes this independent of how large `src_tex` is — the layer's slice is a
    // *page* of the tile atlas now, which is the canvas rounded up to whole
    // tiles, so `in.doc / u.doc_size` stopped being where the texel was.
    let a = textureLoad(src_tex, vec2<i32>(in.doc), 0).a;
    // `max` in the divisor rather than a branch on `a > 0`: `min(a, cov)` is
    // never above `a` and `a` is never above the divisor, so the share stays
    // within 0..1, and a bare pixel gives `0 / eps` rather than a NaN that
    // would carry across the whole quad. With no selection `cov` is 1.0 and the
    // share is exactly 1.0, which is what lets the keep pass be skipped whole.
    let share = min(a, cov) / max(a, 1.0e-6);
    return vec4<f32>(0.0, 0.0, 0.0, share);
}

// One bilinear tap through the inverse transform.
@fragment
fn fs_sample(in: VsOut) -> @location(0) vec4<f32> {
    let src = u.inv_x * in.doc.x + u.inv_y * in.doc.y + u.inv_t;
    // The floating copy is zero everywhere the region is not, so a sample that
    // lands beside it correctly fades to transparent — which is what gives the
    // moved edge its antialiasing. Beyond the *canvas* there is nothing to fade
    // into and the sampler clamps to the edge instead, which would streak the
    // border texel out across the destination. So say zero explicitly.
    if (src.x < 0.0 || src.y < 0.0 || src.x > u.doc_size.x || src.y > u.doc_size.y) {
        return vec4<f32>(0.0, 0.0, 0.0, 0.0);
    }
    return textureSampleLevel(src_tex, samp, src / u.doc_size, 0.0);
}
