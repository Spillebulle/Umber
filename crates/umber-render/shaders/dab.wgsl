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
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: DabUniforms;
// The brush tip, R8Unorm coverage. One tip per stroke, bound for the whole dab
// pass, so N dabs are still a single draw call.
@group(0) @binding(1) var tip: texture_2d<f32>;
@group(0) @binding(2) var tip_sampler: sampler;

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
    return out;
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

    // The tip *modulates coverage*; it does not composite. The blend state is
    // still `max`, so a tipped stroke saturates at 1.0 under overlap exactly as
    // a round one does — see the wet-layer section of CLAUDE.md.
    return select(round, masked, u.use_tip != 0u) * in.coverage;
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

// The smudging path: coverage as above, plus the colour this particular dab
// deposits.
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
    out.color = vec4<f32>(in.color * cov, cov);
    return out;
}
