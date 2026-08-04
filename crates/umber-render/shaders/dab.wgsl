// Stamps brush dabs into the stroke scratch texture (R8Unorm coverage).
//
// Every dab in a frame is one instance of a 4-vertex triangle strip, so a
// thousand dabs cost a single draw call. Blending is configured as `max` on the
// Rust side, which is what stops overlapping dabs within one stroke from
// compounding into a darker, blotchy line.

struct DabUniforms {
    doc_size: vec2<f32>,
    // The tip's proportions, its longer side normalised to 1: a 512x256 stamp
    // gives (1.0, 0.5). (1.0, 1.0) with no tip bound, which is the exact
    // identity — every multiplication by it is by one.
    //
    // This is what keeps a non-square stamp from being squashed, and it lives
    // here rather than on the dab because a stroke has one tip: the mask is
    // bound for the whole pass, so its shape is a property of the pass.
    tip_scale: vec2<f32>,
    // Non-zero when `tip` holds a real bitmap mask rather than the 1x1 white
    // placeholder. A scalar, not a vec3 pad: WGSL aligns vec3 to 16 bytes and
    // the Rust struct would come out short.
    use_tip: u32,
    // How hard the paper bites, 0..1. Zero is the exact identity: coverage is
    // multiplied by `mix(1.0, grain, strength)`, which at zero is a multiply by
    // one and nothing else.
    grain_strength: f32,
    // Side of one tile of `grain`, in **document** pixels. Document rather than
    // dab, because paper does not move when the brush does — that is the whole
    // effect. A second pass over the same stretch lands in the same pits.
    grain_scale: f32,
    _pad: f32,
    // The selection's bounding rectangle, in document pixels: where the
    // `selection` texture is mapped to. The mask covers only its own bounds
    // rather than the whole canvas, because a lasso round one eye should not
    // cost a texture the size of the portrait.
    //
    // `sel_size` is (1, 1) with no selection, so the divide below is never by
    // zero even though the result is discarded.
    sel_min: vec2<f32>,
    sel_size: vec2<f32>,
    // Non-zero when `selection` holds a real mask. Unlike the tip and the
    // paper, a placeholder cannot stand in for "no selection": a 1x1 texture
    // sampled outside its own rectangle is zero, which would mean *nothing*
    // may be painted. Hence a flag, read through a `select` rather than a
    // branch — see `selection_mask`.
    use_selection: u32,
    // Non-zero when `tip_color` holds a real coloured stamp rather than the 1x1
    // placeholder. Read by `fs_colored` alone — the ordinary fragment shader
    // never looks at it, and never samples the texture either, which is what
    // keeps a stroke with no coloured tip paying exactly nothing for this.
    //
    // Took the place of one of the three padding words already here, so the
    // block is the size it always was. A scalar, not a vec3: see the
    // uniform-layout note in CLAUDE.md.
    use_tip_color: u32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<uniform> u: DabUniforms;
// The brush tip, R8Unorm coverage. One tip per stroke, bound for the whole dab
// pass, so N dabs are still a single draw call.
@group(0) @binding(1) var tip: texture_2d<f32>;
@group(0) @binding(2) var tip_sampler: sampler;
// Paper grain, R8Unorm, tiling. A 1x1 white placeholder when the brush asks for
// none, exactly as the tip has — so the bind group layout never varies and
// there is still one set of dab pipelines.
@group(0) @binding(3) var grain: texture_2d<f32>;
// Its own sampler, because this one **repeats** where the tip's clamps. A tip
// stretched to its dab must not wrap; a paper tile must.
@group(0) @binding(4) var grain_sampler: sampler;
// The selection mask, R8Unorm coverage over `sel_min`..`sel_min + sel_size`.
// A 1x1 placeholder when nothing is selected, in which case `use_selection` is
// zero and it is never read. Sampled through `tip_sampler`, which clamps: a
// mask must not wrap, for the same reason a tip must not.
@group(0) @binding(5) var selection: texture_2d<f32>;
// A **coloured stamp**: the tip's own colour, `Rgba8UnormSrgb`, premultiplied
// in linear light with the coverage in the alpha — the layer array's convention,
// and for its reason. The hardware decodes the RGB on the way out, so a
// bilinear tap lands on filtered *premultiplied linear* values, which is the
// only form that does not halo at the edge of a stamp.
//
// A 1x1 placeholder unless the tip carries a colour. Sampled through
// `tip_sampler` beside the coverage, at the same uv, so the two cannot land on
// different texels.
@group(0) @binding(6) var tip_color: texture_2d<f32>;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) radius: f32,
    @location(2) hardness: f32,
    @location(3) coverage: f32,
    // Linear RGB this dab deposits. Read only by `fs_colored`; an ordinary
    // stroke leaves it equal to the stroke colour and never looks at it.
    @location(4) color: vec3<f32>,
    @location(5) aspect: f32,
    @location(6) angle: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Position within the dab, -1..1 on each axis — already in the dab's own
    // frame, so an ellipse is a unit circle here and the fragment shader does
    // not need to know the shape at all.
    @location(0) local: vec2<f32>,
    @location(1) hardness: f32,
    @location(2) coverage: f32,
    // The *short* semi-axis, in document pixels. Only used to size the
    // antialiasing margin, and the short axis is the demanding one: a chisel
    // two pixels across needs the same softening a two-pixel round brush does.
    @location(3) radius: f32,
    @location(4) color: vec3<f32>,
    // Document position, for the grain. It has to be interpolated rather than
    // derived from `local`, because the grain is anchored to the paper and
    // `local` is anchored to the dab.
    @location(5) doc: vec2<f32>,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 4>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>( 1.0, -1.0),
        vec2<f32>(-1.0,  1.0),
        vec2<f32>( 1.0,  1.0),
    );
    let corner = corners[vi];

    // Build the quad as the dab's own bounding box, squashed across the long
    // axis and then rotated into place. Two consequences worth keeping:
    //
    //   - `corner` stays the fragment's position in the dab's own frame, so
    //     `length(local) <= 1` is still "inside", and the falloff below is
    //     unchanged from when every dab was a circle.
    //   - a thin chisel rasterises a thin quad rather than the square that
    //     would contain it, so a 20:1 brush does not shade twenty times the
    //     fragments it covers.
    //
    // `tip_scale` narrows whichever axis the mask is shorter on, so a 512x256
    // stamp occupies a 2:1 box and keeps its proportions. It is (1, 1) for a
    // round brush, so this line is what it always was. Note the falloff is then
    // computed in a distorted frame — which does not matter, because a
    // `tip_scale` other than (1, 1) means a tip is bound and the falloff is
    // discarded.
    let short = inst.radius / max(inst.aspect, 1.0);
    let scaled = vec2<f32>(
        corner.x * inst.radius * u.tip_scale.x,
        corner.y * short * u.tip_scale.y,
    );
    let ca = cos(inst.angle);
    let sa = sin(inst.angle);
    let rotated = vec2<f32>(
        scaled.x * ca - scaled.y * sa,
        scaled.x * sa + scaled.y * ca,
    );
    let doc = inst.pos + rotated;

    // Document space is y-down with the origin top-left; clip space is y-up.
    let ndc = vec2<f32>(
        doc.x / u.doc_size.x * 2.0 - 1.0,
        1.0 - doc.y / u.doc_size.y * 2.0,
    );

    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.local = corner;
    out.hardness = inst.hardness;
    out.coverage = inst.coverage;
    // The shorter of the quad's two semi-axes, whichever it turns out to be:
    // a landscape tip on a round dab is narrow across y, a portrait one across
    // x. With no tip this is `short` exactly, since `aspect` is at least 1.
    out.radius = min(inst.radius * u.tip_scale.x, short * u.tip_scale.y);
    out.color = inst.color;
    out.doc = doc;
    return out;
}

// How much of this fragment the selection lets through, 1.0 everywhere when
// nothing is selected.
//
// **This is the only place a selection clips painting**, and that is the whole
// design. The scratch texture then holds coverage that is already clipped, so
// `composite.wgsl` and `commit.wgsl` are untouched — which matters because
// those two implement the same blending maths and a stroke that clipped
// differently in one of them would visibly jump at pointer-up. Clipping the
// coverage on the way in cannot produce that bug, because there is one copy of
// it.
//
// Sampled unconditionally, like the tip and the paper: `textureSample` may not
// appear in non-uniform control flow, and the placeholder read costs a cache
// hit. With `use_selection` at zero this returns exactly 1.0, so a document
// with no selection pays one multiply by one.
fn selection_mask(doc: vec2<f32>) -> f32 {
    let suv = (doc - u.sel_min) / u.sel_size;
    let m = textureSample(selection, tip_sampler, suv).r;
    // Outside the mask's own rectangle is outside the selection. Clamping
    // would smear the boundary texels across the rest of the canvas instead,
    // which for a rectangle selection means the whole row and column beyond it
    // stay paintable.
    let inside = all(suv >= vec2<f32>(0.0)) && all(suv <= vec2<f32>(1.0));
    return select(1.0, select(0.0, m, inside), u.use_selection != 0u);
}

// Coverage of one dab at this fragment, before the stroke's own opacity.
//
// Shared by both fragment entry points so the two pipelines cannot drift into
// stamping different shapes — the tipped/round choice and the antialiasing
// margin are exactly the sort of thing that gets fixed in one copy only.
fn dab_coverage(in: VsOut) -> f32 {
    // Sampled unconditionally rather than inside the `use_tip` branch.
    // `textureSample` may not appear in non-uniform control flow, and hoisting
    // it out is cheaper than arguing with the uniformity analysis about a flag
    // that happens to come from a uniform buffer. With no tip bound this reads
    // a 1x1 white texture, which is a cache hit and nothing else.
    //
    // `local` runs -1..1 across the quad, and the quad has already been given
    // the tip's proportions in the vertex shader, so the mask lands unsquashed
    // and fills its whole quad — no padding, no empty margin to shade.
    let uv = in.local * 0.5 + vec2<f32>(0.5, 0.5);
    let masked = textureSample(tip, tip_sampler, uv).r;

    let d = length(in.local);

    // Keep at least one pixel of falloff regardless of hardness, otherwise
    // small brushes alias badly. `aa` is one pixel expressed in local units.
    let aa = clamp(1.0 / max(in.radius, 1.0), 0.001, 0.5);
    let inner = clamp(in.hardness, 0.0, 1.0 - aa);
    let round = 1.0 - smoothstep(inner, 1.0, d);

    // Paper. Sampled unconditionally for the same uniformity reason the tip is,
    // and with the same 1x1 white placeholder standing in when there is none.
    // `mix(1.0, g, 0.0)` is exactly 1.0, so a brush with no grain pays one
    // multiply by one — `grain_off_is_the_exact_identity` pins that.
    let tile = textureSample(grain, grain_sampler, in.doc / max(u.grain_scale, 1.0)).r;
    let paper = mix(1.0, tile, u.grain_strength);

    // The tip *modulates coverage*; it does not composite. The blend state is
    // unchanged by either the tip or the paper, so a tipped, grained stroke
    // saturates at 1.0 under overlap exactly as a plain one does — unless the
    // brush asked to build up, which is a blend choice and not this one. See
    // the wet-layer section of CLAUDE.md.
    //
    // The selection multiplies last, for the same reason the paper does: it
    // modulates coverage rather than compositing, so an eraser inside a
    // selection erases exactly what a brush inside it would paint, and a
    // building-up stroke accumulates the clipped coverage rather than
    // accumulating and then being clipped once.
    return select(round, masked, u.use_tip != 0u) * in.coverage * paper * selection_mask(in.doc);
}

// The ordinary path: coverage only, one attachment, one colour for the whole
// stroke applied later at composite and commit.
@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(dab_coverage(in), 0.0, 0.0, 1.0);
}

struct ColoredOut {
    @location(0) coverage: vec4<f32>,
    @location(1) color: vec4<f32>,
};

// The colour this fragment deposits, in **straight linear RGB**.
//
// Three things can put a colour on a dab, and this is where the third joins the
// other two. `in.color` already carries whatever the stroke worked out per dab
// — the palette colour, what a smudging brush picked up, or a colour
// modulation's shift of it. A **coloured stamp** overrides that, per *texel*:
// the whole point of a leaf or a two-hue spatter is that the picture in the
// file is the mark, so the palette has nothing to say about it. That is also
// how GIMP's pixmap brushes and Krita's colour stamps behave.
//
// The stored colour is premultiplied, because that is the only form that
// survives bilinear filtering at the edge of a stamp. Un-premultiplying here
// rather than folding the multiply into `cov` keeps `dab_coverage` byte for
// byte what it was — the coverage a coloured stamp writes is the same number an
// uncoloured one would, so the `max`, the paper, the selection clip and the
// build-up blend are all untouched. The guard is `stroke_rgb`'s in
// `composite.wgsl` and for the same reason: where nothing was stamped the
// sample is all zeroes and the divide would be a NaN.
fn dab_rgb(in: VsOut) -> vec3<f32> {
    let uv = in.local * 0.5 + vec2<f32>(0.5, 0.5);
    let stamped = textureSample(tip_color, tip_sampler, uv);
    let own = select(in.color, stamped.rgb / max(stamped.a, 1e-4), stamped.a > 1e-4);
    return select(in.color, own, u.use_tip_color != 0u);
}

// The per-dab colour path: coverage as above, plus the colour this particular
// dab deposits — picked up off the canvas, shifted by a modulation, or the
// stamp's own.
//
// The two attachments blend *differently*, which is the whole point. Coverage
// still takes a `max`, so a smudging stroke crossing itself is no more opaque
// than one that does not — the wet-layer guarantee is untouched. Colour is
// premultiplied `over`, so it tends towards the most recent dabs and a smear
// trails along the stroke the way paint does.
//
// A `max` on colour would be meaningless (it would take the brightest channel
// wherever the stroke overlapped) and an average over the whole stroke would
// smear the first colour picked up all the way to the end.
@fragment
fn fs_colored(in: VsOut) -> ColoredOut {
    let cov = dab_coverage(in);
    var out: ColoredOut;
    out.coverage = vec4<f32>(cov, 0.0, 0.0, 1.0);
    out.color = vec4<f32>(dab_rgb(in) * cov, cov);
    return out;
}
