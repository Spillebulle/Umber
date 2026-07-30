// Stamps brush dabs into the stroke scratch texture (R8Unorm coverage).
//
// Every dab in a frame is one instance of a 4-vertex triangle strip, so a
// thousand dabs cost a single draw call. Blending is configured as `max` on the
// Rust side, which is what stops overlapping dabs within one stroke from
// compounding into a darker, blotchy line.

struct DabUniforms {
    doc_size: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> u: DabUniforms;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) radius: f32,
    @location(2) hardness: f32,
    @location(3) coverage: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    // Position within the dab, -1..1 on each axis.
    @location(0) local: vec2<f32>,
    @location(1) hardness: f32,
    @location(2) coverage: f32,
    @location(3) radius: f32,
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
    let doc = inst.pos + corner * inst.radius;

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
    out.radius = inst.radius;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    let d = length(in.local);

    // Keep at least one pixel of falloff regardless of hardness, otherwise
    // small brushes alias badly. `aa` is one pixel expressed in local units.
    let aa = clamp(1.0 / max(in.radius, 1.0), 0.001, 0.5);
    let inner = clamp(in.hardness, 0.0, 1.0 - aa);
    let cov = 1.0 - smoothstep(inner, 1.0, d);

    return vec4<f32>(cov * in.coverage, 0.0, 0.0, 1.0);
}
