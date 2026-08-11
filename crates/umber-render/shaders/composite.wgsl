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

// How many *draws* the uniform arrays below can carry, mirrored by `MAX_DRAWS`
// in `canvas.rs`. Raising one means raising both, and a CPU test parses this
// line to say so.
//
// This is deliberately not `LayerStack::MAX`, which is 64 and bounds *stack
// entries*. A draw is not a stack entry: a layer's effects each composite as a
// draw of their own, so one entry can produce several. The difference, 127, is
// the document's effect-draw budget — derived on the Rust side from the layer
// array's 256-slice ceiling, because an effect draw reads an effect slice.
//
// The loop below is bounded by `layer_count`, never by this, so raising it
// costs uniform bytes and the upload and nothing per fragment.
const MAX_DRAWS: u32 = 191u;

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
    // Non-zero when the stroke in flight is being painted into the active
    // layer's *mask* rather than into its pixels. See `fs` for the one place
    // that reads it, and for why the maths there has to match `commit.wgsl`.
    stroke_on_mask: u32,
    // The *brush's* blend mode, in the same numbering `layers[i].y` uses and
    // evaluated by the same `composite_over`. Zero — Normal — is the path every
    // stroke took before brushes had one, and `fs` keeps it as a separate line
    // rather than routing it through the general form; see there.
    //
    // A scalar, not a vec2/vec3<u32>: a vec3 carries 16-byte alignment, which
    // would push it to the next 16-byte boundary and leave the struct 16 bytes
    // longer than the Rust side. Scalars are 4-aligned and pack as intended.
    // This one took the place of the padding word that was already here, so
    // the block is the same size it always was.
    stroke_blend: u32,
    // Per draw, bottom first: (opacity, blend mode, slot, visible). Packed as
    // floats to dodge std140's array-stride rules; every value is a small
    // integer or a 0..1 float, so the round trip is exact.
    layers: array<vec4<f32>, MAX_DRAWS>,
    // The rest of each draw: (mask slot, has mask, clipped, unused).
    //
    // A second array rather than four more bits packed into `layers[i].w`. The
    // mask *slot* does not fit in a flag, and a bit field would have to be
    // unpacked identically here and in Rust — one more pair of statements that
    // can drift, for a kilobyte of a uniform buffer whose guaranteed minimum
    // size is sixty-four.
    //
    // `mask slot` is the layer's own slot where there is no mask, so the index
    // is always inside the array whether or not the sample is taken.
    extra: array<vec4<f32>, MAX_DRAWS>,
};

@group(0) @binding(0) var<uniform> v: View;
// The tile atlas. Its slices are *pages*, not layers: where a slot's texels are
// is `page_table`'s to say. See `tiles.wgsl`, concatenated in front of this file
// along with `blend.wgsl`.
@group(0) @binding(1) var layer_tex: texture_2d_array<f32>;
@group(0) @binding(2) var stroke_tex: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
// Per-dab colour, premultiplied linear RGBA. A 1x1 placeholder unless the
// stroke in progress smudges.
@group(0) @binding(4) var stroke_color_tex: texture_2d<f32>;
// Where each of each slot's tiles lives, or that it lives nowhere.
@group(0) @binding(5) var page_table: texture_2d_array<u32>;

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

// `blend_rgb` and `composite_over` used to live here. They are now in
// `blend.wgsl`, concatenated in front of this file — see that file for why, and
// `Shared::new` for how.

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

    // The layer array is a tile atlas, so its texels are not at `uv` and there
    // is nothing to hand a sampler. `tile_bilinear` is the tap, reconstructed
    // through the page table; the stroke scratch above is still a plain
    // canvas-sized texture and is still sampled.
    let doc_texels = vec2<i32>(v.doc_size);

    var acc = vec4<f32>(0.0);
    // What a clipped layer is bounded by: the alpha of the nearest *unclipped*
    // layer below it, after that layer's own mask and its own wet stroke. One
    // running value is the whole of how a run of clipped layers all answer to
    // the same base, which is what every other application means by the word.
    //
    // Zero to begin with, so a clipped layer with nothing unclipped beneath it
    // shows nothing. Inventing a 1.0 there would make the flag mean something
    // different at the bottom of the stack than it means anywhere else.
    var clip_alpha = 0.0;
    for (var i = 0u; i < v.layer_count; i = i + 1u) {
        let params = v.layers[i];
        let opacity = params.x;
        let mode = u32(params.y);
        let slot = i32(params.z);
        let visible = params.w > 0.5;

        let extra = v.extra[i];
        let mask_slot = i32(extra.x);
        let has_mask = extra.y > 0.5;
        let clipped = extra.z > 0.5;

        // Sampled before the visibility test now, because a hidden layer still
        // has to bound whatever is clipped to it — to nothing. Two fetches for
        // a layer that contributes no pixels is the price of that being one
        // rule rather than two.
        // A layer's empty value is transparent black, which is what a tile
        // nobody has painted into reads as — byte for byte what a dense slice
        // held there.
        var lay = tile_bilinear(layer_tex, page_table, slot, doc, doc_texels, vec4<f32>(0.0));
        let stroke_here = i == v.active_index;
        let cov = coverage * v.stroke_color.a;

        // The mask hides and reveals by multiplying the layer's premultiplied
        // colour, which is exactly "it multiplies the alpha" written for both
        // halves at once. A layer without one multiplies by an exact 1.0 —
        // `no_mask_is_the_exact_identity` pins that.
        //
        // A branch, deliberately, where the tip and the paper use a `select`.
        // Those two fold into a multiply by one and cost a fetch that was
        // happening anyway; this one is a whole extra tap of the atlas, on the
        // pass that runs every frame for every layer. `has_mask` comes out of a
        // uniform, so the branch is uniform across the draw and an unmasked
        // document really does pay nothing.
        //
        // **A mask's empty value is white, not zero**, and that is the one place
        // the substitution is not "what a cleared slice held". A mask multiplies
        // the layer's alpha and a mask nobody has painted on reveals everything,
        // so an absent tile has to read 1.0 — taking it for zero hides the layer
        // everywhere nobody painted, which is the bug `clipstudio.rs` records
        // fixing on the import side, in the same format at the same block size.
        var m = 1.0;
        if (has_mask) {
            m = tile_bilinear(
                layer_tex, page_table, mask_slot, doc, doc_texels, vec4<f32>(1.0)
            ).r;
        }
        // A stroke on the mask previews by blending into `m` here. THIS MUST
        // STAY IDENTICAL to what `commit.wgsl` writes into the mask slice —
        // which is the ordinary paint blend, `src + dst * (1 - src.a)`, read
        // on one channel because a mask slice is written with the greyscale
        // the editor forces on a mask stroke. Any difference between the two
        // shows up as the mask jumping at pointer-up.
        if (stroke_here && v.stroke_on_mask != 0u && has_mask) {
            m = v.stroke_color.r * cov + m * (1.0 - cov);
        }

        // The in-progress stroke belongs to one layer, and must be blended
        // inside the stack rather than on top of the finished composite —
        // otherwise painting under a Multiply layer would preview wrongly.
        if (stroke_here && v.stroke_on_mask == 0u) {
            if (v.stroke_mode == 0u) {
                let s = vec4<f32>(stroke_rgb(uv) * cov, cov);
                // Normal is written out rather than passed to `composite_over`
                // with mode 0, and that is not a duplicate of it. The general
                // form reduces to exactly this line in exact arithmetic and not
                // in floating point — it divides the source by its own alpha
                // and multiplies it back — and what the *commit* does for a
                // Normal stroke is the fixed-function blender computing
                // precisely `src + dst * (1 - src.a)`. So this line is what
                // matches the commit, and matching the commit is the whole
                // point. Anything else goes through `composite_over`, which is
                // the same function `commit.wgsl`'s blended path calls.
                if (v.stroke_blend == 0u) {
                    lay = s + lay * (1.0 - s.a);
                } else {
                    lay = composite_over(lay, s, v.stroke_blend);
                }
            } else {
                // An eraser deposits no colour, so there is nothing for a mode
                // to be a mode of — `Brush::blend_applies` says so and the
                // editor never sends one down this path. See that method.
                lay = lay * (1.0 - cov);
            }
        }

        lay = lay * m;

        if (clipped) {
            lay = lay * clip_alpha;
        } else {
            clip_alpha = select(0.0, lay.a, visible && opacity > 0.0);
        }

        if (!visible || opacity <= 0.0) {
            continue;
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
