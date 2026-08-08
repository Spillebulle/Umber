// Bakes one layer effect — a drop shadow, or the outline the interface calls
// Stroke — into a slice of the layer texture array.
//
// `docs/layer-effects.md` is the design. The whole of what this file exists to
// serve is one sentence of it: **an effect is an ordinary `LayerDraw` pointing
// at an ordinary layer slice**, so `composite.wgsl` is not touched, and the four
// things that reuse the composite pass (`export_rgba`, `pick_colour`,
// `probe_canvas`, the autosave's capture) needed nothing. What that costs is
// this file: the effect's pixels have to exist before the composite runs.
//
// # One derived quantity, two effects
//
// A stroke and a drop shadow are the same pipeline with different parameters,
// and saying so is what keeps this from becoming two shaders that round their
// edges differently. Every effect is built out of the layer's coverage — its
// alpha, after its mask, after the wet stroke — by at most four steps:
//
//   grow     `fs_grow`     signed distance, thresholded at the spread
//   soften   `fs_down` + `fs_box` x4    a tent, on a downsample
//   offset   `fs_resolve`  read the field where the shadow came from
//   confine  `fs_resolve`  multiply by `1 - coverage`, for an *outer* effect
//
// The confinement is the one asymmetry and it is deliberate: an **outer**
// effect bakes its knockout here, because doing it at composite time would need
// an *inverse* clip that `composite.wgsl` has no notion of; an **inner** effect
// does nothing here at all and is confined by `LayerDraw::clipped`, which
// already means "bounded by the alpha of the nearest unclipped layer below".
// That asymmetry is the kind of thing that gets forgotten and reintroduced as a
// uniform, so it is written here as well as in the design.
//
// # The distance field is a jump flood, and only a jump flood
//
// `fs_seed` and `fs_step`. `ceil(log2(r)) + 1` full-screen passes, independent
// of the radius after the log. The alternatives were measured and refused —
// §3.1a — and the one that looks cheapest is the one to be most careful of: a
// separable tent has *square* support, so blur-and-threshold cannot get a corner
// and a diagonal right at the same kernel width, and its shape error crosses
// three pixels between 32 and 48 px of radius. There is no window in which it is
// both cheaper and better, so there is deliberately no second path here for a
// small radius.
//
// # Every intermediate is linear
//
// The coverage and blur targets are `R8Unorm`, which carries no transfer
// function. That matters most between the two box passes of one axis: a
// separable blur that landed its horizontal pass in an sRGB target and read it
// back for the vertical pass would have quantised through a gamma curve in the
// middle, and the result is not the blur of anything. Only `fs_resolve`'s target
// is sRGB, because that one *is* a layer slice and the composite decodes it —
// the same round trip a layer's own pixels take.

struct Cfg {
    // The effect's colour, premultiplied linear, alpha 1. Multiplied by the
    // coverage this file works out; the effect's *opacity* is the draw's and is
    // applied by `composite.wgsl`, so dragging that slider costs no rebake.
    tint: vec4<f32>,
    // Size of the *target* region, in texels. `fs_box` and `fs_down` render into
    // a viewport smaller than their attachment, so this is the viewport.
    size: vec2<f32>,
    // Size of the region of `src` that holds anything, in texels. Different from
    // `size` on exactly the two passes that change resolution: `fs_down` reads
    // `down` times this and `fs_resolve` reads a `down`-times-smaller field.
    src_size: vec2<f32>,
    // Where the effect is displaced to, in full-resolution document texels,
    // y-down. `fs_resolve` reads the field at `p - offset`, which moves the mark
    // *to* `+offset`. `umber_core::Effect::offset` is the one place the angle
    // convention is worked out.
    offset: vec2<f32>,
    // Texel step per tap for a box pass: `(1, 0)` or `(0, 1)`.
    step: vec2<f32>,
    // Box half-width in texels of the target. Two boxes per axis make a tent,
    // which is the kernel `umber-core::selection`'s feather uses — matching it
    // is free and stops a shadow of radius 8 and a feather of radius 8 falling
    // off differently.
    radius: i32,
    // How much smaller the blur ran than the canvas: 1 or 4. Read by
    // `fs_resolve` to decide how far apart its bilinear taps are, and by
    // `fs_down` as its own reduction.
    down: i32,
    // How far the shape is grown before it is softened, in texels.
    spread: f32,
    // Non-zero for an **inner** effect: no knockout, and `fs_grow` returns the
    // band alone rather than the union of the band and the coverage.
    inner: u32,
    // Which slice of the layer array holds the pixels, and which holds the mask.
    slot: i32,
    mask_slot: i32,
    has_mask: u32,
    // Non-zero when the stroke in flight belongs to this layer, in which case
    // `fs_extract` has to fold the scratch in — that is what makes a shadow
    // follow the brush instead of snapping into place at pointer-up.
    stroke_here: u32,
    stroke_mode: u32,        // 0 = paint, 1 = erase
    stroke_opacity: f32,
    stroke_on_mask: u32,
    // The stroke colour's red channel, which is what a stroke on a mask writes.
    stroke_gray: f32,
    // Non-zero when the shape is dilated at all. Zero is the exact identity:
    // `fs_grow` hands the coverage straight back, keeping its antialiasing,
    // where a threshold of a distance field would replace it with a staircase.
    grow: u32,
    // Jump-flood step, in texels.
    k: i32,
    // Non-zero to seed the flood on the **complement** of the coverage, which
    // turns the field into an *inward* distance. That is the whole of how an
    // inside outline is drawn without a second field or a signed one.
    invert: u32,
};

@group(0) @binding(0) var<uniform> c: Cfg;
// The layer array, read by `fs_extract` alone.
@group(0) @binding(1) var layers: texture_2d_array<f32>;
// Whatever the previous pass wrote — the wet stroke's scratch for `fs_extract`,
// a coverage field for everything else.
@group(0) @binding(2) var src: texture_2d<f32>;
// The layer's own coverage, held throughout so `fs_grow` and `fs_resolve` can
// reach it while reading something else.
@group(0) @binding(3) var cov: texture_2d<f32>;
@group(0) @binding(4) var seeds: texture_2d<u32>;

// No seed. 65535 rather than a sentinel of our own because that is what an
// `Rg16Uint` texel can hold, and `max_texture_dimension_2d` keeps every canvas
// Umber allows four times below it — a coordinate can never collide with this.
const NONE: u32 = 65535u;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle: cheaper than a quad and with no diagonal seam.
    var p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(3.0, -1.0), vec2<f32>(-1.0, 3.0),
    );
    return vec4<f32>(p[vi], 0.0, 1.0);
}

// Read one texel of a coverage field. **Outside the field is zero, not the
// nearest edge texel**, and that is the rule rather than a detail.
//
// Clamp-to-edge is what a `textureLoad` most naturally spells and it is wrong
// here in exactly the way it is wrong for the selection's feather, whose docs say
// so: "outside the canvas counts as unselected, so a selection against the
// document edge fades at it, as Photoshop's and GIMP's do". Replicated instead, a
// box pass near the border sums the border row over and over, so a layer running
// to the edge of the canvas — a background wash, a panel of flat colour — keeps a
// shadow at full strength along that edge rather than falling off. Matching the
// feather's kernel and then not matching its boundary would be the worse half of
// both.
//
// It is also what a displaced read wants: `fs_resolve` samples the field at
// `p - offset`, which for a shadow thrown outward runs off the field, and
// clamping there smears the border column across the margin.
fn at(t: texture_2d<f32>, p: vec2<i32>, lim: vec2<i32>) -> f32 {
    if (any(p < vec2<i32>(0)) || any(p >= lim)) {
        return 0.0;
    }
    return textureLoad(t, p, 0).r;
}

fn coverage_at(p: vec2<i32>) -> f32 {
    return at(cov, p, vec2<i32>(c.size));
}

// The coverage every effect on this layer derives from.
//
// **This is the one place in the bake that has to agree with `composite.wgsl`**,
// and it agrees about *alpha* only — there is no colour here, because an effect
// is a shape wearing a colour of its own. The three lines it mirrors are the
// mask multiply, the wet stroke's source-over and the eraser's complement; the
// blend mode is deliberately absent, because `composite_over`'s alpha is plain
// source-over for every mode, so a brush carrying Multiply deposits exactly the
// coverage a Normal one does.
//
// `a_live_stroke_bakes_the_shadow_the_commit_would` is what stops the two
// drifting: it bakes mid-stroke, commits, bakes again, and compares.
@fragment
fn fs_extract(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let lim = vec2<i32>(c.size);
    let p = clamp(vec2<i32>(f.xy), vec2<i32>(0), lim - vec2<i32>(1));
    var a = textureLoad(layers, p, c.slot, 0).a;
    var m = 1.0;
    if (c.has_mask != 0u) {
        m = textureLoad(layers, p, c.mask_slot, 0).r;
    }
    if (c.stroke_here != 0u) {
        let s = at(src, p, lim) * c.stroke_opacity;
        if (c.stroke_on_mask != 0u) {
            if (c.has_mask != 0u) {
                m = c.stroke_gray * s + m * (1.0 - s);
            }
        } else if (c.stroke_mode == 0u) {
            a = s + a * (1.0 - s);
        } else {
            a = a * (1.0 - s);
        }
    }
    return a * m;
}

// A texel of the shape seeds itself; everything else starts unclaimed.
//
// With `invert` the test is the other way round and the field that grows is the
// distance to the nearest *uncovered* texel — the inward distance, which is what
// an inside outline is a band of.
@fragment
fn fs_seed(@builtin(position) f: vec4<f32>) -> @location(0) vec2<u32> {
    let p = vec2<i32>(f.xy);
    let a = coverage_at(p);
    let hit = select(a > 0.5, a <= 0.5, c.invert != 0u);
    if (hit) {
        return vec2<u32>(u32(p.x), u32(p.y));
    }
    return vec2<u32>(NONE, NONE);
}

// One flood step: nine candidates at +-k, keep the nearest seed.
@fragment
fn fs_step(@builtin(position) f: vec4<f32>) -> @location(0) vec2<u32> {
    let lim = vec2<i32>(c.size) - vec2<i32>(1);
    let p = vec2<i32>(f.xy);
    var best = vec2<u32>(NONE, NONE);
    var bestd = 3.4e38;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let q = clamp(p + vec2<i32>(x, y) * c.k, vec2<i32>(0), lim);
            let s = textureLoad(seeds, q, 0).xy;
            if (s.x != NONE) {
                let d = distance(vec2<f32>(p), vec2<f32>(vec2<i32>(s)));
                if (d < bestd) {
                    bestd = d;
                    best = s;
                }
            }
        }
    }
    return best;
}

// The shape the effect is a picture of: the coverage, grown by `spread`.
//
// **A spread of zero is the exact coverage, antialiasing and all**, which is why
// this reads a flag rather than trusting the arithmetic to degenerate. It does
// not degenerate: a threshold of the distance field replaces a soft edge with a
// staircase, because a seeded texel's distance is the same whether it was 51%
// covered or 100%.
//
// Half a texel is subtracted because a seed is the *centre* of a covered texel
// and the edge it stands for is half a texel nearer.
@fragment
fn fs_grow(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let p = vec2<i32>(f.xy);
    let inside = coverage_at(p);
    if (c.grow == 0u) {
        return inside;
    }
    let s = textureLoad(seeds, clamp(p, vec2<i32>(0), vec2<i32>(c.size) - vec2<i32>(1)), 0).xy;
    if (s.x == NONE) {
        return 0.0;
    }
    let d = distance(vec2<f32>(p), vec2<f32>(vec2<i32>(s))) - 0.5;
    let band = 1.0 - smoothstep(c.spread - 0.5, c.spread + 0.5, d);
    // An inner effect is a band and nothing else: the coverage it sits inside is
    // what `LayerDraw::clipped` bounds it by, and unioning it in here would fill
    // the whole shape.
    return select(max(inside, band), band, c.inner != 0u);
}

// 4x box downsample, so the tent that follows runs on a sixteenth of the texels
// at a quarter of the radius. Sixteen loads, once.
@fragment
fn fs_down(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let lim = vec2<i32>(c.src_size);
    let p = vec2<i32>(f.xy) * c.down;
    var sum = 0.0;
    for (var y = 0; y < c.down; y = y + 1) {
        for (var x = 0; x < c.down; x = x + 1) {
            sum = sum + at(src, p + vec2<i32>(x, y), lim);
        }
    }
    return sum / f32(c.down * c.down);
}

// One box pass. `2r + 1` taps: a fragment shader has no running sum, which is
// what makes the cost scale with the radius and the downsample above necessary.
@fragment
fn fs_box(@builtin(position) f: vec4<f32>) -> @location(0) f32 {
    let lim = vec2<i32>(c.size);
    let p = vec2<i32>(f.xy);
    let d = vec2<i32>(c.step);
    var sum = 0.0;
    for (var i = -c.radius; i <= c.radius; i = i + 1) {
        sum = sum + at(src, p + d * i, lim);
    }
    return sum / f32(2 * c.radius + 1);
}

// Bilinear read of `src`, by hand.
//
// Four loads and two mixes rather than a sampler, so this pass needs no sampler
// in its layout and the uint seed texture beside it needs no argument about
// filtering support. At `down = 1` and an integer offset it lands exactly on one
// texel, which is what an outline needs.
fn bilinear(p: vec2<f32>) -> f32 {
    let lim = vec2<i32>(c.src_size);
    // **`p / down - 0.5`, and not `(p - 0.5) / down`.** `fs_down` writes small
    // texel `n` from full-resolution texels `[n·down, n·down + down)`, so what it
    // holds is centred at full coordinate `n·down + down/2` — which means the
    // small-space coordinate of a full-resolution position `p` is `p/down - 0.5`.
    // The other spelling is `p/down - 0.5/down`, which is right at `down == 1` and
    // reads three eighths of a small texel off at 4: a shadow displaced a pixel
    // and a half up and to the left whatever its angle, on the downsampled path
    // alone. The coincidence at `down == 1` is what hid it, and no test ran the
    // other path until one did — `a_softened_shadow_is_mirrored_about_both_axes`
    // now sweeps both, and a diagonal shift is exactly what a mirror test sees.
    let uv = p / f32(c.down) - vec2<f32>(0.5);
    let base = vec2<i32>(floor(uv));
    let fr = fract(uv);
    let a00 = at(src, base + vec2<i32>(0, 0), lim);
    let a10 = at(src, base + vec2<i32>(1, 0), lim);
    let a01 = at(src, base + vec2<i32>(0, 1), lim);
    let a11 = at(src, base + vec2<i32>(1, 1), lim);
    return mix(mix(a00, a10, fr.x), mix(a01, a11, fr.x), fr.y);
}

// The last pass: displace, tint, and knock the layer's own coverage out.
//
// The knockout is the outer effect's whole confinement and it is baked, not
// composited — see the file header. `1 - coverage` rather than anything cleverer
// because that is what the design says in as many words, and because at a
// coverage of 1 it is exactly zero, which is what "a layer at 50% opacity shows
// no shadow inside its own shape" needs.
@fragment
fn fs_resolve(@builtin(position) f: vec4<f32>) -> @location(0) vec4<f32> {
    var a = bilinear(f.xy - c.offset);
    if (c.inner == 0u) {
        a = a * (1.0 - coverage_at(vec2<i32>(f.xy)));
    }
    return c.tint * a;
}
